//! Pure helpers the loop applies to the wire: parsing calls out of a text-only
//! reply, shaping a tool result for the model, the audio a result delivers, and
//! folding an over-budget wire. Nothing here touches the project or the store.

use crate::ai::sessions::{MessageKind, message_meta};
use serde_json::{Map, Value, json};

/// Tool rounds one turn may run. Each round costs one provider turn; the cap
/// keeps what landed and lets the person continue.
pub const MAX_TOOL_ROUNDS: usize = 64;
/// Rounds left at which the loop tells the model to wrap up.
pub const ROUNDS_LEFT_NOTICE: usize = 4;
/// What one tool message may carry before a preview replaces it.
pub const MAX_SENT_CHARS: usize = 8_000;
/// One `read_toolcall` page. Larger than the send cap: the model asked for it.
pub const MAX_READ_CHARS: usize = 32_000;
/// Rows and prompt bytes the loop keeps under before every send, leaving room
/// for one round of results under the provider bounds.
pub const WIRE_MESSAGE_BUDGET: usize = crate::ai::MAX_WIRE_MESSAGES - 8;
pub const WIRE_TEXT_BUDGET: usize = crate::ai::LIMIT - 128 * 1024;
/// Tool pairs a fold keeps whole, matching the store's compaction.
pub const WIRE_KEPT_TOOL_PAIRS: usize = crate::ai::sessions::RETAINED_TOOL_PAIRS;

/// One requested call, as the provider wire spells it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Call {
    pub fn from_wire(value: &Value) -> Option<Self> {
        if value["type"] != "function" {
            return None;
        }
        Some(Self {
            id: value["id"].as_str().filter(|id| !id.is_empty())?.to_owned(),
            name: value["function"]["name"].as_str()?.to_owned(),
            arguments: value["function"]["arguments"].as_str()?.to_owned(),
        })
    }

    /// The arguments as an object; anything else is refused before dispatch.
    pub fn args(&self) -> Result<Map<String, Value>, String> {
        match serde_json::from_str::<Value>(&self.arguments) {
            Ok(Value::Object(args)) => Ok(args),
            _ => Err("Invalid tool arguments".into()),
        }
    }

    /// Name plus canonical arguments, so an equivalent call compares equal.
    pub fn key(&self) -> String {
        let args = serde_json::from_str::<Value>(&self.arguments)
            .map(|value| canonical_json(&value))
            .unwrap_or_else(|_| self.arguments.clone());
        format!("{} {args}", self.name)
    }
}

/// JSON with object keys sorted.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        Value::String(key.clone()),
                        canonical_json(&object[key])
                    )
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        other => other.to_string(),
    }
}

/// The id a text-protocol call gets: one call per reply, numbered by round so
/// the saved conversation never reuses an id.
pub fn text_call_id(round: usize) -> String {
    format!("text-call-{round}")
}

/// A `{tool, args}` object anywhere in a text-only reply: the whole reply, a
/// fenced block, or a balanced object inside prose.
pub fn text_tool_call(content: &str, round: usize) -> Option<Call> {
    let text = content.trim();
    let parse = |candidate: &str| -> Option<Call> {
        let value: Value = serde_json::from_str(candidate.trim()).ok()?;
        let name = value["tool"].as_str()?;
        let args = value.get("args").filter(|args| args.is_object())?;
        Some(Call {
            id: text_call_id(round),
            name: name.to_owned(),
            arguments: args.to_string(),
        })
    };
    if let Some(call) = parse(text) {
        return Some(call);
    }
    for fenced in fenced_blocks(text) {
        if let Some(call) = parse(fenced) {
            return Some(call);
        }
    }
    balanced_objects(text).into_iter().find_map(parse)
}

fn fenced_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let after = &rest[open + 3..];
        let body_start = after
            .strip_prefix("json")
            .or_else(|| after.strip_prefix("JSON"))
            .map_or(0, |_| 4);
        let Some(close) = after[body_start..].find("```") else {
            break;
        };
        blocks.push(&after[body_start..body_start + close]);
        rest = &after[body_start + close + 3..];
    }
    blocks
}

