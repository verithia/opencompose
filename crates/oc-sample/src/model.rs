//! Catalogue types shared by every provider: what a search asks for, what one
//! result carries, and how a refusal is shaped.

use serde::{Deserialize, Serialize};

/// The kinds a search can ask for. A provider turns each into its own query
/// vocabulary; a result reports the kind it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleKind {
    DrumOneShot,
    Loop,
    Vocal,
    Fx,
    InstrumentPhrase,
    Unknown,
}

impl SampleKind {
    /// Every kind, in the order the filters present them.
    pub const ALL: [SampleKind; 6] = [
        SampleKind::DrumOneShot,
        SampleKind::Loop,
        SampleKind::Vocal,
        SampleKind::Fx,
        SampleKind::InstrumentPhrase,
        SampleKind::Unknown,
    ];

    /// The wire spelling of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            SampleKind::DrumOneShot => "drum_one_shot",
            SampleKind::Loop => "loop",
            SampleKind::Vocal => "vocal",
            SampleKind::Fx => "fx",
            SampleKind::InstrumentPhrase => "instrument_phrase",
            SampleKind::Unknown => "unknown",
        }
    }

    /// Parse a kind written by a tool argument or on a command line. The short
    /// forms are what a user types; the wire form is accepted too.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "drum" | "drums" | "drum_one_shot" | "one-shot" | "one_shot" | "oneshot" => {
                Some(SampleKind::DrumOneShot)
            }
            "loop" | "loops" => Some(SampleKind::Loop),
            "vocal" | "vocals" | "voice" => Some(SampleKind::Vocal),
            "fx" | "effect" | "effects" | "sound_effect" => Some(SampleKind::Fx),
            "instrument" | "instrument_phrase" | "phrase" | "melodic" => {
                Some(SampleKind::InstrumentPhrase)
            }
            "any" | "unknown" | "" => Some(SampleKind::Unknown),
            _ => None,
        }
    }

    /// The words a provider adds to its text query to bias a search.
    pub fn search_terms(self) -> &'static [&'static str] {
        match self {
            SampleKind::DrumOneShot => &["drum", "percussion"],
            SampleKind::Loop => &["loop"],
            SampleKind::Vocal => &["vocal", "voice"],
            SampleKind::Fx => &["sound effect"],
            SampleKind::InstrumentPhrase => &["musical instrument"],
            SampleKind::Unknown => &[],
        }
    }
}

impl std::fmt::Display for SampleKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Classify one result from its name and tags. The order matters: a drum loop
/// is a loop, and a vocal loop is a loop as well.
pub fn classify(name: &str, tags: &[String]) -> SampleKind {
    let mut haystack = name.to_lowercase();
    for tag in tags {
        haystack.push(' ');
        haystack.push_str(&tag.to_lowercase());
    }
    let has = |words: &[&str]| words.iter().any(|word| haystack.contains(word));
    if has(&["loop"]) {
        SampleKind::Loop
    } else if has(&[
        "drum",
        "snare",
        "kick",
        "hi-hat",
        "hihat",
        "cymbal",
        "tom-tom",
        "percussion",
        "clap",
        "conga",
        "bongo",
        "timpani",
        "tambourine",
    ]) {
        SampleKind::DrumOneShot
    } else if has(&["vocal", "voice", "choir", "singing", "chant", "spoken"]) {
        SampleKind::Vocal
    } else if has(&["sound effect", "sfx", "foley", "noise"]) {
        SampleKind::Fx
    } else if has(&[
        "piano",
        "guitar",
        "violin",
        "viola",
        "cello",
        "bass",
        "flute",
        "oboe",
        "clarinet",
        "bassoon",
        "saxophone",
        "trumpet",
        "trombone",
        "tuba",
        "harp",
        "organ",
        "synthesizer",
        "instrument",
    ]) {
        SampleKind::InstrumentPhrase
    } else {
        SampleKind::Unknown
    }
}

/// One search. Providers read the fields they support and apply the rest to
/// the results they got back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleQuery {
    pub text: String,
    pub kind: Option<SampleKind>,
    /// Inclusive lower bound on [`SampleItem::duration_seconds`].
    pub min_seconds: Option<f64>,
    /// Inclusive upper bound on [`SampleItem::duration_seconds`].
    pub max_seconds: Option<f64>,
    pub channels: Option<u16>,
    pub tags: Vec<String>,
    pub offset: usize,
    pub limit: usize,
    /// Keep only results whose licence allows commercial use. Absent on the
    /// wire means no licence filter, which is what an older caller meant.
    #[serde(default)]
    pub commercial_ok: bool,
}

