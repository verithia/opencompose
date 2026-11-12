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
}

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
        }
    }
}

impl SampleQuery {
    /// Page size actually requested. The bound keeps one call's JSON small.
    pub fn page_size(&self) -> usize {
        self.limit.clamp(1, 50)
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
    Failed(String),
}

impl SampleError {
    /// Build a structured refusal with a stable code and a user sentence.
    pub fn refused(code: &'static str, message: impl Into<String>) -> Self {
        SampleError::Refused {
            code,
            message: message.into(),
        }
    }

    /// The refusal code, or `None` for an unexpected failure.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            SampleError::Refused { code, .. } => Some(code),
            SampleError::Failed(_) => None,
        }
    }

    /// The sentence shown for this failure.
    pub fn message(&self) -> &str {
        match self {
            SampleError::Refused { message, .. } => message,
            SampleError::Failed(message) => message,
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
            SampleError::Failed(message) => serde_json::json!({
                "status": "failed",
                "message": message,
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
        assert_eq!(
            SampleError::Failed("offline".into()).to_json()["status"],
            "failed"
        );
    }
}
