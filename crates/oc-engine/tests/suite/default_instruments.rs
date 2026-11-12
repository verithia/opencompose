//! A track that names only an instrument family renders through its default.
//!
//! The default is injected by `oc_dsl::parse` and written into the document, so
//! these tests also prove the choice is visible when the project is serialized.

use std::collections::HashMap;
use std::path::Path;

use oc_engine::render_cache::RenderCache;

const SAMPLE_RATE: u32 = 48_000;

fn render(source: &str) -> (oc_dsl::ast::Project, Vec<f32>) {
    let mut project = oc_dsl::parse(source).expect("project parses");
    oc_dsl::instruments::apply_defaults(&mut project);
    let samples = render_project(&project);
    (project, samples)
}

fn render_project(project: &oc_dsl::ast::Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    let mut plugins = HashMap::new();
    cache
        .render_full(project, Path::new("unused.sf2"), SAMPLE_RATE, &mut plugins)
        .expect("default instrument renders without a SoundFont")
}

fn db(level: f64) -> f64 {
    20.0 * level.max(1.0e-12).log10()
}

fn peak_dbfs(samples: &[f32]) -> f64 {
    db(samples
        .iter()
        .map(|value| f64::from(value.abs()))
        .fold(0.0, f64::max))
}

/// RMS of the second bar, interleaved stereo. Bars 1-2 at 120 BPM are 0-4 s.
fn second_bar_rms_dbfs(samples: &[f32]) -> f64 {
    let start = (SAMPLE_RATE as usize) * 2 * 2;
    let end = (SAMPLE_RATE as usize) * 4 * 2;
    let window = &samples[start.min(samples.len())..end.min(samples.len())];
    let power = window
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        / window.len().max(1) as f64;
    db(power.sqrt())
}

fn phrase_family(category: &str) -> String {
    format!(
        "@project \"{category}\"\n  tempo: 120\n  time_signature: 4/4\n\
@track t | instrument: {category}/any\n\
  @phrase line [1:1 - 3:1]\n\
    C4 q mf\n    E4 q mf\n    G4 q mf\n    C5 q mf\n\
    G4 q mf\n    E4 q mf\n    C4 q mf\n    G3 q mf\n"
    )
}

#[test]
fn a_hand_written_bare_track_renders_through_its_default() {
    let (project, samples) = render(&phrase_family("piano"));
    assert_eq!(project.tracks[0].preset.as_deref(), Some("oc_default_keys"));
    let text = oc_dsl::serialize(&project);
    assert!(text.contains("@preset oc_default_keys"), "{text}");
    assert!(text.contains("preset: oc_default_keys"), "{text}");

    assert!(
        peak_dbfs(&samples) > -40.0,
        "peak {} dBFS",
        peak_dbfs(&samples)
    );
    assert!(
        second_bar_rms_dbfs(&samples) > -40.0,
        "second bar {} dBFS; the default must sustain, not click",
        second_bar_rms_dbfs(&samples)
    );
}

#[test]
fn every_mapped_family_renders_non_silent() {
    let families = [
        ("piano", "oc_default_keys"),
        ("keys", "oc_default_keys"),
        ("bass", "oc_default_bass"),
        ("organ", "oc_default_strings"),
        ("strings", "oc_default_strings"),
        ("pad", "oc_default_strings"),
        ("vocal", "oc_default_strings"),
        ("woodwinds", "oc_default_strings"),
        ("lead", "oc_default_lead"),
        ("synth", "oc_default_lead"),
        ("fx", "oc_default_lead"),
        ("guitar", "oc_default_guitar"),
        ("percussion", "oc_default_guitar"),
        ("brass", "oc_default_brass"),
    ];
    for (category, expected) in families {
        let (project, samples) = render(&phrase_family(category));
        assert_eq!(
            project.tracks[0].preset.as_deref(),
            Some(expected),
            "{category}"
        );
        assert!(
            peak_dbfs(&samples) > -40.0,
            "{category}: peak {} dBFS",
            peak_dbfs(&samples)
        );
        assert!(
            second_bar_rms_dbfs(&samples) > -40.0,
            "{category}: second bar {} dBFS",
            second_bar_rms_dbfs(&samples)
        );
    }
}

