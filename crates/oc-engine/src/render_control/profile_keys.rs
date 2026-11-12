//! Ordered InstrumentProfile key recognition shared with hosted preparation.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Recognized instrument-profile key kinds.
pub enum ProfileKeyKind {
    Manifest,
    ScalaRoot,
    ScalaFile,
    TunFile,
    KeyswitchDuration,
    OutputPair,
    MpeEnabled,
    MpeFirstChannel,
    MpeLastChannel,
    ArtRoundRobin,
    ArtKeyswitch,
    ArtCcValue,
    ArtCcReset,
    ArtCc,
    ArtParamValue,
    ArtParamReset,
    ArtParam,
    ArtDurationScale,
    ArtHumanizeTiming,
    ArtHumanizeVelocity,
    ArtReleaseNote,
    ArtReleaseVelocity,
    ArtReleaseDuration,
    ArtReleaseSustainAware,
    ArtOverlap,
    NoteTuning,
    ControlCc14,
    ControlCc,
    ControlParam,
    CcParam,
    ControlNoteExpression,
    ControlOnNote,
    ControlOffNote,
    ControlNoiseVelocity,
    ControlNoiseDuration,
    ControlNoiseThreshold,
    ControlRangeMin,
    ControlRangeMax,
    DrumNote,
    PadNote,
    DrumChokeGroup,
    DrumChokeRelease,
    ChokeRelease,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Classification outcome for a profile value.
pub enum ProfileValueStatus {
    /// Conversion is supported; native-name collision checks are still required.
    ConvertedCandidate,
    /// The parser needs an external resource; no I/O occurs during classification.
    DeferredResource,
    InvalidValue,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Classified profile assignment candidate.
pub struct ProfileAssignmentCandidate {
    pub kind: ProfileKeyKind,
    pub value_status: ProfileValueStatus,
}
#[derive(Clone, Copy)]
enum ValueKind {
    Number,
    U7,
    Note,
    Notes,
    Normalized,
    Boolean,
    String,
    Resource,
}
#[derive(Clone, Copy)]
enum Pattern {
    Exact(&'static [&'static str]),
    Affix {
        prefix: &'static str,
        suffix: &'static str,
        allow_empty: bool,
    },
}
#[derive(Clone, Copy)]
struct Rule {
    kind: ProfileKeyKind,
    pattern: Pattern,
    value: ValueKind,
}
const fn exact(kind: ProfileKeyKind, names: &'static [&'static str], value: ValueKind) -> Rule {
    Rule {
        kind,
        pattern: Pattern::Exact(names),
        value,
    }
}
const fn affix(
    kind: ProfileKeyKind,
    prefix: &'static str,
    suffix: &'static str,
    allow_empty: bool,
    value: ValueKind,
) -> Rule {
    Rule {
        kind,
        pattern: Pattern::Affix {
            prefix,
            suffix,
            allow_empty,
        },
        value,
    }
}
use ProfileKeyKind::*;
use ValueKind::*;
// Rule order is parser precedence. Some legacy aliases overlap by name and
// are distinguished only by the existing value conversion (e.g. cc14param).
const RULES: &[Rule] = &[
    exact(
        Manifest,
        &["profilemanifest", "profilepath", "instrumentprofile"],
        Resource,
    ),
    exact(
        ScalaRoot,
        &["scalarootnote", "tuningrootnote", "sclrootnote"],
        Note,
    ),
    exact(
        ScalaFile,
        &["scalafile", "sclfile", "tuningfile", "scalatuningfile"],
        Resource,
    ),
    exact(
        TunFile,
        &["tunfile", "anmarktunfile", "tuningtunfile"],
        Resource,
    ),
    exact(KeyswitchDuration, &["keyswitchdurationms"], Number),
    exact(
        OutputPair,
        &["outputpair", "pluginoutputpair", "micoutputpair"],
        Number,
    ),
    exact(MpeEnabled, &["mpe", "mpeenabled"], Boolean),
    exact(MpeFirstChannel, &["mpefirstchannel"], U7),
    exact(MpeLastChannel, &["mpelastchannel"], U7),
    affix(
        ArtRoundRobin,
        "articulation",
        "roundrobinkeyswitches",
        false,
        Notes,
    ),
    affix(ArtKeyswitch, "articulation", "keyswitch", false, Note),
    affix(ArtKeyswitch, "keyswitch", "", true, Note),
    affix(ArtCcValue, "articulation", "ccvalue", false, U7),
    affix(ArtCcReset, "articulation", "ccreset", false, U7),
    affix(ArtCc, "articulation", "cc", false, U7),
    affix(
        ArtParamValue,
        "articulation",
        "paramvalue",
        false,
        Normalized,
    ),
    affix(
        ArtParamReset,
        "articulation",
        "paramreset",
        false,
        Normalized,
    ),
    affix(ArtParam, "articulation", "param", false, String),
    affix(
        ArtDurationScale,
        "articulation",
        "durationscale",
        false,
        Number,
    ),
    affix(
        ArtHumanizeTiming,
        "articulation",
        "humanizetimingms",
        false,
        Number,
    ),
    affix(
        ArtHumanizeVelocity,
        "articulation",
        "humanizevelocity",
        false,
        U7,
    ),
    affix(ArtReleaseNote, "articulation", "releasenote", false, Note),
    affix(
        ArtReleaseVelocity,
        "articulation",
        "releasevelocity",
        false,
        U7,
    ),
    affix(
        ArtReleaseDuration,
        "articulation",
        "releasedurationms",
        false,
        Number,
    ),
    affix(
        ArtReleaseSustainAware,
        "articulation",
        "releasesustainaware",
        false,
        Boolean,
    ),
    affix(ArtOverlap, "articulation", "overlapms", false, Number),
    affix(NoteTuning, "microtuning", "cents", false, Number),
    affix(NoteTuning, "tuning", "cents", false, Number),
    affix(ControlCc14, "control", "cc14", false, U7),
    affix(ControlCc14, "cc14", "", false, U7),
    affix(ControlCc, "control", "cc", false, U7),
    affix(ControlCc, "cc", "", true, U7),
    affix(ControlParam, "control", "param", false, String),
    affix(ControlParam, "param", "", true, String),
    affix(CcParam, "cc", "param", false, String),
    affix(
        ControlNoteExpression,
        "control",
        "noteexpression",
        false,
        String,
    ),
    affix(ControlOnNote, "control", "onnote", false, Note),
    affix(ControlOffNote, "control", "offnote", false, Note),
    affix(ControlNoiseVelocity, "control", "noisevelocity", false, U7),
    affix(
        ControlNoiseDuration,
        "control",
        "noisedurationms",
        false,
        Number,
    ),
    affix(
        ControlNoiseThreshold,
        "control",
        "noisethreshold",
        false,
        Normalized,
    ),
    affix(ControlRangeMin, "control", "rangemin", false, Normalized),
    affix(ControlRangeMax, "control", "rangemax", false, Normalized),
    affix(DrumNote, "drum", "note", false, Note),
    affix(PadNote, "pad", "note", false, Note),
    affix(DrumChokeGroup, "drum", "chokegroup", false, String),
    affix(DrumChokeRelease, "drum", "chokereleasems", false, Number),
    affix(ChokeRelease, "choke", "releasems", false, Number),
];

