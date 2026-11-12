//! Typed physical and normalized domains for VST3 standard note expressions.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Standard VST3 note-expression types.
pub enum Vst3StandardNoteExpression {
    Volume,
    Pan,
    Tuning,
    Vibrato,
    Expression,
    Brightness,
}
impl Vst3StandardNoteExpression {
    /// Parse a note-expression name.
    pub fn from_name(name: &str) -> Option<Self> {
        match super::normalize_param_name(name).as_str() {
            "volume" => Some(Self::Volume),
            "pan" => Some(Self::Pan),
            "tuning" | "pitch" => Some(Self::Tuning),
            "vibrato" => Some(Self::Vibrato),
            "expression" => Some(Self::Expression),
            "brightness" | "timbre" => Some(Self::Brightness),
            _ => None,
        }
    }
    /// Native VST3 type id.
    pub fn native_type_id(self) -> u32 {
        match self {
            Self::Volume => 0,
            Self::Pan => 1,
            Self::Tuning => 2,
            Self::Vibrato => 3,
            Self::Expression => 4,
            Self::Brightness => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// A note-expression value in its native domain.
pub enum Vst3NoteExpressionValue {
    Normalized(f64),
    TuningSemitones(f64),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Why a note-expression value was refused.
pub enum Vst3NoteExpressionError {
    InvalidAddress,
    NonFinite,
    OutOfRange,
    DomainMismatch,
}
impl std::fmt::Display for Vst3NoteExpressionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidAddress => "Invalid VST3 note-expression channel, key or sample offset",
            Self::NonFinite => "VST3 note-expression value must be finite",
            Self::OutOfRange => "VST3 note-expression value is outside its declared domain",
            Self::DomainMismatch => "Physical tuning requires the VST3 tuning expression",
        })
    }
}
impl std::error::Error for Vst3NoteExpressionError {}
impl Vst3NoteExpressionValue {
    /// The SDK tuning domain is ±120 semitones, centered at normalized 0.5.
    /// Other standard expressions and explicitly normalized tuning stay in `[0,1]`.
    pub fn normalized_for(
        self,
        kind: Vst3StandardNoteExpression,
    ) -> Result<f64, Vst3NoteExpressionError> {
        let (value, low, high) = match self {
            Self::Normalized(value) => (value, 0.0, 1.0),
            Self::TuningSemitones(value) => {
                if kind != Vst3StandardNoteExpression::Tuning {
                    return Err(Vst3NoteExpressionError::DomainMismatch);
                }
                (value, -120.0, 120.0)
            }
        };
        if !value.is_finite() {
            return Err(Vst3NoteExpressionError::NonFinite);
        }
        if !(low..=high).contains(&value) {
            return Err(Vst3NoteExpressionError::OutOfRange);
        }
        Ok(match self {
            Self::Normalized(_) => value,
            Self::TuningSemitones(_) => 0.5 + value / 240.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_tuning_and_normalized_tuning_have_distinct_domains() {
        for semitones in [-120.0, -12.0, -0.5, 0.0, 0.5, 12.0, 120.0] {
            let normalized = Vst3NoteExpressionValue::TuningSemitones(semitones)
                .normalized_for(Vst3StandardNoteExpression::Tuning)
                .unwrap();
            assert!((240.0 * (normalized - 0.5) - semitones).abs() < 2e-14);
        }
        assert_eq!(
            Vst3NoteExpressionValue::TuningSemitones(0.0)
                .normalized_for(Vst3StandardNoteExpression::Tuning),
            Ok(0.5)
        );
        assert_eq!(
            Vst3NoteExpressionValue::Normalized(0.25)
                .normalized_for(Vst3StandardNoteExpression::Tuning),
            Ok(0.25)
        );
        assert_eq!(
            Vst3NoteExpressionValue::Normalized(0.25)
                .normalized_for(Vst3StandardNoteExpression::Pan),
            Ok(0.25)
        );
        assert_eq!(
            Vst3StandardNoteExpression::from_name("pitch"),
            Some(Vst3StandardNoteExpression::Tuning)
        );
    }
    #[test]
    fn invalid_expression_domains_refuse_without_clamping() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                Vst3NoteExpressionValue::TuningSemitones(value)
                    .normalized_for(Vst3StandardNoteExpression::Tuning),
                Err(Vst3NoteExpressionError::NonFinite)
            );
        }
        for value in [-120.01, 120.01] {
            assert_eq!(
                Vst3NoteExpressionValue::TuningSemitones(value)
                    .normalized_for(Vst3StandardNoteExpression::Tuning),
                Err(Vst3NoteExpressionError::OutOfRange)
            );
        }
        assert_eq!(
            Vst3NoteExpressionValue::Normalized(1.01)
                .normalized_for(Vst3StandardNoteExpression::Tuning),
            Err(Vst3NoteExpressionError::OutOfRange)
        );
        assert_eq!(
            Vst3NoteExpressionValue::TuningSemitones(1.0)
                .normalized_for(Vst3StandardNoteExpression::Pan),
            Err(Vst3NoteExpressionError::DomainMismatch)
        );
    }
}
