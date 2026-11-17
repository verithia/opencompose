//! Bounded summary input and publication. Previous summary prose has its own
//! allocation, while newer dropped turns take precedence over older output.

use super::*;

pub(super) const MAX_SUMMARY_BYTES: usize = 32_768;
pub(crate) const SUMMARY_OUTPUT_TOKENS: u64 = 4_096;
const MAX_INPUT_BYTES: usize = 80_000;
const MAX_TURN_BYTES: usize = 4_000;
const OMITTED: &str = "Earlier dropped turns are omitted to keep this input bounded.\n";

/// The previous summary's prose, without its generated project header, tool
/// digest or artifact inventory. Legacy summaries keep those sections inside
/// their content, so the prose is recovered from the content itself.
pub(super) fn previous_summary(view: &[Value]) -> Option<String> {
    let row = view.iter().rev().find(|row| is_compaction_summary(row))?;
    let meta = message_protocol::read(row);
    let text = match meta.as_ref().and_then(|meta| meta.summary_text.as_deref()) {
        Some(prose) => prose,
        None => legacy_summary_prose(row["content"].as_str()?),
    };
    Some(bounded_text(text, MAX_SUMMARY_BYTES, KeepEnd::Newest))
}

/// The prose inside a legacy mechanical row, without the generated sections.
/// Its decisions sit before the digest and inventory, so a later bound keeps
/// them instead of the generated tail.
fn legacy_summary_prose(content: &str) -> &str {
    let content = artifacts::strip_inventory(content);
    let content = content
        .split(SUMMARY_DIGEST_PREFIX)
        .next()
        .unwrap_or(content);
    for marker in ["Previous conversation summary:\n", "Decisions taken:\n"] {
        if let Some(index) = content.find(marker) {
            return &content[index + marker.len()..];
        }
    }
    content
}

/// Which end of a string a bound keeps.
pub(super) enum KeepEnd {
    Oldest,
    Newest,
}

pub(super) fn bounded_text(text: &str, bytes: usize, keep: KeepEnd) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let available = bytes.saturating_sub('…'.len_utf8());
    match keep {
        KeepEnd::Oldest => {
            let mut end = available;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &text[..end])
        }
        KeepEnd::Newest => {
            let mut start = text.len() - available;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            format!("…{}", &text[start..])
        }
    }
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
                        KeepEnd::Oldest,
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
        bounded_text(line.trim_end(), MAX_TURN_BYTES - 1, KeepEnd::Oldest)
    ))
}

/// The messages a commit writes, and why the model text was replaced when it
/// was not used.
pub(super) struct PlannedSummary {
    pub messages: Vec<Value>,
    pub failure: Option<CompactionFailure>,
}

/// Only a complete, bounded answer that improves the plan may replace the
/// mechanical fallback. Its typed prose survives a later mechanical summary.
/// A summary above the target is still an improvement when the fallback is
/// larger: the target is a goal, not a reason to keep a bigger summary.
pub(super) fn planned_messages(plan: &CompactionPlan, summary: Option<&str>) -> PlannedSummary {
    let rejected = || PlannedSummary {
        messages: plan.fallback.clone(),
        failure: Some(CompactionFailure::Rejected),
    };
    let fallback = || PlannedSummary {
        messages: plan.fallback.clone(),
        failure: None,
    };
    let Some(summary) = summary.map(str::trim).filter(|summary| !summary.is_empty()) else {
        return fallback();
    };
    if summary.len() > MAX_SUMMARY_BYTES || estimate_text_tokens(summary) > SUMMARY_OUTPUT_TOKENS {
        return rejected();
    }
    let fallback_tokens = estimate_tokens(&plan.fallback);
    let mut messages = plan.fallback.clone();
    let Some(row) = messages.first_mut() else {
        return fallback();
    };
    row["content"] = json!(format!("{}{summary}\n\n{}", plan.prefix, plan.digest));
    let mut meta = message_protocol::read(row).expect("a planned summary has typed metadata");
    meta.summary_text = Some(summary.to_owned());
    message_protocol::tag(row, meta);
    let after = estimate_tokens(&messages);
    let improves = after < plan.tokens_before
        && (plan.target_tokens == 0 || after <= plan.target_tokens || after < fallback_tokens);
    if !improves {
        return rejected();
    }
    PlannedSummary {
        messages,
        failure: None,
    }
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

pub(super) struct PlanInputs<'a> {
    pub(super) overhead: u64,
    pub(super) context_length: u64,
    pub(super) max_output: u64,
    pub(super) prompt_tokens: u64,
    pub(super) facts: &'a ProjectFacts,
    pub(super) id: &'a str,
}