#[test]
fn the_drum_families_use_the_synthesised_kit() {
    for category in ["drums", "kit", "drumkit"] {
        let source = format!(
            "@project \"{category}\"\n  tempo: 120\n  time_signature: 4/4\n\
@track t | instrument: {category}/acoustic\n\
  @pattern beat [1:1 - 3:1]\n\
    kick 1:0 ff\n    snare 2:0 mf\n    kick 3:0 ff\n    snare 4:0 mf\n\
    kick 5:0 ff\n    snare 6:0 mf\n    kick 7:0 ff\n    snare 8:0 mf\n"
        );
        let (project, samples) = render(&source);
        assert_eq!(
            project.tracks[0].preset.as_deref(),
            Some("oc_default_drums"),
            "{category}"
        );
        let text = oc_dsl::serialize(&project);
        assert!(text.contains("@kit oc_default_drums"), "{text}");
        assert!(
            peak_dbfs(&samples) > -40.0,
            "{category}: peak {} dBFS",
            peak_dbfs(&samples)
        );
        assert!(
            second_bar_rms_dbfs(&samples) > -40.0,
            "{category}: second bar {} dBFS",
            second_bar_rms_dbfs(&samples)
        );
    }
}

#[test]
fn a_clip_pattern_supplies_the_family_for_a_track_that_declares_none() {
    let (project, samples) = render(
        "@project\n\
@pattern riff | instrument: piano/acoustic_grand\n\
  events:\n    C4 8th mf\n    E4 8th mf\n\
@track keys\n  clip: riff [1:1 - 2:1]\n",
    );
    assert_eq!(project.tracks[0].preset.as_deref(), Some("oc_default_keys"));
    assert!(
        peak_dbfs(&samples) > -40.0,
        "the pattern family did not sound: peak {} dBFS",
        peak_dbfs(&samples)
    );
}

// ------------------------------------------------------------------ acoustics
//
// The numbers these tests pin come from a separate acoustic probe. Assertions
// keep a wide tolerance and only claim the direction the change promised: decay
// shortens with the register, the hammer lifts the onset high band, the release
// follows the key and the hold, and the patch does not get louder.

/// A bare piano track holding one note, so every probe is its own render.
fn piano_note(pitch: &str, token: &str, tempo: u32, bars: u32) -> (oc_dsl::ast::Project, Vec<f32>) {
    render(&format!(
        "@project \"probe\"\n  tempo: {tempo}\n  time_signature: 4/4\n\
@track keys | instrument: piano/acoustic_grand\n\
  @phrase probe [1:1 - {bars}:1]\n    {pitch} {token} mf\n"
    ))
}

fn mono(samples: &[f32]) -> Vec<f64> {
    samples
        .chunks(2)
        .map(|pair| f64::from(pair.iter().sum::<f32>()) / 2.0)
        .collect()
}

/// RMS envelope, 40 ms windows every 10 ms: two periods of C2, so the low
/// octave's own waveform does not modulate the measurement.
fn envelope(samples: &[f64]) -> Vec<(f64, f64)> {
    let window = SAMPLE_RATE as usize / 25;
    let hop = SAMPLE_RATE as usize / 100;
    let mut out = Vec::new();
    let mut start = 0;
    while start + window <= samples.len() {
        let chunk = &samples[start..start + window];
        let power = chunk.iter().map(|value| value * value).sum::<f64>() / window as f64;
        out.push((
            (start + window / 2) as f64 / f64::from(SAMPLE_RATE),
            power.sqrt(),
        ));
        start += hop;
    }
    out
}

