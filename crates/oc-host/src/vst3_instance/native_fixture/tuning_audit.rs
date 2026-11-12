//! The optimized native processor decodes normalized tuning into pitch ratios.
use super::*;
use std::mem::{align_of, size_of};

#[link(name = "oc_vst3_guard_fixture", kind = "static")]
unsafe extern "C" {
    fn oc_vst3_fixture_event_layout(index: u32) -> usize;
}

#[derive(Debug)]
/// Coverage of the VST3 tuning payload layout.
pub struct Vst3TuningPayloadAudit {
    pub physical_cases: usize,
    pub normalized_cases: usize,
}

/// Audit the VST3 tuning payload layout.
pub fn audit_tuning_payload() -> Result<Vst3TuningPayloadAudit, String> {
    let expected = [
        size_of::<Event>(),
        align_of::<Event>(),
        std::mem::offset_of!(Event, __field0),
        size_of::<NoteExpressionValueEvent>(),
        std::mem::offset_of!(NoteExpressionValueEvent, value),
        size_of::<vst3::Steinberg::Vst::IEventListVtbl>(),
        std::mem::offset_of!(vst3::Steinberg::Vst::IEventListVtbl, getEventCount),
        std::mem::offset_of!(vst3::Steinberg::Vst::IEventListVtbl, getEvent),
    ];
    for (index, expected) in expected.into_iter().enumerate() {
        assert_eq!(
            // SAFETY: probe returns only compile-time C++ event layout constants.
            unsafe { oc_vst3_fixture_event_layout(index as u32) },
            expected,
        );
    }
    for semitones in [-12.0, -0.5, 0.0, 0.5, 12.0] {
        let mut fixture = NativeFixture::new(false);
        fixture.configure_parameters_for_test(10);
        let instance = fixture.instance_mut();
        instance.note_on_channel(2, 64, 0.5, 3);
        // The shared engine event contract expresses tuning/pitch in semitones.
        instance.note_expression(2, 64, "pitch", semitones, 5);
        let pcm = instance.process_block(16);
        let expected_ratio = 2.0f64.powf(semitones / 12.0);
        assert!(
            (f64::from(pcm[0]) - expected_ratio).abs() < 2e-6,
            "physical tuning {semitones}"
        );
        assert!((f64::from(pcm[1]) - 2.0 * expected_ratio).abs() < 4e-6);
        assert!(!instance.failed());
    }
    for normalized in [0.45, 0.5, 0.55] {
        let mut fixture = NativeFixture::new(false);
        fixture.configure_parameters_for_test(10);
        let instance = fixture.instance_mut();
        instance.note_on_channel(2, 64, 0.5, 3);
        instance
            .queue_note_expression(
                2,
                64,
                Vst3StandardNoteExpression::Tuning,
                Vst3NoteExpressionValue::Normalized(normalized),
                5,
            )
            .map_err(|error| error.to_string())?;
        let pcm = instance.process_block(16);
        let semitones = 240.0 * (normalized - 0.5);
        assert!((f64::from(pcm[0]) - 2.0f64.powf(semitones / 12.0)).abs() < 2e-6);
        assert!(!instance.failed());
    }
    let mut fixture = NativeFixture::new(false);
    let instance = fixture.instance_mut();
    let before = instance.pending_events.len();
    for value in [f64::NAN, f64::INFINITY, 121.0, -121.0] {
        assert!(
            instance
                .queue_note_expression(
                    2,
                    64,
                    Vst3StandardNoteExpression::Tuning,
                    Vst3NoteExpressionValue::TuningSemitones(value),
                    5
                )
                .is_err()
        );
        assert_eq!(instance.pending_events.len(), before);
    }
    assert_eq!(
        instance.queue_note_expression(
            16,
            64,
            Vst3StandardNoteExpression::Tuning,
            Vst3NoteExpressionValue::TuningSemitones(0.0),
            5
        ),
        Err(Vst3NoteExpressionError::InvalidAddress)
    );
    assert_eq!(instance.pending_events.len(), before);
    Ok(Vst3TuningPayloadAudit {
        physical_cases: 5,
        normalized_cases: 3,
    })
}

#[cfg(test)]
#[test]
fn native_tuning_payload_uses_sdk_pitch_units_and_exact_note_identity() {
    let audit = audit_tuning_payload().unwrap();
    assert_eq!(audit.physical_cases, 5);
    assert_eq!(audit.normalized_cases, 3);
}