pub(super) fn plan_compaction(
    view: &[Value],
    target: u64,
    facts: &ProjectFacts,
    id: &str,
) -> Result<Compaction, String> {
    if view.len() < 3 {
        return Err("This session has nothing older to compact.".into());
    }
    let before = estimate_tokens(view);
    let blocks = tool_blocks(view);
    let at = now();
    let mut keep = RETAINED_TOOL_PAIRS.min(blocks.len());
    loop {
        let retained: &[(usize, usize)] = if keep == 0 {
            &[]
        } else {
            &blocks[blocks.len() - keep..]
        };
        let split = match retained.first() {
            Some((start, _)) => *start,
            None => view.len().saturating_sub(2).max(1),
        };
        if split == 0 {
            return Err("This session has nothing older to compact.".into());
        }
        let summary = summary_body(view, split, retained, facts, before, at.as_str());
        let mut messages = Vec::with_capacity(1 + retained.len() + view.len() - split);
        let mut summary_row =
            json!({ "role": "user", "content": summary.full, "context": CONTEXT_SUMMARY });
        let mut metadata = summary_metadata(view, split, retained);
        // The prose survives without the generated header, digest or inventory,
        // so a later mechanical summary carries decisions rather than nesting
        // the whole row.
        metadata.summary_text = Some(summary.carried);
        message_protocol::tag(&mut summary_row, metadata);
        messages.push(summary_row);
        // The bound project stays beside the summary: every later path in the
        // conversation is relative to it.
        messages.extend(newest_pinned_rows(view).into_iter().map(|(_, row)| row));
        // Preserve the latest labeled pair in a retained round. For other
        // auditions use a single row to keep replay bounded.
        for (start, end) in retained {
            if let Some(pair) = retained_comparison_pair(&view[*start..*end]) {
                messages.extend(pair.iter().map(|row| {
                    let mut kept = json!({
                        "role": "user",
                        "context": CONTEXT_AUDITION,
                        "content": clip(row["content"].as_str().unwrap_or_default(), 512),
                        "media": canonical_media(&row["media"]),
                    });
                    if let Some(meta) = message_protocol::legacy(row) {
                        message_protocol::tag(&mut kept, meta);
                    }
                    kept
                }));
            } else if let Some(row) = retained_media_row(view, *start, *end) {
                messages.push(row);
            }
        }
        messages.extend(tail_messages(view, split, retained));
        // Keep a bounded set of reference paths when their prose is folded.
        let mut paths: std::collections::HashSet<String> = messages
            .iter()
            .filter_map(|message| message["media"].as_array())
            .flatten()
            .filter_map(|entry| entry["path"].as_str().map(str::to_owned))
            .collect();
        let local_references: Vec<Value> = view
            .iter()
            .rev()
            .filter_map(|message| message["media"].as_array())
            .flat_map(|entries| entries.iter().rev())
            .filter(|entry| entry["local_reference"] == true)
            .filter(|entry| {
                entry["path"]
                    .as_str()
                    .is_some_and(|path| paths.insert(path.to_owned()))
            })
            .take(super::super::media::MAX_ATTACHMENTS_PER_MESSAGE)
            .cloned()
            .collect();
        if !local_references.is_empty() {
            messages[0]["media"] = json!(local_references);
        }
        if !strips_replay_state(&messages) {
            return Err("Compaction produced a message that still replays provider state.".into());
        }
        let after = estimate_tokens(&messages);
        if after < before && (target == 0 || after <= target) {
            return Ok(Compaction {
                id: id.to_owned(),
                compacted_at: at,
                messages,
                dropped_from: 0,
                dropped_to: split,
                tokens_before: before,
                tokens_after: after,
                retained_pairs: keep,
            });
        }
        if keep > 0 {
            keep -= 1;
            continue;
        }
        if after >= before {
            return Err("Compaction would not reduce this conversation.".into());
        }
        // Reduced as far as the retained pairs allow but still above the target:
        // a smaller summary is not available without losing the standing intent.
        return Ok(Compaction {
            id: id.to_owned(),
            compacted_at: at,
            messages,
            dropped_from: 0,
            dropped_to: split,
            tokens_before: before,
            tokens_after: after,
            retained_pairs: 0,
        });
    }
}