/// Longest search text, in characters, a provider receives. Search boxes
/// index words, so a pasted paragraph only makes the request URL huge.
pub const MAX_QUERY_CHARS: usize = 200;
/// Most tags one search sends. Each tag narrows the result set, so a longer
/// list mostly produces empty pages while growing the URL.
pub const MAX_QUERY_TAGS: usize = 8;
/// Longest single tag, in characters.
pub const MAX_TAG_CHARS: usize = 64;

impl Default for SampleQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            kind: None,
            min_seconds: None,
            max_seconds: None,
            channels: None,
            tags: Vec::new(),
            offset: 0,
            limit: 20,
            commercial_ok: false,
        }
    }
}

/// Collapse every whitespace run to one space, trim the ends and keep at most
/// `max_chars` characters.
fn squash(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() >= max_chars {
            break;
        }
    }
    match out.char_indices().nth(max_chars) {
        Some((end, _)) => out[..end].trim_end().to_string(),
        None => out,
    }
}

impl SampleQuery {
    /// Page size actually requested. The bound keeps one call's JSON small.
    pub fn page_size(&self) -> usize {
        self.limit.clamp(1, 50)
    }

    /// The query with its text and tags in the shape every provider expects:
    /// whitespace collapsed, empty and repeated tags dropped, and the text and
    /// tag list bounded. Providers build their URLs from this form only.
    pub fn normalized(&self) -> SampleQuery {
        let mut tags: Vec<String> = Vec::new();
        for tag in &self.tags {
            if tags.len() >= MAX_QUERY_TAGS {
                break;
            }
            let tag = squash(tag, MAX_TAG_CHARS);
            if !tag.is_empty() && !tags.iter().any(|seen| seen.eq_ignore_ascii_case(&tag)) {
                tags.push(tag);
            }
        }
        SampleQuery {
            text: squash(&self.text, MAX_QUERY_CHARS),
            tags,
            ..self.clone()
        }
    }

    /// Drop the items this query rejects, keeping the order of the rest.
    pub fn retain_matching(&self, items: &mut Vec<SampleItem>) {
        items.retain(|item| self.matches(item));
    }

    /// Whether one result satisfies the bounds the provider could not express
    /// in its own query. A result with no known duration fails a duration
    /// bound rather than passing on a guess.
    pub fn matches(&self, item: &SampleItem) -> bool {
        if let Some(kind) = self.kind
            && kind != SampleKind::Unknown
            && item.kind != kind
        {
            return false;
        }
        if self.min_seconds.is_some() || self.max_seconds.is_some() {
            let Some(duration) = item.duration_seconds else {
                return false;
            };
            if self.min_seconds.is_some_and(|min| duration < min) {
                return false;
            }
            if self.max_seconds.is_some_and(|max| duration > max) {
                return false;
            }
        }
        if let Some(channels) = self.channels
            && item.channels != Some(channels)
        {
            return false;
        }
        if self.commercial_ok && !item.license.allows_commercial_use() {
            return false;
        }
        true
    }
}

/// Whether the file behind [`SampleItem::download_url`] is the source's
/// original or a derived preview. The distinction travels into the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Original,
    PreviewMp3,
}

impl Quality {
    /// The wire spelling of this quality.
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Original => "original",
            Quality::PreviewMp3 => "preview_mp3",
        }
    }
}

/// One searchable sample. Every field a provider cannot supply is `None`
/// rather than a guessed value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleItem {
    /// Provider-scoped and stable, so `pull` can be called with the id alone.
    pub id: String,
    pub provider: String,
    pub name: String,
    pub kind: SampleKind,
    pub duration_seconds: Option<f64>,
    pub channels: Option<u16>,
    pub sample_rate: Option<u32>,
    pub license: crate::license::License,
    /// The credit line a project must carry while the file is in it.
    pub attribution: String,
    /// The page a person opens; `download_url` is the file itself.
    pub source_url: String,
    pub preview_url: Option<String>,
    pub download_url: String,
    /// Declared sha256 of `download_url`, verified before the file lands.
    pub sha256: Option<String>,
    /// Declared size of `download_url`, verified before the file lands.
    pub bytes: Option<u64>,
    pub quality: Quality,
    pub tags: Vec<String>,
}

