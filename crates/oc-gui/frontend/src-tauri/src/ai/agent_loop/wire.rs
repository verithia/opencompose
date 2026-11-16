//! Pure helpers the loop applies to the wire: parsing calls out of a text-only
//! reply, shaping a tool result for the model, the audio a result delivers, and
//! folding an over-budget wire. Nothing here touches the project or the store.

use crate::ai::sessions::{MessageKind, message_meta, turns};
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

/// The id a text-protocol call gets: one call per reply. `turn` is unique per
/// turn and `round` per reply within it, so the saved conversation never
/// reuses an id across turns.
pub fn text_call_id(turn: &str, round: usize) -> String {
    format!("text-call-{turn}-{round}")
}

/// A `{tool, args}` object anywhere in a text-only reply: the whole reply, a
/// fenced block, or a balanced object inside prose.
pub fn text_tool_call(content: &str, id: &str) -> Option<Call> {
    let text = content.trim();
    let parse = |candidate: &str| -> Option<Call> {
        let value: Value = serde_json::from_str(candidate.trim()).ok()?;
        let name = value["tool"].as_str()?;
        let args = value.get("args").filter(|args| args.is_object())?;
        Some(Call {
            id: id.to_owned(),
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
    let requested_to = result["end_s"].as_f64().unwrap_or(0.0);
    let to = result["duration_s"]
        .as_f64()
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .map(|duration| requested_to.min(from + duration))
        .unwrap_or(requested_to);
    let mut meta = message_meta(MessageKind::Audition);
    if from.is_finite() && to.is_finite() && to > from && from >= 0.0 {
        meta["audio_range"] = json!({"start_seconds": from, "end_seconds": to, "source": source});
    }
    let mut text = if source {
        let what = if result["archived_render"] == true {
            "this is the saved arrangement render."
        } else {
            "this is the original clip audio."
        };
        format!("You asked to hear source seconds {from}\u{2013}{to}: {what}")
    } else {
        format!("You asked to hear {from}\u{2013}{to} s: this is that render.")
    };
    if result["truncated"] == true || to + 0.0001 < requested_to {
        text.push_str(&format!(
            " The listening clip ends at {to} s; the requested range ends at {requested_to} s. Request the remaining passage to hear it."
        ));
    }
    let mut audio = json!({"data": data, "format": "wav"});
    if result["source_render_unavailable"] == true {
        audio["source_render_unavailable"] = json!(true);
    } else if let Some(path) = result["source_render"]
        .as_str()
        .or_else(|| result["path"].as_str())
    {
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

/// What a fold dropped: `from..to` became one summary row. `ask` is where the
/// turn's ask sits in the folded wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub from: usize,
    pub to: usize,
    pub dropped: usize,
    pub pairs: usize,
    pub ask: Option<usize>,
}

/// The token side of a wire's budget, sized from the model's context window
/// with the same estimator the store's compaction uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenBudget {
    /// The model's input window. A wire measured above it is not sent.
    pub window: u64,
    /// Tokens every request carries beside the wire: the tool list.
    pub fixed: u64,
    /// Reported over estimated input of the last request, at least 1, so a
    /// provider whose tokenizer counts more than the estimate is measured by
    /// what it reported.
    pub scale: f64,
    /// Output tokens the model may write; held back from the trigger.
    pub max_output: u64,
    /// Tokens the compaction prompt costs; held back from the trigger.
    pub prompt_tokens: u64,
}

/// The highest scale a report may set; anything above is a broken report.
const MAX_SCALE: f64 = 4.0;

impl TokenBudget {
    /// `None` when the window is unknown (0 or 1).
    pub fn new(window: u64, fixed: u64) -> Option<Self> {
        (window > 1).then_some(Self {
            window,
            fixed,
            scale: 1.0,
            max_output: 0,
            prompt_tokens: 0,
        })
    }

    /// The estimate before calibration: text, attachments and the fixed part.
    fn estimate(&self, wire: &[Value]) -> u64 {
        crate::ai::sessions::estimate_tokens(wire)
            + crate::ai::sessions::media_tokens(wire)
            + self.fixed
    }

    /// The wire's size in tokens: the estimate, or what the provider reported
    /// for the same wire when that is larger.
    pub fn measure(&self, wire: &[Value]) -> u64 {
        (self.estimate(wire) as f64 * self.scale).ceil() as u64
    }

    /// Learns from a provider's reported input for exactly `wire`. A report
    /// below the estimate leaves the estimate in charge.
    pub fn calibrate(&mut self, wire: &[Value], reported: u64) {
        let estimate = self.estimate(wire).max(1);
        let scale = reported as f64 / estimate as f64;
        if scale.is_finite() {
            self.scale = scale.clamp(1.0, MAX_SCALE);
        }
    }

    /// The size above which a wire compacts, from the same function the store
    /// uses so admission and the loop never disagree.
    pub fn limit(&self) -> u64 {
        crate::ai::sessions::compaction_threshold(self.window, self.max_output, self.prompt_tokens)
    }

    /// Holds back the output the model may write and the compaction prompt, so
    /// the trigger leaves room for both.
    pub fn for_compaction(mut self, max_output: u64, prompt_tokens: u64) -> Self {
        self.max_output = max_output;
        self.prompt_tokens = prompt_tokens;
        self
    }

    /// The share a fold aims for, so the next rounds do not fold again at once.
    fn target(&self) -> u64 {
        (self.window as f64 * crate::ai::sessions::AUTO_COMPACT_TARGET_FRACTION) as u64
    }
}

/// The bounds a fold brings a wire under.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoldLimits {
    pub rows: usize,
    pub bytes: usize,
    pub tokens: Option<TokenBudget>,
}

impl FoldLimits {
    /// The loop's bounds before every send.
    pub fn round(tokens: Option<TokenBudget>) -> Self {
        Self {
            rows: WIRE_MESSAGE_BUDGET,
            bytes: WIRE_TEXT_BUDGET,
            tokens,
        }
    }

    fn fits_shape(&self, wire: &[Value]) -> bool {
        let (rows, bytes) = wire_budget(wire);
        rows <= self.rows && bytes <= self.bytes
    }

    /// Whether the wire is inside every bound.
    pub fn fits(&self, wire: &[Value]) -> bool {
        self.fits_shape(wire)
            && self
                .tokens
                .is_none_or(|tokens| tokens.measure(wire) <= tokens.limit())
    }
}

/// Tokens the tool list of one request costs: the registry tools the
/// selection offers, plus the result pager when the request carries it.
pub fn offered_tool_tokens(toolsets: Option<&[String]>, pager: bool) -> u64 {
    offered_tool_specs(toolsets, pager)
        .iter()
        .map(|spec| {
            crate::ai::sessions::value_tokens(&json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters,
            }))
        })
        .sum()
}