pub(super) fn plan_compaction_request(
    view: &[Value],
    target: u64,
    facts: &ProjectFacts,
    id: &str,
) -> Result<CompactionPlan, String> {
    let outcome = plan_compaction(view, target, facts, id)?;
    let blocks = tool_blocks(view);
    let keep = outcome.retained_pairs.min(blocks.len());
    let retained: &[(usize, usize)] = if keep == 0 {
        &[]
    } else {
        &blocks[blocks.len() - keep..]
    };
    let split = outcome.dropped_to;
    Ok(CompactionPlan {
        id: id.to_owned(),
        view_digest: view_digest(view),
        dropped_from: outcome.dropped_from,
        dropped_to: split,
        retained_pairs: outcome.retained_pairs,
        tokens_before: outcome.tokens_before,
        target_tokens: target,
        compacted_at: outcome.compacted_at.clone(),
        input: compaction_input(view, split),
        prefix: summary_intro(view, split, retained, facts, &outcome.compacted_at),
        digest: summary_digest(view, split, retained, outcome.tokens_before),
        fallback: outcome.messages,
        facts: facts.clone(),
    })
}

pub(super) fn view_digest(view: &[Value]) -> String {
    let mut hasher = Sha256::new();
    for message in view {
        hasher.update(message.to_string().as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tail_messages(view: &[Value], split: usize, retained: &[(usize, usize)]) -> Vec<Value> {
    let in_block = |index: usize| {
        retained
            .iter()
            .any(|(start, end)| index >= *start && index < *end)
    };
    let mut tail: Vec<Value> = (split..view.len())
        .filter(|index| !in_block(*index))
        .filter_map(|index| plain_message(&view[index]))
        .collect();
    let pending = view
        .iter()
        .rposition(|message| message["role"] == "user" && plain_message(message).is_some());
    if let Some(index) = pending {
        let message = plain_message(&view[index]).expect("checked above");
        if !retained
            .iter()
            .any(|(start, end)| index >= *start && index < *end)
            && !tail.contains(&message)
        {
            tail.push(message);
        }
    }
    tail
}

fn block_media(view: &[Value], start: usize, end: usize) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for message in &view[start..end] {
        let Some(media) = message.get("media").and_then(Value::as_array) else {
            continue;
        };
        for entry in canonical_media(&json!(media))
            .as_array()
            .into_iter()
            .flatten()
        {
            if !entries.contains(entry) {
                entries.push(entry.clone());
            }
        }
    }
    entries
}

pub(super) fn retained_comparison_pair(rows: &[Value]) -> Option<&[Value]> {
    rows.windows(2).rev().find(|pair| {
        pair.iter()
            .zip([
                MessageKind::ComparisonReference,
                MessageKind::ComparisonCandidate,
            ])
            .all(|(row, expected)| {
                message_protocol::kind(row) == Some(expected)
                    && row["media"]
                        .as_array()
                        .is_some_and(|media| !media.is_empty())
            })
    })
}

pub(super) fn retained_media_row(view: &[Value], start: usize, end: usize) -> Option<Value> {
    let entries = block_media(view, start, end);
    if entries.is_empty() {
        return None;
    }
    let latest = view[start..end]
        .iter()
        .rev()
        .find(|message| audition_render_reference(message).is_some())
        .or_else(|| {
            view[start..end]
                .iter()
                .find(|message| message.get("media").is_some())
        });
    let label = latest
        .and_then(|message| message["content"].as_str())
        .unwrap_or("Saved audio attachment");
    let mut row = json!({
        "role": "user",
        "content": clip(label, 512),
        "context": CONTEXT_AUDITION,
        "media": entries,
    });
    let mut meta = latest
        .and_then(message_protocol::legacy)
        .unwrap_or_else(|| MessageMeta::new(MessageKind::Audition));
    if !message_protocol::is_audition(meta.kind) {
        meta = MessageMeta::new(MessageKind::Audition);
    }
    message_protocol::tag(&mut row, meta);
    if let Some(path) = latest.and_then(audition_render_reference) {
        row["source_render"] = json!(path);
    }
    Some(row)
}

fn retained_media(view: &[Value], retained: &[(usize, usize)]) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for (start, end) in retained {
        let block = &view[*start..*end];
        let kept = retained_comparison_pair(block).unwrap_or(block);
        for entry in block_media(kept, 0, kept.len()) {
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
    }
    entries
}

fn dropped_audition_count(view: &[Value], retained: &[(usize, usize)]) -> usize {
    let in_retained = |index: usize| {
        retained
            .iter()
            .any(|(start, end)| index >= *start && index < *end)
    };
    let rows = view
        .iter()
        .enumerate()
        .filter(|(index, message)| !in_retained(*index) && is_audition(message))
        .count();
    let previous = view
        .iter()
        .filter_map(message_protocol::legacy)
        .filter(|meta| meta.kind == MessageKind::Summary)
        .find_map(|meta| meta.discarded_auditions)
        .unwrap_or(0);
    rows + previous
}

pub(super) fn summary_metadata(view: &[Value], split: usize, retained: &[(usize, usize)]) -> MessageMeta {
    let old = &view[..split];
    let mut metadata = MessageMeta::new(MessageKind::Summary);
    metadata.standing_intent = old
        .iter()
        .filter_map(message_protocol::legacy)
        .filter(|meta| meta.kind == MessageKind::Summary)
        .find_map(|meta| meta.standing_intent)
        .or_else(|| {
            old.iter().find_map(|message| {
                if message_protocol::kind(message) != Some(MessageKind::User) {
                    return None;
                }
                Some(clip(
                    &collapse(message["content"].as_str()?),
                    MAX_INTENT_CHARS,
                ))
            })
        });
    metadata.discarded_auditions = Some(dropped_audition_count(view, retained));
    for message in old.iter().rev() {
        let Some(meta) = message_protocol::legacy(message) else {
            continue;
        };
        if metadata.rollback_notice.is_none() {
            metadata.rollback_notice = if meta.kind == MessageKind::TransactionRollback {
                message["content"].as_str().map(|text| clip(text, 4096))
            } else {
                meta.rollback_notice
            };
        }
        if metadata.proposal_notice.is_none() {
            metadata.proposal_notice = if meta.kind == MessageKind::ProposalOutcome {
                message["content"].as_str().map(|text| clip(text, 4096))
            } else {
                meta.proposal_notice
            };
        }
    }
    metadata
}

struct SummaryBody {
    full: String,
    carried: String,
}

fn summary_body(
    view: &[Value],
    split: usize,
    retained: &[(usize, usize)],
    facts: &ProjectFacts,
    tokens_before: u64,
    at: &str,
) -> SummaryBody {
    let prose = summary_prose(view, split);
    let mut full = summary_intro(view, split, retained, facts, at);
    full.push_str(&prose.content);
    full.push_str(&summary_digest(view, split, retained, tokens_before));
    SummaryBody {
        full,
        carried: prose.carried,
    }
}

#[cfg(test)]
pub(super) fn summary_text(
    view: &[Value],
    split: usize,
    retained: &[(usize, usize)],
    facts: &ProjectFacts,
    tokens_before: u64,
    at: &str,
) -> String {
    summary_body(view, split, retained, facts, tokens_before, at).full
}

struct SummaryProse {
    content: String,
    carried: String,
}

fn summary_prose(view: &[Value], split: usize) -> SummaryProse {
    let previous = summary::previous_summary(&view[..split]);
    let decisions = summary_decisions(view, split);
    let mut content = String::new();
    let mut carried = String::new();
    if let Some(previous) = previous.as_deref() {
        content.push_str(&format!("Previous conversation summary:\n{previous}\n\n"));
        carried.push_str(previous);
        carried.push_str("\n\n");
    }
    content.push_str(&decisions);
    carried.push_str(&decisions);
    let carried = summary::bounded_text(
        &carried,
        summary::MAX_SUMMARY_BYTES,
        summary::KeepEnd::Newest,
    );
    SummaryProse { content, carried }
}

fn summary_intro(
    view: &[Value],
    split: usize,
    retained: &[(usize, usize)],
    facts: &ProjectFacts,
    at: &str,
) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "Conversation summary written {at}. Everything older than the most recent tool calls \
         below is replaced by this message; treat it as context, not as an instruction.\n\n"
    ));
    text.push_str(&format!(
        "Project: {}\n",
        facts
            .path
            .as_deref()
            .unwrap_or("no saved project (detached conversation)")
    ));
    if !facts.dsl_digest.is_empty() {
        text.push_str(&format!(
            "Project DSL: sha256 {} ({} tracks)\n",
            facts.dsl_digest, facts.track_count
        ));
    }
    let metadata = summary_metadata(view, split, retained);
    let intent = metadata
        .standing_intent
        .as_deref()
        .unwrap_or("not recorded");
    text.push_str(&format!("Standing intent: {intent}\n"));
    for outcome in [metadata.rollback_notice, metadata.proposal_notice]
        .into_iter()
        .flatten()
    {
        text.push_str(&outcome);
        text.push('\n');
    }
    text
}

