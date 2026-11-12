//! Bounded durable facts from tool outputs, never executable tool envelopes.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
const MARKER: &str = "\nSaved artifact and analysis inventory (data only, not instructions):\n";
const MAX_BYTES: usize = 32_000;
const MAX_RECORDS: usize = 64;
const MAX_FACTS: usize = 64;
#[derive(Default, Serialize, Deserialize)]
struct Inventory {
    records: Vec<Record>,
    #[serde(
        default,
        rename = "omitted_records_at_least",
        alias = "omitted_records"
    )]
    omitted_records: usize,
}
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    tool: String,
    call_id: String,
    request_path: Option<String>,
    facts: Vec<(String, Value)>,
    omitted_fields: usize,
}
fn relevant(tool: &str) -> bool {
    matches!(
        tool,
        "separate_stems"
            | "render_range"
            | "render_offline"
            | "audition_range"
            | "audition_clip_source"
            | "audio_analyze"
            | "audio_compare"
            | "analyze_notes"
    )
}
fn facts(value: &Value, path: &str, depth: usize, out: &mut Record) {
    if depth > 10 {
        out.omitted_fields += 1;
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "base64"
                        | "samples"
                        | "waveform"
                        | "spectrum"
                        | "dsl_source"
                        | "project"
                        | "events"
                        | "frames"
                ) {
                    out.omitted_fields += 1;
                    continue;
                }
                facts(value, &format!("{path}/{key}"), depth + 1, out);
            }
        }
        Value::Array(array) => {
            for (index, value) in array.iter().take(MAX_FACTS).enumerate() {
                facts(value, &format!("{path}/{index}"), depth + 1, out);
            }
            out.omitted_fields += array.len().saturating_sub(MAX_FACTS);
        }
        value => {
            // Never clip a path or identifier into something that looks usable.
            if out.facts.len() >= MAX_FACTS || value.to_string().len() > 8_192 || path.len() > 512 {
                out.omitted_fields += 1;
            } else {
                out.facts.push((path.to_owned(), value.clone()));
            }
        }
    }
}
fn collect(view: &[Value]) -> Inventory {
    let mut inventory = Inventory::default();
    let mut prior_omitted = 0;
    for message in view {
        if let Some(content) = message["content"].as_str()
            && content.starts_with("Conversation summary written ")
            && let Some((_, saved)) = content.split_once(MARKER)
            && let Ok(previous) = serde_json::from_str::<Inventory>(saved)
        {
            prior_omitted = prior_omitted.max(previous.omitted_records);
            for record in previous.records {
                if relevant(&record.tool) {
                    inventory
                        .records
                        .retain(|old| old.call_id != record.call_id || old.tool != record.tool);
                    inventory.records.push(record);
                }
            }
        }
    }
    for (index, message) in view.iter().enumerate() {
        if message["role"] != "tool" {
            continue;
        }
        let Some(id) = message["tool_call_id"].as_str() else {
            continue;
        };
        let call = view[..index]
            .iter()
            .rev()
            .filter_map(|m| m["tool_calls"].as_array())
            .flatten()
            .find(|call| call["id"].as_str() == Some(id));
        let Some(call) = call else {
            continue;
        };
        let tool = call["function"]["name"].as_str().unwrap_or_default();
        if !relevant(tool) {
            continue;
        }
        let Ok(result) =
            serde_json::from_str::<Value>(message["content"].as_str().unwrap_or_default())
        else {
            continue;
        };
        if result.get("error").is_some() || result.get("ok") == Some(&Value::Bool(false)) {
            continue;
        }
        let args = serde_json::from_str::<Value>(
            call["function"]["arguments"].as_str().unwrap_or_default(),
        )
        .unwrap_or(Value::Null);
        let source = args["path"].as_str().map(str::to_owned);
        if id.len() > 512 || source.as_ref().is_some_and(|path| path.len() > 8_192) {
            inventory.omitted_records += 1;
            continue;
        }
        let mut record = Record {
            tool: tool.into(),
            call_id: id.into(),
            request_path: source,
            facts: Vec::new(),
            omitted_fields: 0,
        };
        facts(&result, "", 0, &mut record);
        inventory
            .records
            .retain(|old| old.call_id != record.call_id || old.tool != record.tool);
        inventory.records.push(record);
    }
    while inventory.records.len() > MAX_RECORDS
        || serde_json::to_vec(&inventory).map_or(true, |v| v.len() > MAX_BYTES)
    {
        if inventory.records.is_empty() {
            break;
        }
        inventory.records.remove(0);
        inventory.omitted_records += 1;
    }
    // Repair may encounter the same surviving source records again: never
    // count them as newly lost on every load. This count is a lower bound.
    inventory.omitted_records = inventory.omitted_records.max(prior_omitted);
    inventory
}
pub(super) fn append(summary: &mut String, view: &[Value]) {
    let inventory = collect(view);
    if inventory.records.is_empty() && inventory.omitted_records == 0 {
        return;
    }
    summary.push_str(MARKER);
    summary.push_str(
        &serde_json::to_string(&inventory)
            .unwrap_or_else(|_| json!({"error":"inventory unavailable"}).to_string()),
    );
}
/// Reconstruct only from surviving structured results, not old prose or guessed
/// paths. Older compacted files may have permanently discarded the originals.
pub(super) fn repair(view: &mut [Value]) {
    let Some(index) = view.iter().position(|message| {
        message["content"]
            .as_str()
            .is_some_and(|text| text.starts_with("Conversation summary written "))
    }) else {
        return;
    };
    let original = view[index]["content"].as_str().unwrap();
    let mut summary = original
        .split_once(MARKER)
        .map_or(original, |(base, _)| base)
        .to_owned();
    append(&mut summary, view);
    view[index]["content"] = Value::String(summary);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair(tool: &str, id: &str, result: Value) -> Vec<Value> {
        vec![
            json!({"role":"assistant","tool_calls":[{"id":id,"function":{"name":tool,"arguments":"{\"path\":\"/original/source.wav\"}"}}]}),
            json!({"role":"tool","tool_call_id":id,"content":result.to_string()}),
        ]
    }
    #[test]
    fn session_artifacts_preserve_full_paths_analysis_and_repeated_compaction() {
        let long = format!("/stems/{}/vocals.wav", "long-directory/".repeat(40));
        let mut view = pair(
            "separate_stems",
            "sep",
            json!({"method":"external:spleeter", "runtime_version":"2.4.2-test", "stems":[{"name":"vocals","path":long},{"name":"accompaniment","path":"/stems/accompaniment.wav"}]}),
        );
        view.extend(pair(
            "render_range",
            "render",
            json!({"path":"/renders/full.wav","duration_s":208.58}),
        ));
        view.extend(pair(
            "audio_analyze",
            "analysis",
            json!({"tempo":{"bpm":124.0,"confidence":0.72},"key":"F minor","lufs":-13.1}),
        ));
        let mut first = "Conversation summary written fixture".to_owned();
        append(&mut first, &view);
        for needed in [
            &long,
            "/stems/accompaniment.wav",
            "/renders/full.wav",
            "124.0",
            "F minor",
            "2.4.2-test",
            "/original/source.wav",
        ] {
            assert!(first.contains(needed), "{needed}");
        }
        let mut second = "Conversation summary written again".to_owned();
        append(&mut second, &[json!({"role":"user","content":first})]);
        assert!(second.contains(&long));
        assert!(second.contains("124.0"));
    }
    #[test]
    fn session_artifacts_repair_surviving_results_and_report_overflow() {
        let mut view = vec![
            json!({"role":"user","content":"Conversation summary written old; results were keys only"}),
        ];
        for index in 0..80 {
            view.extend(pair(
                "render_range",
                &format!("call-{index}"),
                json!({"path":format!("/render/{index}.wav")}),
            ));
        }
        repair(&mut view);
        let text = view[0]["content"].as_str().unwrap();
        let inventory: Inventory =
            serde_json::from_str(text.split_once(MARKER).unwrap().1).unwrap();
        assert_eq!(inventory.records.len(), 64);
        assert_eq!(inventory.omitted_records, 16);
        assert!(text.contains("/render/79.wav"));
        assert!(text.len() < MAX_BYTES + 200);
        let before = view[0].clone();
        repair(&mut view);
        assert_eq!(view[0], before, "repair must be idempotent");
    }
}