impl SampleItem {
    /// Whether the licence rule lets this item into a project.
    pub fn license_ok(&self) -> bool {
        self.license.check().is_ok()
    }
}

/// One page of results plus where the next page starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SamplePage {
    pub provider: String,
    pub offset: usize,
    pub limit: usize,
    /// `None` when the source does not report a total.
    pub total: Option<u64>,
    /// `None` when this was the last page.
    pub next_offset: Option<usize>,
    pub items: Vec<SampleItem>,
}

/// Why a source could not be read. The kind decides what a caller does next
/// (wait, ask for a key, retry later, give up), so it travels as a tag and
/// never has to be read back out of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// HTTP 429: the source asks the caller to slow down.
    RateLimited,
    /// HTTP 401 or 403: the key or token is missing, wrong or expired.
    Unauthorized,
    /// HTTP 404 or 410: the record or file is gone.
    NotFound,
    /// HTTP 5xx: the source is up but not answering properly.
    Unavailable,
    /// The request did not finish inside the client's deadline.
    Timeout,
    /// The connection could not be made or broke mid-body.
    Network,
    /// The source answered with a body this crate cannot read.
    Parse,
    /// Anything else, including a kind a newer helper reports that this
    /// build does not know.
    #[serde(other)]
    Other,
}

impl FailureKind {
    /// The wire spelling of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::RateLimited => "rate_limited",
            FailureKind::Unauthorized => "unauthorized",
            FailureKind::NotFound => "not_found",
            FailureKind::Unavailable => "unavailable",
            FailureKind::Timeout => "timeout",
            FailureKind::Network => "network",
            FailureKind::Parse => "parse",
            FailureKind::Other => "other",
        }
    }
}

/// Longest `Retry-After` this crate repeats. A source asking for more than a
/// day is treated as asking for a day; callers apply their own tighter cap.
pub const MAX_RETRY_AFTER_SECS: u64 = 24 * 60 * 60;

/// A failure with its kind, the HTTP status when there was one, and the wait
/// the source asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub status: Option<u16>,
    /// Seconds from `Retry-After`, bounded by [`MAX_RETRY_AFTER_SECS`].
    pub retry_after_secs: Option<u64>,
    pub message: String,
}

impl Failure {
    /// A failure of `kind` with no status and no requested wait.
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            retry_after_secs: None,
            message: message.into(),
        }
    }
}

/// A refusal the caller can act on, or a failure that changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SampleError {
    /// The call was understood and refused for a stated reason.
    Refused {
        /// Stable machine code: `key_required`, `license_unsupported`, ...
        code: &'static str,
        message: String,
    },
    /// The source could not be read; nothing was written.
    Failed(Failure),
}

impl SampleError {
    /// Build a structured refusal with a stable code and a user sentence.
    pub fn refused(code: &'static str, message: impl Into<String>) -> Self {
        SampleError::Refused {
            code,
            message: message.into(),
        }
    }

    /// A failure whose cause has no more specific kind.
    pub fn failed(message: impl Into<String>) -> Self {
        SampleError::Failed(Failure::new(FailureKind::Other, message))
    }

    /// A failure of a known kind.
    pub fn failure(kind: FailureKind, message: impl Into<String>) -> Self {
        SampleError::Failed(Failure::new(kind, message))
    }

    /// A transport failure, prefixed with the source that caused it.
    pub fn from_fetch(source: &str, error: crate::provider::FetchError) -> Self {
        SampleError::Failed(Failure {
            kind: error.kind,
            status: error.status,
            retry_after_secs: error.retry_after_secs,
            message: format!("{source}: {}", error.message),
        })
    }