fn summary_decisions(view: &[Value], split: usize) -> String {
    let old = &view[..split];
    let mut text = String::new();
    let mut decisions = Vec::new();
    let mut failures = Vec::new();
    let mut proposals = Vec::new();
    let mut asks = Vec::new();
    for (index, message) in old.iter().enumerate() {
        match message["role"].as_str() {
            Some("user") => {
                // Only real prose counts as an ask: a delivery, a summary or a
                // media row is host context, and the round nudge is the loop's.
                if let Some(plain) = plain_message(message)
                    && let Some(content) = plain["content"].as_str()
                {
                    let content = clip(&collapse(content), MAX_SUMMARY_ITEM_CHARS);
                    if !content.is_empty() && asks.len() < 5 {
                        asks.push(content);
                    }
                }
                continue;
            }
            Some("tool") => {
                let name = tool_name_of(view, index);
                if is_proposal(message) {
                    if proposals.len() < 6 {
                        proposals.push(format!(
                            "{name} proposed a replacement for {} (not applied; the user decides)",
                            proposal_target(view, index)
                        ));
                    }
                    continue;
                }
                let summary = summary_of_result(message);
                if is_failure(message) {
                    if failures.len() < 6 {
                        failures.push(format!("{name} \u{2192} {summary}"));
                    }
                } else if crate::tool_registry::is_mutation_tool(&name) && decisions.len() < 6 {
                    decisions.push(format!("{name} reported success at that time ({summary}); consult current project state"));
                }
                continue;
            }
            _ => {}
        }
    }
    text.push_str("Decisions taken:\n");
    if decisions.is_empty() {
        text.push_str("- none recorded\n");
    }
    for decision in &decisions {
        text.push_str(&format!("- {decision}\n"));
    }
    if !proposals.is_empty() {
        text.push_str("Proposals awaiting the user:\n");
        for proposal in &proposals {
            text.push_str(&format!("- {proposal}\n"));
        }
    }
    text.push_str("What was tried and failed:\n");
    if failures.is_empty() {
        text.push_str("- nothing failed\n");
    }
    for failure in &failures {
        text.push_str(&format!("- {failure}\n"));
    }
    let open = old
        .iter()
        .rev()
        .find(|message| message["role"] == "assistant")
        .and_then(|message| message["content"].as_str())
        .map(|content| clip(&collapse(content), MAX_SUMMARY_ITEM_CHARS * 2))
        .unwrap_or_else(|| "none recorded".into());
    text.push_str(&format!("Open questions:\n- {open}\n"));
    if !asks.is_empty() {
        text.push_str("Earlier user asks:\n");
        for ask in &asks {
            text.push_str(&format!("- \"{ask}\"\n"));
        }
    }
    text
}

