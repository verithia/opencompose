//! Bounded summary input and publication. Previous summary prose has its own
//! allocation, while newer dropped turns take precedence over older output.

use super::*;

pub(super) const MAX_SUMMARY_BYTES: usize = 32_768;
pub(crate) const SUMMARY_OUTPUT_TOKENS: u64 = 4_096;
const MAX_INPUT_BYTES: usize = 80_000;
const MAX_TURN_BYTES: usize = 4_000;
const OMITTED: &str = "Earlier dropped turns are omitted to keep this input bounded.\n";

/// The previous summary's prose, without recursively nesting its generated
/// project header and artifact inventory. Legacy summaries use their content.
pub(super) fn previous_summary(view: &[Value]) -> Option<String> {
    let row = view.iter().rev().find(|row| is_compaction_summary(row))?;
    let meta = message_protocol::read(row);
    let text = meta
        .as_ref()
        .and_then(|meta| meta.summary_text.as_deref())
        .or_else(|| row["content"].as_str())?;
    Some(bounded_text(text, MAX_SUMMARY_BYTES))
}

fn bounded_text(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut end = bytes.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub(super) fn compaction_input(view: &[Value], split: usize) -> String {
    let old = &view[..split];
    let mut text = previous_summary(old)
        .map(|summary| format!("Previous conversation summary:\n{summary}\n\n"))
        .unwrap_or_default();
    let mut remaining = MAX_INPUT_BYTES.saturating_sub(text.len() + OMITTED.len());
    let mut selected = Vec::new();
    let mut kept = std::collections::HashSet::new();
    let mut omitted = false;
    // Explicit instructions must not be crowded out by the tool output that
    // followed them. The remaining space still holds recent execution context.
    let mut instructions = remaining.min(16_000);
    for (index, message) in old.iter().enumerate().rev() {
        if message_protocol::kind(message) != Some(MessageKind::User) {
            continue;
        }
        let Some(line) = input_line(view, index, message) else {
            continue;
        };
        if line.len() <= instructions {
            instructions -= line.len();
            remaining -= line.len();
            selected.push((index, line));
            kept.insert(index);
        }
    }
    for (index, message) in old.iter().enumerate().rev() {
        if is_compaction_summary(message) || kept.contains(&index) {
            continue;
        }
        let Some(line) = input_line(view, index, message) else {
            continue;
        };
        if line.len() > remaining {
            omitted = true;
            continue;
        }
        remaining -= line.len();
        selected.push((index, line));
    }
    if omitted {
        text.push_str(OMITTED);
    }
    selected.sort_by_key(|(index, _)| *index);
    for (_, line) in selected {
        text.push_str(&line);
    }
    text
}

fn input_line(view: &[Value], index: usize, message: &Value) -> Option<String> {
    let line = match message["role"].as_str() {
        Some("user") => {
            let plain = plain_message(message)?;
            let content = collapse(plain["content"].as_str().unwrap_or_default());
            if content.is_empty() {
                return None;
            }
            format!("User: {content}")
        }
        Some("assistant") => {
            let mut line = String::new();
            if let Some(content) = message["content"]
                .as_str()
                .filter(|content| !content.trim().is_empty())
            {
                line.push_str(&format!("Assistant: {}\n", collapse(content)));
            }
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                line.push_str(&format!(
                    "Assistant called {} with {}\n",
                    call["function"]["name"].as_str().unwrap_or("tool"),
                    bounded_text(
                        &collapse(call["function"]["arguments"].as_str().unwrap_or_default()),
                        MAX_TURN_BYTES,
                    )
                ));
            }
            line
        }
        Some("tool") => format!(
            "Tool {} result: {}",
            tool_name_of(view, index),
            collapse(message["content"].as_str().unwrap_or_default())
        ),
        _ => return None,
    };
    if line.trim().is_empty() {
        return None;
    }
    Some(format!(
        "{}\n",
        bounded_text(line.trim_end(), MAX_TURN_BYTES - 1)
    ))
}

/// Only a complete, bounded answer that improves the plan may replace the
/// mechanical fallback. Its typed prose survives a later mechanical summary.
pub(super) fn planned_messages(plan: &CompactionPlan, summary: Option<&str>) -> Vec<Value> {
    let fallback = || plan.fallback.clone();
    let Some(summary) = summary.map(str::trim).filter(|summary| !summary.is_empty()) else {
        return fallback();
    };
    if summary.len() > MAX_SUMMARY_BYTES || estimate_text_tokens(summary) > SUMMARY_OUTPUT_TOKENS {
        return fallback();
    }
    let mut messages = fallback();
    let Some(row) = messages.first_mut() else {
        return messages;
    };
    row["content"] = json!(format!("{}{summary}\n\n{}", plan.prefix, plan.digest));
    let mut meta = message_protocol::read(row).expect("a planned summary has typed metadata");
    meta.summary_text = Some(summary.to_owned());
    message_protocol::tag(row, meta);
    let after = estimate_tokens(&messages);
    if after >= plan.tokens_before || (plan.target_tokens > 0 && after > plan.target_tokens) {
        return fallback();
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_input_keeps_the_prior_summary_and_the_newest_correction() {
        let mut summary =
            json!({"role":"user","context":CONTEXT_SUMMARY,"content":"keep vocals dry"});
        let mut meta = MessageMeta::new(MessageKind::Summary);
        meta.discarded_auditions = Some(0);
        message_protocol::tag(&mut summary, meta);
        let mut view = vec![summary];
        view.push(json!({"role":"user","content":"reject the metallic bass patch"}));
        for _ in 0..40 {
            view.push(json!({"role":"assistant","content":"old output ".repeat(1000)}));
        }
        let input = compaction_input(&view, view.len());
        assert!(input.contains("keep vocals dry"));
        assert!(input.contains("reject the metallic bass patch"));
        assert!(input.contains(OMITTED.trim()));
        assert!(input.len() <= MAX_INPUT_BYTES);
        assert!(
            input.find("Previous conversation summary").unwrap() < input.find("User:").unwrap()
        );
    }

    #[test]
    fn unicode_and_parallel_call_rows_stay_within_the_byte_budget() {
        let view = vec![json!({"role":"assistant","content":"声".repeat(80_000),
            "tool_calls": (0..32).map(|_| json!({"function":{"name":"read","arguments":"声".repeat(10_000)}})).collect::<Vec<_>>()})];
        let input = compaction_input(&view, 1);
        assert!(input.len() <= MAX_INPUT_BYTES);
        assert!(input.is_char_boundary(input.len()));
    }
}