    /// The refusal code, or `None` for an unexpected failure.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            SampleError::Refused { code, .. } => Some(code),
            SampleError::Failed(_) => None,
        }
    }

    /// The failure details, or `None` for a refusal.
    pub fn failure_detail(&self) -> Option<&Failure> {
        match self {
            SampleError::Refused { .. } => None,
            SampleError::Failed(failure) => Some(failure),
        }
    }

    /// The failure kind, or `None` for a refusal.
    pub fn failure_kind(&self) -> Option<FailureKind> {
        self.failure_detail().map(|failure| failure.kind)
    }

    /// The sentence shown for this failure.
    pub fn message(&self) -> &str {
        match self {
            SampleError::Refused { message, .. } => message,
            SampleError::Failed(failure) => &failure.message,
        }
    }

    /// The structured form a tool returns: a refusal is data, not an error.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            SampleError::Refused { code, message } => serde_json::json!({
                "status": "refused",
                "code": code,
                "message": message,
            }),
            SampleError::Failed(failure) => serde_json::json!({
                "status": "failed",
                "kind": failure.kind,
                "http_status": failure.status,
                "retry_after_secs": failure.retry_after_secs,
                "message": failure.message,
            }),
        }
    }
}

impl std::fmt::Display for SampleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for SampleError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn a_kind_parses_from_the_words_a_user_types() {
        assert_eq!(SampleKind::parse("drum"), Some(SampleKind::DrumOneShot));
        assert_eq!(SampleKind::parse("one-shot"), Some(SampleKind::DrumOneShot));
        assert_eq!(
            SampleKind::parse(" Instrument "),
            Some(SampleKind::InstrumentPhrase)
        );
        assert_eq!(SampleKind::parse("vocal"), Some(SampleKind::Vocal));
        assert_eq!(SampleKind::parse("any"), Some(SampleKind::Unknown));
        assert_eq!(SampleKind::parse("banjo"), None);
    }

    #[test]
    fn a_loop_wins_over_the_instrument_in_its_name() {
        assert_eq!(
            classify("Drum loop 90 bpm", &tags(&["drums"])),
            SampleKind::Loop
        );
        assert_eq!(
            classify("Snare drum unmuffled", &tags(&["Sounds of drums"])),
            SampleKind::DrumOneShot
        );
        assert_eq!(classify("Choir ahh", &[]), SampleKind::Vocal);
        assert_eq!(
            classify("Door slam", &tags(&["sound effect"])),
            SampleKind::Fx
        );
        assert_eq!(
            classify("Cello pizzicato", &[]),
            SampleKind::InstrumentPhrase
        );
        assert_eq!(classify("Field recording", &[]), SampleKind::Unknown);
    }

    #[test]
    fn a_query_applies_the_bounds_a_provider_cannot_express() {
        let item = SampleItem {
            id: "x".into(),
            provider: "test".into(),
            name: "Snare".into(),
            kind: SampleKind::DrumOneShot,
            duration_seconds: Some(1.5),
            channels: Some(2),
            sample_rate: Some(44_100),
            license: crate::license::License::cc0(),
            attribution: String::new(),
            source_url: String::new(),
            preview_url: None,
            download_url: String::new(),
            sha256: None,
            bytes: None,
            quality: Quality::Original,
            tags: Vec::new(),
        };
        let query = SampleQuery {
            kind: Some(SampleKind::DrumOneShot),
            max_seconds: Some(2.0),
            channels: Some(2),
            ..Default::default()
        };
        assert!(query.matches(&item));
        assert!(
            !SampleQuery {
                max_seconds: Some(1.0),
                ..query.clone()
            }
            .matches(&item)
        );
        assert!(
            !SampleQuery {
                channels: Some(1),
                ..query.clone()
            }
            .matches(&item)
        );
        assert!(
            !SampleQuery {
                kind: Some(SampleKind::Loop),
                ..query.clone()
            }
            .matches(&item)
        );
        // An unknown duration cannot satisfy a duration bound.
        let mut unknown = item.clone();
        unknown.duration_seconds = None;
        assert!(!query.matches(&unknown));
    }

    #[test]
    fn a_page_size_is_bounded() {
        assert_eq!(
            SampleQuery {
                limit: 0,
                ..Default::default()
            }
            .page_size(),
            1
        );
        assert_eq!(
            SampleQuery {
                limit: 500,
                ..Default::default()
            }
            .page_size(),
            50
        );
    }

    #[test]
    fn a_refusal_serialises_as_data() {
        let error = SampleError::refused("key_required", "Set the key.");
        let value = error.to_json();
        assert_eq!(value["status"], "refused");
        assert_eq!(value["code"], "key_required");
        assert_eq!(value["message"], "Set the key.");
        assert_eq!(SampleError::failed("offline").to_json()["status"], "failed");
    }

    #[test]
    fn a_failure_serialises_its_kind_status_and_wait() {
        let error = SampleError::Failed(Failure {
            kind: FailureKind::RateLimited,
            status: Some(429),
            retry_after_secs: Some(30),
            message: "slow down".into(),
        });
        let value = error.to_json();
        assert_eq!(value["kind"], "rate_limited");
        assert_eq!(value["http_status"], 429);
        assert_eq!(value["retry_after_secs"], 30);
        assert_eq!(error.failure_kind(), Some(FailureKind::RateLimited));
        assert_eq!(error.code(), None);
        assert_eq!(SampleError::refused("x", "y").failure_kind(), None);
    }

    #[test]
    fn every_failure_kind_has_one_wire_spelling() {
        for kind in [
            FailureKind::RateLimited,
            FailureKind::Unauthorized,
            FailureKind::NotFound,
            FailureKind::Unavailable,
            FailureKind::Timeout,
            FailureKind::Network,
            FailureKind::Parse,
            FailureKind::Other,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
            let back: FailureKind = serde_json::from_value(kind.as_str().into()).unwrap();
            assert_eq!(back, kind);
        }
        // A kind a newer helper invents reads as `other`, not as an error.
        let future: FailureKind = serde_json::from_str("\"quota_exhausted\"").unwrap();
        assert_eq!(future, FailureKind::Other);
    }

    #[test]
    fn a_normalized_query_collapses_whitespace_and_bounds_text_and_tags() {
        let query = SampleQuery {
            text: "  snare \t\n  drum  ".into(),
            tags: vec![
                " acoustic  kit ".into(),
                "".into(),
                "ACOUSTIC KIT".into(),
                "  ".into(),
            ],
            ..Default::default()
        }
        .normalized();
        assert_eq!(query.text, "snare drum");
        assert_eq!(query.tags, vec!["acoustic kit".to_string()]);

        let long = SampleQuery {
            text: "é".repeat(MAX_QUERY_CHARS * 3),
            tags: (0..MAX_QUERY_TAGS * 2).map(|n| format!("t{n}")).collect(),
            ..Default::default()
        }
        .normalized();
        assert_eq!(long.text.chars().count(), MAX_QUERY_CHARS);
        assert_eq!(long.tags.len(), MAX_QUERY_TAGS);
        let tag = SampleQuery {
            tags: vec!["x".repeat(MAX_TAG_CHARS + 10)],
            ..Default::default()
        }
        .normalized();
        assert_eq!(tag.tags[0].chars().count(), MAX_TAG_CHARS);
    }

    #[test]
    fn a_commercial_filter_drops_non_commercial_and_unclassified_licences() {
        let mut item = SampleItem {
            id: "x".into(),
            provider: "test".into(),
            name: "Pad".into(),
            kind: SampleKind::Unknown,
            duration_seconds: None,
            channels: None,
            sample_rate: None,
            license: crate::license::License::cc0(),
            attribution: String::new(),
            source_url: String::new(),
            preview_url: None,
            download_url: String::new(),
            sha256: None,
            bytes: None,
            quality: Quality::Original,
            tags: Vec::new(),
        };
        let query = SampleQuery {
            commercial_ok: true,
            ..Default::default()
        };
        assert!(query.matches(&item));
        item.license.non_commercial = true;
        assert!(!query.matches(&item));
        assert!(
            SampleQuery::default().matches(&item),
            "no filter by default"
        );
        item.license = crate::license::License::unknown("Some licence");
        assert!(!query.matches(&item));

        let mut items = vec![item.clone()];
        query.retain_matching(&mut items);
        assert!(items.is_empty());
    }

    #[test]
    fn a_query_without_the_licence_field_reads_as_unfiltered() {
        let query: SampleQuery = serde_json::from_value(serde_json::json!({
            "text": "snare",
            "kind": null,
            "min_seconds": null,
            "max_seconds": null,
            "channels": null,
            "tags": [],
            "offset": 0,
            "limit": 5
        }))
        .unwrap();
        assert!(!query.commercial_ok);
    }
}