fn summary_digest(
    view: &[Value],
    split: usize,
    retained: &[(usize, usize)],
    tokens_before: u64,
) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "{SUMMARY_DIGEST_PREFIX}{}, oldest first):\n",
        retained.len()
    ));
    if retained.is_empty() {
        text.push_str("- none retained\n");
    }
    for (offset, (start, end)) in retained.iter().enumerate() {
        // Every call of a parallel batch, each with the result that answers it.
        let lines: Vec<String> = turns::calls_of(&view[*start])
            .into_iter()
            .map(|(id, name)| {
                let arguments = view[*start]["tool_calls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|call| call["id"].as_str() == Some(id.as_str()))
                    .and_then(|call| call["function"]["arguments"].as_str())
                    .map(|arguments| clip(&collapse(arguments), MAX_SUMMARY_ITEM_CHARS))
                    .unwrap_or_default();
                let result = view[*start + 1..*end]
                    .iter()
                    .find(|message| turns::answered_call(message).as_deref() == Some(id.as_str()))
                    .map(summary_of_result)
                    .unwrap_or_else(|| "no result recorded".into());
                format!("{name} \u{2192} {arguments} \u{2192} {result}")
            })
            .collect();
        text.push_str(&format!("{}. {}\n", offset + 1, lines.join("; ")));
    }
    let kept = retained_media(view, retained);
    if !kept.is_empty() {
        let newest = kept
            .last()
            .and_then(|entry| entry["name"].as_str())
            .unwrap_or("audio clip");
        text.push_str(&format!(
            "Audio kept on disk: {} clip(s), newest {newest}.\n",
            kept.len()
        ));
    }
    let dropped = dropped_audition_count(view, retained);
    if dropped > 0 {
        text.push_str(&format!(
            "Discarded audition deliveries: {dropped} clips (their files remain on disk; ask for a range again to listen).\n"
        ));
    }
    text.push_str(&format!(
        "Dropped messages 0..{split} of {total} (~{tokens_before} tokens before compaction); \
         this summary replaces them.\n",
        total = view.len()
    ));
    artifacts::append(&mut text, view);
    text
}

