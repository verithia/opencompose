//! `amp.damping`: one law that shortens a note's decay with the key it starts
//! on and its release with the key and with a short hold.
//!
//! The preset that uses it is `oc_default_keys` (see `oc-engine` tests), so this
//! file guards the engine-side law: the same patch at damping 0 keeps one decay
//! for every key, and at damping 1 the decay halves per octave above C4.

use oc_dsl::ast::ParamValue;
use oc_synth_core::midi_event::{MidiEvent, MidiMessage};
use oc_synth_core::synth::{SynthPatch, render_track_with_patch};

const SAMPLE_RATE: u32 = 48_000;

fn track(note: u8, held_seconds: f64) -> Vec<MidiEvent> {
    vec![
        MidiEvent {
            time_samples: 0,
            event: MidiMessage::NoteOn {
                channel: 0,
                note,
                velocity: 100,
            },
            track_index: 0,
        },
        MidiEvent {
            time_samples: (held_seconds * f64::from(SAMPLE_RATE)) as u64,
            event: MidiMessage::NoteOff { channel: 0, note },
            track_index: 0,
        },
    ]
}

fn patch(params: &[(&str, f64)]) -> SynthPatch {
    let params: Vec<(String, ParamValue)> = params
        .iter()
        .map(|(name, value)| ((*name).to_string(), ParamValue::Number(*value)))
        .collect();
    let mut patch = SynthPatch::default();
    patch.apply_params(Some(&params));
    patch
}

fn render(note: u8, held_seconds: f64, seconds: f64, params: &[(&str, f64)]) -> Vec<f64> {
    let events = track(note, held_seconds);
    let refs: Vec<&MidiEvent> = events.iter().collect();
    let samples = render_track_with_patch(
        &refs,
        (seconds * f64::from(SAMPLE_RATE)) as usize,
        SAMPLE_RATE,
        patch(params),
        120.0,
    );
    samples
        .chunks(2)
        .map(|pair| f64::from(pair.iter().sum::<f32>()) / 2.0)
        .collect()
}

/// Time at which the 40 ms RMS envelope drops `drop_db` below the note's peak.
fn time_to_drop(samples: &[f64], drop_db: f64) -> f64 {
    let window = SAMPLE_RATE as usize / 25;
    let hop = SAMPLE_RATE as usize / 100;
    let mut peak = 0.0f64;
    let mut windows = Vec::new();
    let mut start = 0;
    while start + window <= samples.len() {
        let chunk = &samples[start..start + window];
        let value =
            (chunk.iter().map(|sample| sample * sample).sum::<f64>() / window as f64).sqrt();
        peak = peak.max(value);
        windows.push(((start + window) as f64 / f64::from(SAMPLE_RATE), value));
        start += hop;
    }
    let floor = peak * 10f64.powf(drop_db / 20.0);
    windows
        .iter()
        .find(|(_, value)| *value <= floor)
        .map(|(time, _)| *time)
        .expect("the note decays past the threshold")
}

/// A decaying patch with no sustain floor, so the decay is measurable.
fn decaying(damping: f64) -> Vec<(&'static str, f64)> {
    vec![
        ("amp.attack", 0.002),
        ("amp.decay", 2.0),
        ("amp.sustain", 0.0),
        ("amp.curve", 0.5),
        ("amp.release", 0.5),
        ("amp.damping", damping),
        // One oscillator through a fixed filter: the measurement is the amp
        // envelope alone, with no beating or key-tracked cutoff in it.
        ("unison.voices", 1.0),
        ("unison.detune", 0.0),
        ("unison.width", 0.0),
        ("osc.b.level", 0.0),
        ("filter.keytrack", 0.0),
        ("filter.cutoff", 12_000.0),
        ("output.gain", 0.5),
    ]
}

#[test]
fn damping_zero_keeps_one_decay_for_every_key() {
    let params = decaying(0.0);
    let low = time_to_drop(&render(36, 5.0, 6.0, &params), -20.0);
    let high = time_to_drop(&render(84, 5.0, 6.0, &params), -20.0);
    let ratio = low / high;
    assert!(
        (0.85..=1.15).contains(&ratio),
        "undamped decay changed with the key: C2 {low:.2}s, C6 {high:.2}s"
    );
}

#[test]
fn damping_shortens_the_decay_by_half_per_octave() {
    let params = decaying(1.0);
    let low = time_to_drop(&render(36, 5.0, 6.0, &params), -20.0);
    let middle = time_to_drop(&render(60, 5.0, 6.0, &params), -20.0);
    let high = time_to_drop(&render(84, 5.0, 6.0, &params), -20.0);
    // C2 is two octaves below C4 and C6 two above: 4x each way at damping 1.
    assert!(
        low > middle * 2.5 && middle > high * 2.5,
        "decay did not halve per octave: C2 {low:.2}s, C4 {middle:.2}s, C6 {high:.2}s"
    );
}

#[test]
fn damping_shortens_the_release_for_short_holds() {
    let params = decaying(1.0);
    // Drop from just before note-off to 200 ms after it, so the comparison is
    // the release and not how far the note had already decayed.
    let drop = |held: f64| {
        let samples = render(60, held, 3.0, &params);
        let rms = |start: f64, end: f64| {
            let start = (start * f64::from(SAMPLE_RATE)) as usize;
            let end = ((end * f64::from(SAMPLE_RATE)) as usize).min(samples.len());
            let power = samples[start..end]
                .iter()
                .map(|value| value * value)
                .sum::<f64>()
                / (end - start) as f64;
            10.0 * power.max(1.0e-24).log10()
        };
        rms(held + 0.20, held + 0.22) - rms(held - 0.03, held - 0.01)
    };
    let short = drop(0.10);
    let long = drop(1.0);
    assert!(
        short < long - 3.0,
        "a 16th-note release ({short:.2} dB) was not shorter than a held one ({long:.2} dB)"
    );
}