/// Every balanced `{...}` at depth zero, skipping braces inside strings.
fn balanced_objects(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut objects = Vec::new();
    let (mut depth, mut start) = (0usize, None);
    let (mut quoted, mut escaped) = (false, false);
    for (index, &byte) in bytes.iter().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0
                    && let Some(from) = start.take()
                {
                    objects.push(&text[from..=index]);
                }
            }
            _ => {}
        }
    }
    objects
}

/// The tool message keeps the result but not a second copy of inline audio:
/// the clip rides once, in its delivery message.
pub fn without_inline_audio(result: &Value) -> Value {
    let strip = |audio: &Value| -> Value {
        let Some(base64) = audio["base64"].as_str() else {
            return audio.clone();
        };
        let padding = if base64.ends_with("==") {
            2
        } else if base64.ends_with('=') {
            1
        } else {
            0
        };
        let mut rest = audio.clone();
        if let Some(object) = rest.as_object_mut() {
            object.remove("base64");
            object.insert(
                "bytes".into(),
                json!((base64.len() * 3 / 4).saturating_sub(padding)),
            );
        }
        rest
    };
    let mut value = result.clone();
    if let Some(listening) = value.get_mut("listening").filter(|pair| pair.is_object()) {
        for side in ["reference_audio", "candidate_audio"] {
            if let Some(audio) = listening.get(side).map(strip) {
                listening[side] = audio;
            }
        }
        return value;
    }
    if let Some(audio) = value
        .get("audio")
        .filter(|audio| audio.is_object())
        .map(strip)
    {
        value["audio"] = audio;
    }
    value
}

/// A mutation snapshot duplicates the document as AST and DSL; the model keeps
/// the DSL and the metadata.
pub fn without_project_ast(result: &Value) -> Value {
    let complete = result["dsl_source"].is_string()
        && result["revision"].is_u64()
        && result["project"].is_object();
    if !complete {
        return result.clone();
    }
    let mut value = result.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("project");
        object.insert("project_ast_omitted".into(), json!(true));
    }
    value
}