#[derive(Clone, Copy)]
pub(super) struct ProfileKeys<'a> {
    normalized: &'a str,
}
#[derive(Clone, Copy)]
struct KeyMatch<'a> {
    rule: Rule,
    selector: &'a str,
}
impl KeyMatch<'_> {
    fn accepts(self, value: &ParamValue) -> bool {
        match self.rule.value {
            Number => param_value_as_f64(value).is_some(),
            U7 => param_value_as_u7(value).is_some(),
            Note => param_value_as_midi_note(value).is_some(),
            Notes => param_value_as_midi_note_list(value).is_some(),
            Normalized => param_value_as_normalized(value).is_some(),
            Boolean => param_value_as_bool(value).is_some(),
            String | Resource => matches!(value, ParamValue::String(_)),
        }
    }
    fn candidate(self, value: &ParamValue) -> ProfileAssignmentCandidate {
        let status = if !self.accepts(value) {
            ProfileValueStatus::InvalidValue
        } else if matches!(self.rule.value, Resource) {
            ProfileValueStatus::DeferredResource
        } else {
            let finite = match value {
                ParamValue::Number(n)
                | ParamValue::Db(n)
                | ParamValue::Percentage(n)
                | ParamValue::Frequency(n)
                | ParamValue::Time(n)
                | ParamValue::Cents(n) => n.is_finite(),
                ParamValue::String(_) if matches!(self.rule.value, Number | U7 | Normalized) => {
                    param_value_as_f64(value).is_some_and(f64::is_finite)
                }
                _ => true,
            };
            if finite {
                ProfileValueStatus::ConvertedCandidate
            } else {
                ProfileValueStatus::InvalidValue
            }
        };
        ProfileAssignmentCandidate {
            kind: self.rule.kind,
            value_status: status,
        }
    }
}
impl<'a> ProfileKeys<'a> {
    pub(super) fn new(normalized: &'a str) -> Self {
        Self { normalized }
    }
    fn matches(self) -> impl Iterator<Item = KeyMatch<'a>> {
        RULES.iter().copied().filter_map(move |rule| {
            let selector = match rule.pattern {
                Pattern::Exact(names) => names.contains(&self.normalized).then_some("")?,
                Pattern::Affix {
                    prefix,
                    suffix,
                    allow_empty,
                } => {
                    let target = self.normalized.strip_prefix(prefix)?.strip_suffix(suffix)?;
                    if !allow_empty && target.is_empty() {
                        return None;
                    }
                    target
                }
            };
            if rule.kind == CcParam && selector.parse::<u8>().is_err() {
                return None;
            }
            if rule.kind == NoteTuning && parse_midi_note(selector).is_none() {
                return None;
            }
            Some(KeyMatch { rule, selector })
        })
    }
    pub(super) fn contains(self, kind: ProfileKeyKind) -> bool {
        self.target(kind).is_some()
    }
    pub(super) fn target(self, kind: ProfileKeyKind) -> Option<&'a str> {
        self.matches()
            .find(|entry| entry.rule.kind == kind)
            .map(|entry| entry.selector)
    }
    pub(super) fn controller(self) -> Option<u8> {
        self.target(CcParam)?.parse().ok()
    }
    fn candidate(self, value: &ParamValue) -> Option<ProfileAssignmentCandidate> {
        let first = self.matches().next()?;
        // Exact scalar/path branches consume their key even when conversion or
        // external-resource loading fails; legacy prefix fallbacks cannot win.
        if matches!(first.rule.pattern, Pattern::Exact(_)) {
            return Some(first.candidate(value));
        }
        Some(
            self.matches()
                .find(|entry| entry.accepts(value))
                .unwrap_or(first)
                .candidate(value),
        )
    }
}

/// This is a candidate for the profile consumer, not a reserved native key.
/// Activation must resolve native inventory collisions before consuming it.
pub fn classify_profile_assignment(
    name: &str,
    value: &ParamValue,
) -> Option<ProfileAssignmentCandidate> {
    let normalized = normalize_key(name);
    ProfileKeys::new(&normalized).candidate(value)
}

#[cfg(test)]
mod tests;