fn tool_name_of(view: &[Value], index: usize) -> String {
    let call_id = view[index]["tool_call_id"].as_str().unwrap_or_default();
    for message in view[..index].iter().rev() {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                if call["id"].as_str() == Some(call_id) {
                    return call["function"]["name"]
                        .as_str()
                        .unwrap_or("tool")
                        .to_owned();
                }
            }
        }
    }
    "tool".into()
}

fn is_proposal(message: &Value) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(message["content"].as_str().unwrap_or_default())
    else {
        return false;
    };
    value.get("status").and_then(Value::as_str) == Some("pending_user_confirmation")
        || value.get("applied") == Some(&Value::Bool(false))
}

fn proposal_target(view: &[Value], index: usize) -> String {
    let call_id = view[index]["tool_call_id"].as_str().unwrap_or_default();
    for message in view[..index].iter().rev() {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                if call["id"].as_str() != Some(call_id) {
                    continue;
                }
                let arguments = serde_json::from_str::<Value>(
                    call["function"]["arguments"].as_str().unwrap_or_default(),
                )
                .ok();
                if let Some(path) = arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("path"))
                    .and_then(Value::as_str)
                {
                    return path.to_owned();
                }
                return call["function"]["name"]
                    .as_str()
                    .unwrap_or("a subtree")
                    .to_owned();
            }
        }
    }
    "a subtree".into()
}

fn is_failure(message: &Value) -> bool {
    let content = message["content"].as_str().unwrap_or_default();
    if content.trim_start().starts_with('{')
        && let Ok(value) = serde_json::from_str::<Value>(content)
    {
        return value.get("error").is_some() || value.get("ok") == Some(&Value::Bool(false));
    }
    content.contains("\"error\"") || content.starts_with("failed")
}

fn summary_of_result(message: &Value) -> String {
    let content = message["content"].as_str().unwrap_or_default();
    if let Ok(value) = serde_json::from_str::<Value>(content) {
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return clip(
                &format!("failed: {}", collapse(error)),
                MAX_SUMMARY_RESULT_CHARS,
            );
        }
        if is_proposal(message) {
            return "proposed, not applied".into();
        }
        if let Some(object) = value.as_object() {
            let values: serde_json::Map<String, Value> = object
                .iter()
                .filter(|(_, value)| {
                    value.is_number()
                        || value.is_boolean()
                        || value.as_str().is_some_and(|text| text.len() <= 80)
                })
                .take(6)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            if values.is_empty() {
                return "Structured result; see saved artifact inventory for paths and findings"
                    .into();
            }
            return clip(
                &format!("ok: {}", Value::Object(values)),
                MAX_SUMMARY_RESULT_CHARS,
            );
        }
        if value.is_array() {
            return format!("ok: {} items", value.as_array().map(Vec::len).unwrap_or(0));
        }
    }
    if content.trim().is_empty() {
        return "ok: empty".into();
    }
    clip(&collapse(content), MAX_SUMMARY_RESULT_CHARS)
}

pub(super) fn should_compact(
    view: &[Value],
    overhead: u64,
    context_length: u64,
    max_output: u64,
    prompt_tokens: u64,
) -> bool {
    if view.len() > MAX_AUTO_COMPACT_MESSAGES {
        return true;
    }
    // A window of 0 or 1 is a placeholder for "unknown", not a size.
    if context_length <= 1 {
        return false;
    }
    let tokens = estimate_tokens(view) + media_tokens(view) + overhead;
    tokens > compaction_threshold(context_length, max_output, prompt_tokens)
}

fn tool_blocks(view: &[Value]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < view.len() {
        if !turns::calls_of(&view[index]).is_empty() {
            let mut end = index + 1;
            while end < view.len()
                && (turns::answered_call(&view[end]).is_some()
                    || turns::is_round_delivery(&view[end]))
            {
                end += 1;
            }
            blocks.push((index, end));
            index = end;
        } else {
            index += 1;
        }
    }
    blocks
}