/// First time the envelope falls `drop_db` below the loudest window of the note.
fn time_to_drop(samples: &[f64], end_seconds: f64, drop_db: f64) -> Option<f64> {
    let envelope = envelope(samples);
    let inside: Vec<(f64, f64)> = envelope
        .into_iter()
        .filter(|(time, _)| *time <= end_seconds)
        .collect();
    let reference = inside.iter().map(|(_, value)| *value).fold(0.0, f64::max);
    if reference <= 1.0e-9 {
        return None;
    }
    let floor = reference * 10f64.powf(drop_db / 20.0);
    inside
        .iter()
        .find(|(_, value)| *value <= floor)
        .map(|(time, _)| *time)
}

/// Mean square of one band over a Hann-windowed segment, by direct DFT.
fn band_power(samples: &[f64], start_seconds: f64, seconds: f64, lo: f64, hi: f64) -> f64 {
    let start = (start_seconds * f64::from(SAMPLE_RATE)).round() as usize;
    let length = (seconds * f64::from(SAMPLE_RATE)).round() as usize;
    let Some(chunk) = samples.get(start..start + length) else {
        return 0.0;
    };
    let n = chunk.len();
    let window: Vec<f64> = (0..n)
        .map(|index| {
            let phase = std::f64::consts::TAU * index as f64 / (n - 1).max(1) as f64;
            0.5 - 0.5 * phase.cos()
        })
        .collect();
    let scale = 1.0 / (n as f64 * window.iter().map(|w| w * w).sum::<f64>());
    let first = (lo * n as f64 / f64::from(SAMPLE_RATE)).ceil() as usize;
    let last = (hi * n as f64 / f64::from(SAMPLE_RATE)).floor() as usize;
    let mut power = 0.0;
    for bin in first..=last {
        let omega = std::f64::consts::TAU * bin as f64 / n as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (index, value) in chunk.iter().enumerate() {
            let phase = omega * index as f64;
            re += value * window[index] * phase.cos();
            im += value * window[index] * phase.sin();
        }
        power += re * re + im * im;
    }
    2.0 * power * scale
}

fn band_emphasis_db(samples: &[f64]) -> f64 {
    // 4-10 kHz against the 100-2000 Hz body, in the first 30 ms and at 60-90 ms.
    let onset = |lo, hi| band_power(samples, 0.0, 0.03, lo, hi);
    let steady = |lo, hi| band_power(samples, 0.06, 0.03, lo, hi);
    let onset_ratio = onset(4000.0, 10_000.0) / onset(100.0, 2000.0).max(1.0e-30);
    let steady_ratio = steady(4000.0, 10_000.0) / steady(100.0, 2000.0).max(1.0e-30);
    10.0 * (onset_ratio / steady_ratio).log10()
}

fn segment_rms_db(samples: &[f64], start_seconds: f64, end_seconds: f64) -> f64 {
    let start = (start_seconds * f64::from(SAMPLE_RATE)) as usize;
    let end = ((end_seconds * f64::from(SAMPLE_RATE)) as usize).min(samples.len());
    let Some(chunk) = samples.get(start.min(end)..end) else {
        return -240.0;
    };
    if chunk.is_empty() {
        return -240.0;
    }
    let power = chunk.iter().map(|value| value * value).sum::<f64>() / chunk.len() as f64;
    10.0 * power.max(1.0e-24).log10()
}

fn peak_db(samples: &[f64]) -> f64 {
    db(samples
        .iter()
        .fold(0.0f64, |acc, value| acc.max(value.abs())))
}

/// Drop every hammer route from the injected keys patch, keeping the envelope.
fn strip_hammer(project: &mut oc_dsl::ast::Project) {
    let preset = project
        .presets
        .iter_mut()
        .find(|preset| preset.name == "oc_default_keys")
        .expect("the keys default is injected");
    for (name, value) in &mut preset.params {
        if name.starts_with("mod.") && name.ends_with(".amount") {
            *value = oc_dsl::ast::ParamValue::Number(0.0);
        }
    }
}

