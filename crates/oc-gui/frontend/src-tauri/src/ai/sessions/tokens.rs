//! The conversation size estimate shared by admission, compaction and the
//! token budget. One estimator owns the accounting of message text, tool
//! envelopes, reasoning and attachments.

use serde_json::Value;

/// An estimate, not a tokenizer: about four ASCII characters per token, one
/// token per non-ASCII character (CJK text tokenizes near one per character),
/// plus a small per-message overhead. Monotone in the messages it is given.
pub fn estimate_tokens(messages: &[Value]) -> u64 {
    messages.iter().map(message_tokens).sum()
}

fn message_tokens(message: &Value) -> u64 {
    let mut tokens = 4;
    if let Some(content) = message["content"].as_str() {
        tokens += text_tokens(content);
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        for call in calls {
            tokens += text_tokens(call["function"]["name"].as_str().unwrap_or_default());
            tokens += text_tokens(call["function"]["arguments"].as_str().unwrap_or_default());
        }
    }
    // Reasoning is replayed to the provider, so it occupies the context too.
    if let Some(thinking) = message["thinking"].as_array() {
        for entry in thinking {
            tokens += text_tokens(entry["text"].as_str().unwrap_or_default());
            tokens += text_tokens(entry["signature"].as_str().unwrap_or_default());
        }
    }
    tokens
}

/// Tokens of plain text under the conversation estimator, for callers that
/// size a prompt rather than a saved message.
pub fn estimate_text_tokens(text: &str) -> u64 {
    text_tokens(text)
}

fn text_tokens(text: &str) -> u64 {
    let mut units = 0u64;
    for character in text.chars() {
        units += if character.is_ascii() { 1 } else { 4 };
    }
    units.div_ceil(4)
}

/// Tokens of any serialized payload under the same rule as the conversation,
/// for parts of a request the store does not save, such as the tool list.
pub fn value_tokens(value: &Value) -> u64 {
    text_tokens(&value.to_string())
}

/// What one image costs a provider, about a full-resolution tile set. Images
/// carry no size the store can read, so every image counts the same.
pub const IMAGE_TOKENS: u64 = 1_600;
/// Audio input tokens per second of sound, a common provider rate.
pub const AUDIO_TOKENS_PER_SECOND: f64 = 32.0;
/// Bytes per second assumed for compressed audio whose duration is unknown.
const COMPRESSED_AUDIO_BYTES_PER_SECOND: u64 = 16_000;
/// Bytes per second of the 16-bit stereo 44.1 kHz WAV an inline clip carries.
const WAV_BYTES_PER_SECOND: u64 = 176_400;

/// The attachments a request carries, estimated apart from the text because
/// their cost depends on their kind and length, not on their descriptor. A
/// disk-only reference is not sent and costs nothing.
pub fn media_tokens(messages: &[Value]) -> u64 {
    let audio = |seconds: f64| (seconds.max(0.0) * AUDIO_TOKENS_PER_SECOND).ceil() as u64;
    let mut tokens = 0u64;
    for message in messages {
        for entry in message["media"].as_array().into_iter().flatten() {
            if entry["local_reference"].as_bool() == Some(true) {
                continue;
            }
            tokens += match entry["kind"].as_str() {
                Some("image") => IMAGE_TOKENS,
                _ => match entry["duration_secs"].as_f64().filter(|s| s.is_finite()) {
                    Some(seconds) => audio(seconds),
                    None => {
                        let rate = if entry["mime"] == "audio/wav" {
                            WAV_BYTES_PER_SECOND
                        } else {
                            COMPRESSED_AUDIO_BYTES_PER_SECOND
                        };
                        audio(entry["bytes"].as_u64().unwrap_or(0) as f64 / rate as f64)
                    }
                },
            };
        }
        if let Some(data) = message["input_audio"]["data"].as_str() {
            let bytes = data.len() as u64 / 4 * 3;
            tokens += audio(bytes as f64 / WAV_BYTES_PER_SECOND as f64);
        }
    }
    tokens
}