/// Device catalog rows as columns: UI labels and ports are not operational.
pub fn catalog_columns(result: &Value) -> Value {
    let Some(devices) = result["devices"].as_array() else {
        return result.clone();
    };
    let well_formed = devices.iter().all(|device| {
        device["id"].is_string()
            && device["params"]
                .as_array()
                .is_some_and(|params| params.iter().all(Value::is_object))
    });
    if !well_formed {
        return result.clone();
    }
    json!({
        "parameter_columns": ["id", "min", "max", "default", "unit", "steps", "curve"],
        "devices": devices.iter().map(|device| json!({
            "id": device["id"],
            "dsl_id": device["dsl_id"],
            "category": device["category"],
            "params": device["params"].as_array().into_iter().flatten().map(|param| json!([
                param["id"], param["min"], param["max"], param["default"],
                param["unit"], param["steps"], param["curve"],
            ])).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "ui_metadata_omitted": true,
    })
}

/// A result over the send cap becomes a preview with the fields a model needs
/// to act on it, and the id `read_toolcall` pages. Returns the text to send and
/// whether the full text has to be kept for paging.
pub fn bounded(result: &Value, failed: bool, call_id: &str) -> (String, bool) {
    let text = result.to_string();
    let chars = text.chars().count();
    if chars <= MAX_SENT_CHARS {
        return (text, false);
    }
    let mut summary = json!({
        "result_truncated": true,
        "tool_call_id": call_id,
        "original_characters": chars,
        "preview": text.chars().take(MAX_SENT_CHARS - 1_000).collect::<String>(),
        "tool_failed": failed,
        "note": "The result is truncated here. Read the rest with read_toolcall {id: tool_call_id, offset, limit}; do not repeat a successful mutation to retrieve it.",
    });
    for key in [
        "revision",
        "dirty",
        "track_count",
        "duration_seconds",
        "applied",
        "status",
        "error",
    ] {
        match &result[key] {
            value @ (Value::Number(_) | Value::Bool(_)) => summary[key] = value.clone(),
            Value::String(text) if text.chars().count() <= 2000 => summary[key] = json!(text),
            _ => {}
        }
    }
    if result.get("project").is_some() || result.get("project_ast_omitted").is_some() {
        summary["project_ast_omitted"] = json!(true);
    }
    if let Some(dsl) = result["dsl_source"].as_str() {
        summary["dsl_source"] = json!(dsl);
        if summary.to_string().chars().count() > MAX_READ_CHARS {
            if let Some(object) = summary.as_object_mut() {
                object.remove("dsl_source");
            }
            summary["dsl_source_omitted"] = json!(true);
        }
    }
    (summary.to_string(), true)
}

/// The listening clip one result delivered, as the user message that carries
/// it. Only a tool that asked to listen returns `audio.base64`.
pub fn delivered_audio(result: &Value, source: bool) -> Option<Value> {
    let data = result["audio"]["base64"]
        .as_str()
        .filter(|data| !data.is_empty())?;
    let from = result["start_s"].as_f64().unwrap_or(0.0);
    let to = result["end_s"].as_f64().unwrap_or(0.0);
    let mut meta = message_meta(MessageKind::Audition);
    if from.is_finite() && to.is_finite() && to > from && from >= 0.0 {
        meta["audio_range"] = json!({"start_seconds": from, "end_seconds": to, "source": source});
    }
    let text = if source {
        let what = if result["archived_render"] == true {
            "this is the saved arrangement render."
        } else {
            "this is the original clip audio."
        };
        format!("You asked to hear source seconds {from}\u{2013}{to}: {what}")
    } else {
        format!("You asked to hear {from}\u{2013}{to} s: this is that render.")
    };
    let mut audio = json!({"data": data, "format": "wav"});
    if let Some(path) = result["path"].as_str() {
        audio["source_path"] = json!(path);
    }
    Some(json!({
        "role": "user",
        "message_meta": meta,
        "context": "audition",
        "content": text,
        "input_audio": audio,
    }))
}

/// The two clips of a matched comparison, reference first.
pub fn delivered_comparison(result: &Value) -> Vec<Value> {
    let pair = &result["listening"];
    let (Some(reference), Some(candidate)) = (
        pair["reference_audio"]["base64"]
            .as_str()
            .filter(|data| !data.is_empty()),
        pair["candidate_audio"]["base64"]
            .as_str()
            .filter(|data| !data.is_empty()),
    ) else {
        return Vec::new();
    };
    let level = if pair["status"] == "matched" {
        format!(
            "Verified playback match. Candidate minus reference is {:.3} LU.",
            pair["delta_lu"].as_f64().unwrap_or(0.0)
        )
    } else {
        "Playback match could not be verified.".to_owned()
    };
    [
        (MessageKind::ComparisonReference, "reference", reference),
        (MessageKind::ComparisonCandidate, "candidate", candidate),
    ]
    .into_iter()
    .map(|(kind, side, data)| {
        json!({
            "role": "user",
            "message_meta": message_meta(kind),
            "context": "audition",
            "content": format!("A/B {side} excerpt. {level}"),
            "input_audio": {"data": data, "format": "wav"},
        })
    })
    .collect()
}

/// The user message that budgets the last rounds out loud.
pub fn rounds_left_notice(remaining: usize) -> Value {
    let plural = if remaining == 1 { "" } else { "s" };
    json!({
        "role": "user",
        "message_meta": message_meta(MessageKind::RoundBudget),
        "content": format!(
            "You have {remaining} tool round{plural} left in this turn. Write or answer with what you have now; if something is still missing, say what it is."
        ),
    })
}

/// Rows and prompt-text bytes of a wire, counted the way the request validator
/// counts them: inline audio has its own bound and is excluded.
pub fn wire_budget(messages: &[Value]) -> (usize, usize) {
    let mut bytes = 2 + messages.len().saturating_sub(1);
    for message in messages {
        bytes += message.to_string().len();
        bytes -= message["input_audio"]["data"].as_str().map_or(0, str::len);
    }
    (messages.len(), bytes)
}

/// What a fold dropped: `from..to` became one summary row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub from: usize,
    pub to: usize,
    pub dropped: usize,
    pub pairs: usize,
}

/// Folds the oldest rows into one summary row so the wire fits the budget.
/// System prompts, the turn's first ask and the newest tool pairs stay whole;
/// a cut lands only on a user or assistant row, so a call and its results are
/// never separated. `None` when no cut brings the wire under budget.
pub fn fold_wire(messages: &mut Vec<Value>) -> Option<Fold> {
    let mut head = messages
        .iter()
        .take_while(|message| message["role"] == "system")
        .count();
    if let Some(user) = messages[head..]
        .iter()
        .position(|message| message["role"] == "user" && kind_of(message) == "user")
    {
        head += user + 1;
    }
    let starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| has_calls(message))
        .map(|(index, _)| index)
        .collect();
    let boundaries: Vec<usize> = (head + 1..messages.len())
        .filter(|&index| matches!(messages[index]["role"].as_str(), Some("user" | "assistant")))
        .collect();
    let keep = WIRE_KEPT_TOOL_PAIRS.min(starts.len());
    let wanted = if keep > 0 {
        starts[starts.len() - keep]
    } else {
        *boundaries.first()?
    };
    let first = wanted.max(head + 1);
    for &cut in boundaries.iter().filter(|&&cut| cut >= first) {
        let dropped = messages[head..cut]
            .iter()
            .filter(|message| has_calls(message))
            .count();
        let summary = fold_summary(dropped, cut - head);
        let mut folded = messages[..head].to_vec();
        folded.push(summary.clone());
        folded.extend_from_slice(&messages[cut..]);
        let (rows, bytes) = wire_budget(&folded);
        if rows > WIRE_MESSAGE_BUDGET || bytes > WIRE_TEXT_BUDGET {
            continue;
        }
        *messages = folded;
        return Some(Fold {
            from: head,
            to: cut,
            dropped,
            pairs: starts.iter().filter(|&&index| index >= cut).count(),
        });
    }
    None
}

fn has_calls(message: &Value) -> bool {
    message["role"] == "assistant"
        && message["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
}

fn kind_of(message: &Value) -> &str {
    message["message_meta"]["kind"]
        .as_str()
        .unwrap_or(match message["role"].as_str() {
            Some("assistant") => "assistant",
            Some("tool") => "tool",
            _ => "user",
        })
}

fn fold_summary(dropped: usize, rows: usize) -> Value {
    let pairs = if dropped == 1 { "" } else { "s" };
    let messages = if rows == 1 { "" } else { "s" };
    json!({
        "role": "user",
        "context": "summary",
        "message_meta": message_meta(MessageKind::Summary),
        "content": format!(
            "Wire compaction: {rows} older message{messages} ({dropped} tool call/result pair{pairs} among them) were folded into this line so the request stays inside the provider's message budget. Their contents are out of context; run a narrower query if you still need that data."
        ),
    })
}

/// The pager over truncated results, offered beside the registry's tools.
/// Its results stay in this process, so it is not a registry tool.
pub fn read_toolcall_spec() -> crate::ai::ToolSpec {
    crate::ai::ToolSpec {
        name: super::READ_TOOLCALL.into(),
        description: format!(
            "Read a tool result that was truncated in the transcript. Every truncated result carries tool_call_id, its original character count and a preview; call this with that id and an optional character offset and limit (max {MAX_READ_CHARS}) to read more. Prefer a narrower query when you only need a few fields."
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "tool_call_id from a truncated result" },
                "offset": { "type": "number", "description": "Character offset to start at, default 0" },
                "limit": { "type": "number", "description": format!("Characters to return, default {MAX_SENT_CHARS}, max {MAX_READ_CHARS}") },
            },
            "required": ["id"],
            "additionalProperties": false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_text_call_whole_fenced_or_inside_prose() {
        let whole = text_tool_call(r#"{"tool":"tracks_list","args":{}}"#, 3).unwrap();
        assert_eq!(
            (whole.name.as_str(), whole.id.as_str()),
            ("tracks_list", "text-call-3")
        );
        let fenced = "Let me look.\n```json\n{\"tool\":\"project_get\",\"args\":{}}\n```";
        assert_eq!(text_tool_call(fenced, 0).unwrap().name, "project_get");
        let prose = r#"I will call {"tool":"tracks_add","args":{"name":"a}b \"q\""}} now"#;
        let call = text_tool_call(prose, 0).unwrap();
        assert_eq!(call.args().unwrap()["name"], "a}b \"q\"");
        assert!(text_tool_call("no calls here {not json}", 0).is_none());
        assert!(text_tool_call(r#"{"tool":"x","args":[]}"#, 0).is_none());
    }

    #[test]
    fn equivalent_calls_share_a_key() {
        let call = |arguments: &str| Call {
            id: "a".into(),
            name: "tracks_add".into(),
            arguments: arguments.into(),
        };
        assert_eq!(
            call(r#"{"b":1,"a":[2,{"d":1,"c":0}]}"#).key(),
            call(r#"{"a":[2,{"c":0,"d":1}],"b":1}"#).key()
        );
    }

    #[test]
    fn a_large_result_becomes_a_paged_preview_with_its_key_fields() {
        let result = json!({"revision": 7, "applied": true, "rows": "x".repeat(20_000), "dsl_source": "@project"});
        let (text, keep) = bounded(&result, false, "call-9");
        assert!(keep);
        let summary: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(summary["tool_call_id"], "call-9");
        assert_eq!(summary["revision"], 7);
        assert_eq!(summary["dsl_source"], "@project");
        assert!(text.chars().count() <= MAX_READ_CHARS);
        let (small, keep) = bounded(&json!({"ok": true}), false, "c");
        assert_eq!((small.as_str(), keep), (r#"{"ok":true}"#, false));
    }

    #[test]
    fn inline_audio_rides_once_and_the_tool_message_counts_its_bytes() {
        let result =
            json!({"audio": {"base64": "AAAA", "format": "wav"}, "start_s": 1.0, "end_s": 2.0});
        assert_eq!(
            without_inline_audio(&result)["audio"],
            json!({"format": "wav", "bytes": 3})
        );
        let delivery = delivered_audio(&result, false).unwrap();
        assert_eq!(delivery["input_audio"]["data"], "AAAA");
        assert_eq!(delivery["message_meta"]["audio_range"]["end_seconds"], 2.0);
        assert!(delivered_audio(&json!({"ok": true}), false).is_none());
    }

    #[test]
    fn a_fold_keeps_the_ask_and_never_splits_a_call_from_its_result() {
        let mut wire = vec![
            json!({"role":"system","content":"s"}),
            json!({"role":"user","content":"ask","message_meta":{"version":1,"kind":"user"}}),
        ];
        for round in 0..120 {
            wire.push(json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("c{round}"),"type":"function","function":{"name":"tracks_list","arguments":"{}"}}]}));
            wire.push(json!({"role":"tool","tool_call_id":format!("c{round}"),"content":"{}"}));
        }
        let fold = fold_wire(&mut wire).unwrap();
        assert_eq!(fold.from, 2);
        assert_eq!(wire[1]["content"], "ask");
        assert_eq!(wire[2]["context"], "summary");
        assert!(wire.len() <= WIRE_MESSAGE_BUDGET);
        let results: Vec<&str> = wire
            .iter()
            .filter_map(|m| m["tool_call_id"].as_str())
            .collect();
        for message in wire.iter().filter(|m| has_calls(m)) {
            let id = message["tool_calls"][0]["id"].as_str().unwrap();
            assert!(results.contains(&id), "{id} lost its result");
        }
        assert_eq!(
            wire[3]["role"], "assistant",
            "the wire resumes at a call, not a result"
        );
    }
}