#[test]
fn the_keys_decay_shortens_with_the_register() {
    // A whole note at 30 BPM is 8 s, so even C2 reaches -40 dB.
    let mut times = Vec::new();
    for pitch in ["C2", "C4", "C6"] {
        let (_, samples) = piano_note(pitch, "w", 30, 2);
        let samples = mono(&samples);
        let t40 = time_to_drop(&samples, 8.0, -40.0);
        assert!(t40.is_some(), "{pitch} never reached -40 dB");
        times.push((pitch, t40.unwrap()));
    }
    let (c2, c4, c6) = (times[0].1, times[1].1, times[2].1);
    assert!(
        c2 > c4 * 1.5 && c4 > c6 * 1.5,
        "decay is not register dependent: C2 {c2:.2}s, C4 {c4:.2}s, C6 {c6:.2}s"
    );
}

#[test]
fn the_keys_hammer_lifts_the_onset_high_band() {
    let (project, samples) = piano_note("C4", "w", 120, 2);
    let with_hammer = band_emphasis_db(&mono(&samples));
    let mut bare = project.clone();
    strip_hammer(&mut bare);
    let without_hammer = band_emphasis_db(&mono(&render_project(&bare)));
    assert!(
        with_hammer > without_hammer + 2.0,
        "hammer adds {:.2} dB of 4-10 kHz emphasis; without it {without_hammer:.2} dB",
        with_hammer - without_hammer
    );
}

#[test]
fn the_keys_damps_high_and_short_notes_faster() {
    // The tail 200 ms after note-off, relative to the note's peak.
    let mut tails = Vec::new();
    for pitch in ["C2", "C4"] {
        let (_, samples) = piano_note(pitch, "q", 120, 2);
        let samples = mono(&samples);
        tails.push(segment_rms_db(&samples, 0.70, 0.72) - peak_db(&samples));
    }
    assert!(
        tails[1] < tails[0] - 6.0,
        "C4 tail {:.2} dB is not clearly below C2 tail {:.2} dB",
        tails[1],
        tails[0]
    );

    // A sixteenth is damped sooner than a quarter at the same key.
    let (_, short) = piano_note("C4", "16th", 120, 2);
    let short = mono(&short);
    let (_, long) = piano_note("C4", "q", 120, 2);
    let long = mono(&long);
    let short_tail = segment_rms_db(&short, 0.32, 0.34) - peak_db(&short);
    let long_tail = segment_rms_db(&long, 0.70, 0.72) - peak_db(&long);
    let short_drop = short_tail - (segment_rms_db(&short, 0.08, 0.10) - peak_db(&short));
    let long_drop = long_tail - (segment_rms_db(&long, 0.45, 0.47) - peak_db(&long));
    assert!(
        short_drop < long_drop - 3.0,
        "sixteenth release {short_drop:.2} dB is not shorter than the quarter's {long_drop:.2} dB"
    );
}

#[test]
fn the_keys_stays_in_its_level_window() {
    // The 2-bar phrase peaks at -9.26 dBFS (48 kHz, no normalisation). A
    // redesign of the envelope must not buy loudness, so the peak stays inside
    // one dB of that value.
    let (_, samples) = render(
        "@project \"reference\"\n  tempo: 120\n  time_signature: 4/4\n\
@track keys | instrument: piano/acoustic_grand\n\
  @phrase line [1:1 - 3:1]\n\
    C4 q mf\n    E4 q mf\n    G4 q mf\n    C5 q mf\n\
    G4 q mf\n    E4 q mf\n    C4 q mf\n    G3 q mf\n",
    );
    let peak = peak_dbfs(&samples);
    assert!(
        (-10.3..=-8.3).contains(&peak),
        "reference peak {peak:.2} dBFS left the -9.26 dBFS +/-1 dB window"
    );
    assert!(
        second_bar_rms_dbfs(&samples) > -32.0,
        "second bar {:.2} dBFS: the note died before the phrase ended",
        second_bar_rms_dbfs(&samples)
    );
}