/// The tools one request offers, from the same selection the provider request
/// uses: the registry's panel tools, plus the result pager when it applies.
pub fn offered_tool_specs(toolsets: Option<&[String]>, pager: bool) -> Vec<crate::ai::ToolSpec> {
    let mut specs = crate::ai::tool_specs_for(toolsets);
    if pager {
        specs.push(read_toolcall_spec());
    }
    specs
}

/// The sorted, deduplicated names a loop turn offers under a toolset
/// selection. The transcript compares these names between turns.
pub fn offered_tool_names(toolsets: Option<&[String]>) -> Vec<String> {
    let mut names: Vec<String> = offered_tool_specs(toolsets, true)
        .into_iter()
        .map(|spec| spec.name)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The turn's ask: the newest message a person wrote that is not steering.
/// User-kind rows after it are steering delivered into the running turn.
pub fn turn_ask(messages: &[Value]) -> Option<usize> {
    messages.iter().rposition(|message| {
        message["role"] == "user" && message_kind(message) == Some(MessageKind::User)
    })
}

/// Where a fold may resume the wire: a person's message or an assistant
/// round, never a result or a delivery that belongs to the round before it.
fn is_boundary(message: &Value) -> bool {
    matches!(message["role"].as_str(), Some("user" | "assistant"))
        && turns::answered_call(message).is_none()
        && !turns::is_round_delivery(message)
}

fn has_calls(message: &Value) -> bool {
    !turns::calls_of(message).is_empty()
}

fn message_kind(message: &Value) -> Option<MessageKind> {
    crate::ai::sessions::message_kind(message)
}

/// Folds the oldest rows into one summary row so the wire fits `limits`.
/// System prompts, the turn's ask, the steering delivered after it, the
/// store's latest compaction summary and the newest tool pairs stay whole; a
/// cut lands only where the pairing rules allow a new message, so a call and
/// its results, native or text protocol, are never separated. `ask` is the
/// index of the turn's ask. `None` when no cut brings the wire under budget.
pub fn fold_wire(
    messages: &mut Vec<Value>,
    limits: &FoldLimits,
    ask: Option<usize>,
) -> Option<Fold> {
    let head = messages
        .iter()
        .take_while(|message| message["role"] == "system")
        .count();
    let ask = ask.filter(|&index| index >= head && index < messages.len());
    let summary = messages[head..ask.unwrap_or(messages.len())]
        .iter()
        .rposition(crate::ai::sessions::is_compaction_summary)
        .map(|offset| head + offset);
    let notices: std::collections::HashSet<usize> =
        crate::ai::sessions::pinned_notice_indices(&messages[head..ask.unwrap_or(messages.len())])
            .into_iter()
            .map(|offset| head + offset)
            .collect();
    // The derived goal-and-plan row is not saved, so it names itself with a
    // context marker; the newest goal and plan rows are pinned through the
    // store's indexes. Both must survive a fold.
    let state_context: std::collections::HashSet<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message["context"] == crate::ai::session_state::CONTEXT_STATE)
        .map(|(index, _)| index)
        .collect();
    let pinned = |index: usize| {
        Some(index) == summary
            || notices.contains(&index)
            || state_context.contains(&index)
            || ask.is_some_and(|ask| {
                index >= ask && message_kind(&messages[index]) == Some(MessageKind::User)
            })
    };
    let starts: Vec<usize> = (head..messages.len())
        .filter(|&index| has_calls(&messages[index]))
        .collect();
    let boundaries: Vec<usize> = (head + 1..messages.len())
        .filter(|&index| is_boundary(&messages[index]))
        .collect();
    let keep = WIRE_KEPT_TOOL_PAIRS.min(starts.len());
    let wanted = if keep > 0 {
        starts[starts.len() - keep]
    } else {
        *boundaries.first()?
    };
    let first = wanted.max(head + 1);
    // A wire that pairs is folded only into one that still pairs.
    let paired = turns::check_pairing(&messages[head..]).is_ok();
    let mut fallback = None;
    for &cut in boundaries.iter().filter(|&&cut| cut >= first) {
        let kept: Vec<usize> = (head..cut).filter(|&index| pinned(index)).collect();
        let rows = cut - head - kept.len();
        if rows == 0 {
            continue;
        }
        let dropped = (head..cut)
            .filter(|&index| !pinned(index) && has_calls(&messages[index]))
            .count();
        let mut folded = messages[..head].to_vec();
        folded.extend(kept.iter().map(|&index| messages[index].clone()));
        let from = folded.len();
        folded.push(fold_summary(dropped, rows));
        folded.extend_from_slice(&messages[cut..]);
        if (paired && turns::check_pairing(&folded[head..]).is_err()) || !limits.fits(&folded) {
            continue;
        }
        let fold = Fold {
            from,
            to: cut,
            dropped,
            pairs: starts.iter().filter(|&&index| index >= cut).count(),
            ask: ask.map(|ask| match kept.iter().position(|&index| index == ask) {
                Some(position) => head + position,
                None => ask - cut + from + 1,
            }),
        };
        // The first cut inside every bound wins when it also reaches the
        // token target; otherwise a later cut may, and the first is kept in
        // case none does.
        let on_target = limits
            .tokens
            .is_none_or(|tokens| tokens.measure(&folded) <= tokens.target());
        if on_target {
            *messages = folded;
            return Some(fold);
        }
        if fallback.is_none() {
            fallback = Some((folded, fold));
        }
    }
    let (folded, fold) = fallback?;
    *messages = folded;
    Some(fold)
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
    fn audition_delivery_names_only_the_passage_the_clip_contains() {
        let value = json!({"start_s":98.0,"end_s":120.0,"duration_s":16.0,"truncated":true,"audio":{"base64":"fixture"}});
        let delivery = delivered_audio(&value, false).unwrap();
        assert_eq!(
            delivery["message_meta"]["audio_range"]["end_seconds"],
            114.0
        );
        assert!(
            delivery["content"]
                .as_str()
                .unwrap()
                .contains("98\u{2013}114")
        );
        assert!(
            delivery["content"]
                .as_str()
                .unwrap()
                .contains("requested range ends at 120")
        );
        let source = delivered_audio(&value, true).unwrap();
        assert_eq!(source["message_meta"]["audio_range"]["end_seconds"], 114.0);
        assert_eq!(source["message_meta"]["audio_range"]["source"], true);
    }

    #[test]
    fn audition_delivery_uses_its_immutable_source_and_fails_closed_when_missing() {
        let mut value = json!({"path":"/export/reused.wav","source_render":"/scratch/immutable.wav","audio":{"base64":"fixture"}});
        assert_eq!(
            delivered_audio(&value, false).unwrap()["input_audio"]["source_path"],
            "/scratch/immutable.wav"
        );
        value["source_render_unavailable"] = json!(true);
        let delivery = delivered_audio(&value, false).unwrap();
        assert!(delivery["input_audio"].get("source_path").is_none());
        assert_eq!(delivery["input_audio"]["source_render_unavailable"], true);
    }

    #[test]
    fn finds_a_text_call_whole_fenced_or_inside_prose() {
        let whole =
            text_tool_call(r#"{"tool":"tracks_list","args":{}}"#, &text_call_id("t", 3)).unwrap();
        assert_eq!(
            (whole.name.as_str(), whole.id.as_str()),
            ("tracks_list", "text-call-t-3")
        );
        let fenced = "Let me look.\n```json\n{\"tool\":\"project_get\",\"args\":{}}\n```";
        assert_eq!(
            text_tool_call(fenced, "text-call-0").unwrap().name,
            "project_get"
        );
        let prose = r#"I will call {"tool":"tracks_add","args":{"name":"a}b \"q\""}} now"#;
        let call = text_tool_call(prose, "text-call-0").unwrap();
        assert_eq!(call.args().unwrap()["name"], "a}b \"q\"");
        assert!(text_tool_call("no calls here {not json}", "text-call-0").is_none());
        assert!(text_tool_call(r#"{"tool":"x","args":[]}"#, "text-call-0").is_none());
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
        let ask = turn_ask(&wire);
        let fold = fold_wire(&mut wire, &FoldLimits::round(None), ask).unwrap();
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

    fn user(text: &str) -> Value {
        json!({"role":"user","content":text,"message_meta":{"version":1,"kind":"user"}})
    }

    fn native_round(wire: &mut Vec<Value>, id: &str, payload: usize) {
        wire.push(json!({"role":"assistant","content":null,"message_meta":{"version":1,"kind":"assistant"},
            "tool_calls":[{"id":id,"type":"function","function":{"name":"tracks_list","arguments":"{}"}}]}));
        wire.push(
            json!({"role":"tool","tool_call_id":id,"message_meta":{"version":1,"kind":"tool"},
            "content":"r".repeat(payload)}),
        );
    }

    #[test]
    fn a_fold_keeps_this_turn_s_ask_and_its_steering_not_the_first_ask() {
        let mut wire = vec![
            json!({"role":"system","content":"s"}),
            user("an older ask"),
            json!({"role":"assistant","content":"older answer","message_meta":{"version":1,"kind":"assistant"}}),
            user("the current ask"),
        ];
        for round in 0..120 {
            native_round(&mut wire, &format!("c{round}"), 2);
            if round == 10 {
                let mut steer = user("make the drop longer");
                steer["message_meta"]["steer_id"] = json!("row-1");
                wire.push(steer);
            }
        }
        let ask = turn_ask(&wire[..4]);
        assert_eq!(ask, Some(3));
        let fold = fold_wire(&mut wire, &FoldLimits::round(None), ask).unwrap();
        let texts: Vec<&str> = wire
            .iter()
            .filter_map(|row| row["content"].as_str())
            .collect();
        assert!(!texts.contains(&"an older ask"));
        assert!(texts.contains(&"make the drop longer"), "steering survives");
        assert_eq!(wire[fold.ask.unwrap()]["content"], "the current ask");
        assert_eq!(turns::check_pairing(&wire[1..]), Ok(()));
    }

    #[test]
    fn a_fold_never_resumes_on_a_text_protocol_result() {
        let mut wire = vec![json!({"role":"system","content":"s"}), user("ask")];
        for round in 0..120 {
            let id = format!("text-call-t-{round}");
            wire.push(json!({"role":"assistant","content":"{\"tool\":\"tracks_list\",\"args\":{}}",
                "message_meta":{"version":1,"kind":"assistant","text_call":{"id":id,"tool":"tracks_list"}}}));
            wire.push(json!({"role":"user","content":format!("{}{{}}", turns::APPLICATION_RESULT_PREFIX),
                "message_meta":{"version":1,"kind":"application_result","text_call":{"id":id,"tool":"tracks_list"}}}));
        }
        let ask = turn_ask(&wire);
        let fold = fold_wire(&mut wire, &FoldLimits::round(None), ask).unwrap();
        assert_eq!(turns::check_pairing(&wire[1..]), Ok(()));
        let resumed = &wire[fold.from + 1];
        assert!(turns::answered_call(resumed).is_none(), "{resumed}");
        assert!(fold.pairs <= WIRE_KEPT_TOOL_PAIRS);
    }

    #[test]
    fn a_fold_bounds_tokens_by_the_reported_input() {
        let mut wire = vec![json!({"role":"system","content":"s"}), user("ask")];
        for round in 0..20 {
            native_round(&mut wire, &format!("c{round}"), 4_000);
        }
        let mut tokens = TokenBudget::new(60_000, 0).unwrap();
        let limits = FoldLimits::round(Some(tokens));
        assert!(limits.fits(&wire), "the estimate alone fits the window");
        // The provider counted three times what the estimate says.
        tokens.calibrate(&wire, tokens.measure(&wire) * 3);
        let limits = FoldLimits::round(Some(tokens));
        assert!(!limits.fits(&wire));
        let ask = turn_ask(&wire);
        fold_wire(&mut wire, &limits, ask).unwrap();
        assert!(tokens.measure(&wire) <= tokens.limit());
        assert_eq!(wire[1]["content"], "ask");
    }

    #[test]
    fn a_fold_keeps_the_newest_project_binding_notice() {
        use crate::ai::host_notice::{BindCause, HostNotice, notice_row};
        let mut wire = vec![
            json!({"role":"system","content":"s"}),
            user("the first ask"),
            notice_row(&HostNotice::ProjectBound {
                from: None,
                to: "/songs/first.ocp".into(),
                cause: BindCause::Chosen,
            }),
            notice_row(&HostNotice::ProjectBound {
                from: Some("/songs/first.ocp".into()),
                to: "/songs/second.ocp".into(),
                cause: BindCause::Chosen,
            }),
        ];
        for round in 0..120 {
            native_round(&mut wire, &format!("c{round}"), 200);
        }
        wire.push(user("the current ask"));
        let ask = turn_ask(&wire);
        fold_wire(&mut wire, &FoldLimits::round(None), ask).unwrap();
        let bound: Vec<&str> = wire
            .iter()
            .filter_map(|row| row["message_meta"]["notice"]["kind"].as_str())
            .collect();
        assert_eq!(bound, ["project_bound"]);
        let kept = wire
            .iter()
            .find(|row| row["message_meta"]["notice"]["to"] == "/songs/second.ocp")
            .expect("the newest binding is kept");
        assert_eq!(kept["message_meta"]["notice"]["to"], "/songs/second.ocp");
        assert!(
            !wire
                .iter()
                .any(|row| row["message_meta"]["notice"]["to"] == "/songs/first.ocp"),
            "the older binding is folded away"
        );
        assert_eq!(turns::check_pairing(&wire[1..]), Ok(()));
    }

    #[test]
    fn a_fold_keeps_the_newest_goal_and_plan_and_the_derived_context_row() {
        use crate::ai::session_state::{
            Goal, PlanItem, PlanStatus, SessionState, StateAuthor, context_row, state_row,
        };
        let goal = |text: &str| {
            state_row(&SessionState::GoalSet {
                text: text.into(),
                author: StateAuthor::Person,
            })
        };
        let plan = |text: &str| {
            state_row(&SessionState::PlanSet {
                items: vec![PlanItem {
                    text: text.into(),
                    status: PlanStatus::InProgress,
                }],
            })
        };
        let context = |goal: &str, plan: &str| {
            context_row(
                Some(&Goal {
                    text: goal.into(),
                    author: StateAuthor::Person,
                }),
                Some(&[PlanItem {
                    text: plan.into(),
                    status: PlanStatus::InProgress,
                }]),
            )
            .unwrap()
        };
        let mut wire = vec![
            json!({"role":"system","content":"s"}),
            context("new goal", "new plan"),
            user("the first ask"),
            goal("old goal"),
            plan("old plan"),
            goal("new goal"),
            plan("new plan"),
        ];
        for round in 0..120 {
            native_round(&mut wire, &format!("c{round}"), 200);
        }
        wire.push(user("the current ask"));
        let ask = turn_ask(&wire);
        fold_wire(&mut wire, &FoldLimits::round(None), ask).unwrap();
        let kinds: Vec<&str> = wire
            .iter()
            .filter_map(|row| row["message_meta"]["state"]["kind"].as_str())
            .collect();
        assert_eq!(kinds, ["goal_set", "plan_set"]);
        let (goal, plan) = crate::ai::session_state::current_state(&wire);
        assert_eq!(goal.unwrap().text, "new goal");
        assert_eq!(plan.unwrap()[0].text, "new plan");
        let context_row = wire
            .iter()
            .find(|row| row["context"] == crate::ai::session_state::CONTEXT_STATE)
            .expect("the derived row survives the fold");
        assert_eq!(
            context_row["content"],
            "Goal: new goal\nPlan:\n1. [in progress] new plan"
        );
        assert!(context_row.get("message_meta").is_none());
        assert_eq!(turns::check_pairing(&wire[1..]), Ok(()));
    }

    #[test]
    fn a_fold_keeps_the_newest_tool_surface_notice() {
        use crate::ai::host_notice::{HostNotice, notice_row};
        let mut wire = vec![
            json!({"role":"system","content":"s"}),
            user("the first ask"),
            notice_row(&HostNotice::ToolSurface {
                added: vec!["tracks_add".into()],
                removed: Vec::new(),
                permission: None,
            }),
            notice_row(&HostNotice::ToolSurface {
                added: vec!["tracks_remove".into()],
                removed: vec!["tracks_add".into()],
                permission: None,
            }),
        ];
        for round in 0..120 {
            native_round(&mut wire, &format!("c{round}"), 200);
        }
        wire.push(user("the current ask"));
        let ask = turn_ask(&wire);
        fold_wire(&mut wire, &FoldLimits::round(None), ask).unwrap();
        let kinds: Vec<&str> = wire
            .iter()
            .filter_map(|row| row["message_meta"]["notice"]["kind"].as_str())
            .collect();
        assert_eq!(kinds, ["tool_surface"]);
        let kept = wire
            .iter()
            .find(|row| row["message_meta"]["notice"]["kind"] == "tool_surface")
            .expect("the newest surface is kept");
        assert_eq!(
            kept["message_meta"]["notice"]["removed"],
            json!(["tracks_add"])
        );
        assert_eq!(turns::check_pairing(&wire[1..]), Ok(()));
    }

    #[test]
    fn an_unknown_window_has_no_token_budget() {
        assert!(TokenBudget::new(0, 10).is_none());
        assert!(TokenBudget::new(1, 10).is_none());
        assert!(offered_tool_tokens(None, true) > offered_tool_tokens(None, false));
    }

    #[test]
    fn offered_tool_names_match_the_loop_request_for_each_selection() {
        let provider = crate::commands::AiProviderEntry {
            id: "wire-names".into(),
            name: "Wire names".into(),
            kind: crate::commands::ProviderKind::OpenAiChatCompletions,
            wire: Some(crate::commands::Wire::OpenAiChatCompletions),
            auth: Default::default(),
            auth_provider: None,
            oauth_allowed_hosts: Vec::new(),
            endpoint: "https://models.example/v1/chat/completions".into(),
            model: "third-party-model".into(),
            models: Vec::new(),
            context_length: 32_000,
            max_tokens: 262_144,
            temperature: 0.3,
            thinking_level: "medium".into(),
            capabilities: crate::commands::AiCapabilities {
                text: true,
                streaming: true,
                ..Default::default()
            },
        };
        for selection in [
            None,
            Some(vec!["ears".to_owned()]),
            Some(vec!["dsl-edit".to_owned(), "transport".to_owned()]),
        ] {
            let toolsets = selection.as_deref();
            // The loop sends the wire's request with the result pager attached;
            // build both from the same production call path.
            let (mut request, _) = crate::ai::request_from_wire(
                &provider,
                json!([{"role":"user","content":"hi"}]),
                true,
                toolsets,
            )
            .unwrap();
            request.tools.push(read_toolcall_spec());
            let mut built: Vec<String> = request.tools.into_iter().map(|tool| tool.name).collect();
            built.sort();
            built.dedup();
            assert_eq!(offered_tool_names(toolsets), built, "{selection:?}");
        }
    }
}
