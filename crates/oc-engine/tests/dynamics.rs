//! Hairpins must be audible as a level ramp: `cresc` swells and `dim` falls
//! across four consecutive windows, and the same notes without a mark stay flat.
//!
//! Every measurement window sits in the middle of a bar, so no note onset or
//! release is inside it; the only difference between the takes is the mark.

use crate::sf2_builder;
use crate::sf2_spectral;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oc_dsl::ast::Project;
use oc_engine::render_cache::RenderCache;
use sf2_builder::{Font, Points, Sample, generators};

const RATE: u32 = 48_000;
/// 120 bpm in 4/4: one bar is two seconds.
const BAR_SECONDS: f64 = 2.0;

/// Four whole notes over four bars, with the mark on the first line of the
/// phrase. `None` is the control take: same notes at one level, no mark.
///
/// A hairpin stops at the next written dynamic, so the marked take writes the
/// starting level on the first note only and lets the ramp carry the rest.
fn project(mark: Option<&str>, first: &str, rest: &str) -> Project {
    let mark = mark.map(|mark| format!("    {mark}\n")).unwrap_or_default();
    let source = format!(
        r#"@ocp "0.2.0"
@project "swell"
  tempo: 120
  time_signature: 4/4

@preset swell_pad
  source: builtin
  plugin: "builtin.open_synth"
  params: {{ osc.a.waveform: 1, osc.a.level: 0.8, amp.attack: 0.01, amp.decay: 0.1, amp.sustain: 0.9, amp.release: 0.05, filter.cutoff: 12000, filter.env.amount: 0, voice.polyphony: 8 }}

@track strings | preset: swell_pad, instrument: strings/violin
  @phrase swell [1:1 - 5:1]
{mark}    @0 C4 w {first}
    @4 C4 w {rest}
    @8 C4 w {rest}
    @12 C4 w {rest}
"#
    );
    let project = oc_dsl::parse(&source).expect("the swell fixture parses");
    oc_dsl::validate(&project).expect("the swell fixture validates");
    project
}

fn render(project: &Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    cache
        .render_full(
            project,
            Path::new("/nonexistent/hairpin-must-use-the-builtin-preset.sf2"),
            RATE,
            &mut HashMap::new(),
        )
        .expect("the project renders")
}

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    (sum / samples.len() as f64).sqrt()
}

fn db(level: f64) -> f64 {
    20.0 * level.max(1e-12).log10()
}

/// Mono RMS over the middle half of one bar, clear of the note onsets.
fn bar_level(audio: &[f32], bar: usize) -> f64 {
    let start = bar as f64 * BAR_SECONDS + BAR_SECONDS * 0.25;
    let end = start + BAR_SECONDS * 0.5;
    let first = (start * f64::from(RATE)) as usize;
    let last = ((end * f64::from(RATE)) as usize).min(audio.len() / 2);
    let mono: Vec<f32> = (first..last)
        .map(|frame| {
            let left = audio.get(frame * 2).copied().unwrap_or(0.0);
            let right = audio.get(frame * 2 + 1).copied().unwrap_or(0.0);
            (left + right) * 0.5
        })
        .collect();
    rms(&mono)
}

fn ladder(audio: &[f32]) -> Vec<f64> {
    (0..4).map(|bar| db(bar_level(audio, bar))).collect()
}

/// A control take of the same notes: the level is flat, so any ladder below is
/// the mark and not the phrase.
fn assert_flat(ladder: &[f64]) {
    let spread = ladder.iter().cloned().fold(f64::MIN, f64::max)
        - ladder.iter().cloned().fold(f64::MAX, f64::min);
    assert!(spread < 0.5, "the control take is flat: {ladder:?}");
}

#[test]
fn crescendo_raises_the_level_in_four_consecutive_windows() {
    assert_flat(&ladder(&render(&project(None, "pp", "pp"))));

    let swelled = ladder(&render(&project(Some("@0.0 cresc"), "pp", "")));
    for pair in swelled.windows(2) {
        assert!(
            pair[1] > pair[0],
            "every window is louder than the one before: {swelled:?}"
        );
    }
    let total = swelled[3] - swelled[0];
    assert!(
        total >= 12.0,
        "the swell spans at least 12 dB: {total} dB in {swelled:?}"
    );
}

#[test]
fn diminuendo_falls_by_the_same_ladder() {
    assert_flat(&ladder(&render(&project(None, "pp", "pp"))));

    let faded = ladder(&render(&project(Some("@0.0 dim"), "f", "")));
    for pair in faded.windows(2) {
        assert!(
            pair[1] < pair[0],
            "every window is quieter than the one before: {faded:?}"
        );
    }
    let total = faded[0] - faded[3];
    assert!(
        total >= 12.0,
        "the fade spans at least 12 dB: {total} dB in {faded:?}"
    );
}

#[test]
fn the_ramp_reaches_the_target_dynamic_at_the_phrase_end() {
    // pp (33) to f (96) under the sampler's square law is +18.55 dB over the
    // span; the four windows measure the middle of each bar of it.
    let swelled = ladder(&render(&project(Some("@0.0 cresc"), "pp", "")));
    let expected = 40.0 * (96.0f64 / 33.0).log10();
    let measured = swelled[3] - swelled[0];
    // The windows centre on beats 2 and 14 of the 16-beat span.
    let predicted = expected * (14.0 - 2.0) / 16.0;
    assert!(
        (measured - predicted).abs() < 1.5,
        "the windows follow the ramp: measured {measured} dB, predicted {predicted} dB"
    );
}

// ---------------------------------------------------------------------------
// Sampled swells: the same mark must move the velocity-layer mix, not only the
// track gain. The bank has two velocity layers with equal RMS and different
// spectra, so the centroid reads the layer mix and the RMS reads the level law.

/// One second of `partials`, each `(hertz, amplitude)`, phase-continuous.
fn layer_points(partials: &[(f64, f64)]) -> Vec<i16> {
    (0..RATE as usize)
        .map(|frame| {
            let time = frame as f64 / f64::from(RATE);
            let value: f64 = partials
                .iter()
                .map(|(hz, amplitude)| amplitude * (2.0 * std::f64::consts::PI * hz * time).sin())
                .sum();
            (value * 32767.0) as i16
        })
        .collect()
}

/// Two layers at equal RMS: one 440 Hz partial against 440 Hz plus 3520 Hz.
fn two_layer_font() -> PathBuf {
    let frames = RATE;
    let bright = 0.4 / 2f64.sqrt();
    let low = Sample {
        name: "low".into(),
        rate: RATE,
        root_key: 60,
        points: Points::I16(layer_points(&[(440.0, 0.4)])),
        loop_start: 0,
        loop_end: frames,
    };
    let high = Sample {
        name: "high".into(),
        rate: RATE,
        root_key: 60,
        points: Points::I16(layer_points(&[(440.0, bright), (3520.0, bright)])),
        loop_start: 0,
        loop_end: frames,
    };
    let mut font = Font::new("two-layer swell");
    font.add_sample(low);
    font.add_sample(high);
    let instrument = font.add_instrument(
        "instrument",
        vec![
            vec![
                (generators::SAMPLE_ID, 0),
                (generators::SAMPLE_MODES, 1),
                (generators::OVERRIDING_ROOT_KEY, 60),
                (generators::KEY_RANGE, sf2_builder::range(0, 127) as i16),
                (generators::VEL_RANGE, sf2_builder::range(1, 63) as i16),
            ],
            vec![
                (generators::SAMPLE_ID, 1),
                (generators::SAMPLE_MODES, 1),
                (generators::OVERRIDING_ROOT_KEY, 60),
                (generators::KEY_RANGE, sf2_builder::range(0, 127) as i16),
                (generators::VEL_RANGE, sf2_builder::range(64, 127) as i16),
            ],
        ],
    );
    font.add_preset(
        "swell",
        0,
        0,
        vec![vec![(generators::INSTRUMENT, instrument as i16)]],
    );
    font.write_temp()
}

/// The sampled swell fixture. `opt_out` writes the layer crossfade off, either
/// on the track or on the preset.
fn sampled_swell(font: &Path, mark: Option<&str>, opt_out: Option<&str>) -> Project {
    let mark = mark.map(|mark| format!("    {mark}\n")).unwrap_or_default();
    let engine = opt_out
        .map(|where_| match where_ {
            "track" => "  engine_params: {dynamic_layers: off}\n".to_string(),
            _ => String::new(),
        })
        .unwrap_or_default();
    let preset_params = match opt_out {
        Some("preset") => "  params: {dynamic_layers: off}\n",
        _ => "",
    };
    let source = format!(
        r#"@ocp "0.2.0"
@project "sampled swell"
  tempo: 120
  time_signature: 4/4

@preset swell
  source: soundfont
  program: 0
  file: "{}"
{preset_params}
@track strings | preset: swell, instrument: strings/violin
{engine}  @phrase swell [1:1 - 5:1]
{mark}    @0 C4 w pp
    @4 C4 w
    @8 C4 w
    @12 C4 w
"#,
        font.display()
    );
    let project = oc_dsl::parse(&source).expect("the sampled swell parses");
    oc_dsl::validate(&project).expect("the sampled swell validates");
    project
}

fn render_with_font(project: &Project) -> Vec<f32> {
    let mut cache = RenderCache::new();
    cache.set_pattern_previews(false);
    cache
        .render_full(
            project,
            Path::new("/nonexistent/unused.sf2"),
            RATE,
            &mut HashMap::new(),
        )
        .expect("the sampled swell renders")
}

/// Spectral centroid of the middle half of one bar, 100 Hz .. 12 kHz.
fn bar_centroid(audio: &[f32], bar: usize) -> f64 {
    let start = bar as f64 * BAR_SECONDS + BAR_SECONDS * 0.25;
    let end = start + BAR_SECONDS * 0.5;
    let first = (start * f64::from(RATE)) as usize;
    let last = ((end * f64::from(RATE)) as usize).min(audio.len() / 2);
    let mono: Vec<f32> = (first..last)
        .map(|frame| {
            let left = audio.get(frame * 2).copied().unwrap_or(0.0);
            let right = audio.get(frame * 2 + 1).copied().unwrap_or(0.0);
            (left + right) * 0.5
        })
        .collect();
    sf2_spectral::spectral_centroid(&mono, f64::from(RATE), 100.0, 12_000.0)
}

fn centroid_ladder(audio: &[f32]) -> Vec<f64> {
    (0..4).map(|bar| bar_centroid(audio, bar)).collect()
}

fn spread(ladder: &[f64]) -> f64 {
    ladder.iter().cloned().fold(f64::MIN, f64::max)
        - ladder.iter().cloned().fold(f64::MAX, f64::min)
}

/// The streaming lane carries the same ramp table as the offline renderer, so a
/// monitored phrase blends its velocity layers exactly like a render does.
///
/// Every take streams through the real [`BlockSource`] path with caller-reserved
/// blocks, and the reference is the offline player the renderer builds.
#[cfg(feature = "plugin-host")]
mod streaming_parity {
    use super::*;
    use oc_engine::block_source::{BlockSource, MAX_SOURCE_BLOCK_FRAMES, SourceOutput};
    use oc_engine::interpret::{self, InterpretSettings};
    use oc_engine::soundfont::SoundFontRenderer;

    /// The phrase is four bars at 120 bpm: 8 s.
    const PHRASE_FRAMES: usize = 8 * RATE as usize;
    /// Beat 8 of the phrase: half way through the ramp, 4 s in.
    const SEEK_FRAME: u64 = 8 * 24_000;
    const TOLERANCE: f32 = 1e-6;
    /// A layer blend this large is far above the parity tolerance.
    const AUDIBLE: f32 = 1e-3;

    fn interpreted(project: &Project) -> Vec<oc_engine::interpret::MidiEvent> {
        interpret::try_interpret(
            project,
            &InterpretSettings {
                sample_rate: RATE,
                ..Default::default()
            },
        )
        .expect("the phrase interprets")
    }

    /// The track's ramps in output frames, the table the offline renderer
    /// installs. Built here so the reference does not borrow the streaming
    /// lane's install path.
    fn offline_ramps(project: &Project) -> Vec<oc_engine::sf2::DynamicRamp> {
        let clock =
            oc_dsl::timing::ProjectClock::new(&project.properties).expect("the project clock");
        oc_dsl::dynamics::track_layer_ramps(project, &project.tracks[0], &clock)
            .expect("the track ramps")
            .iter()
            .map(|ramp| oc_engine::sf2::DynamicRamp {
                start_frame: clock
                    .sample_at_quarter(ramp.start, RATE)
                    .expect("the ramp start"),
                end_frame: clock
                    .sample_at_quarter(ramp.end, RATE)
                    .expect("the ramp end"),
                start_velocity: ramp.start_velocity,
                target_velocity: ramp.target_velocity,
            })
            .collect()
    }

    /// The offline renderer's source for one track, exactly as the renderer
    /// builds it: layers set, then the ramp table.
    fn offline(project: &Project, font: &Path, frames: usize, layers: bool) -> Vec<f32> {
        let mut renderer = SoundFontRenderer::new(RATE, font).expect("the bank loads");
        renderer.set_dynamic_layers(layers);
        if layers {
            renderer.set_dynamic_ramps(offline_ramps(project));
        }
        renderer
            .try_render_events(&interpreted(project), frames as f64 / f64::from(RATE))
            .expect("the offline render runs")
    }

    /// The streaming lane: events interleaved with reserved blocks, starting at
    /// `offset` project frames.
    fn streaming(project: &Project, font: &Path, frames: usize, offset: u64) -> Vec<f32> {
        let track = &project.tracks[0];
        let preset = project.presets.first();
        let params: Vec<(String, oc_dsl::ast::ParamValue)> = Vec::new();
        let mut source =
            BlockSource::new(preset, &params, font, RATE, project.properties.tempo, None)
                .expect("the streaming lane builds");
        source
            .install_hairpin_layers(preset, project, track, &project.properties, offset)
            .expect("the layer ramps install");

        let events = interpreted(project);
        let mut output = SourceOutput::new(0, &[]);
        let mut audio = Vec::with_capacity(frames * 2);
        let end = offset as usize + frames;
        let mut position = offset as usize;
        // A lane that joins the timeline mid-phrase never sends earlier events.
        let mut cursor = events
            .iter()
            .position(|event| event.time_samples as usize >= position)
            .unwrap_or(events.len());
        while position < end {
            while cursor < events.len() && events[cursor].time_samples as usize <= position {
                source
                    .send(&events[cursor].event)
                    .expect("the streaming event dispatches");
                cursor += 1;
            }
            let next = events
                .get(cursor)
                .map(|event| event.time_samples as usize)
                .unwrap_or(end);
            let count = MAX_SOURCE_BLOCK_FRAMES
                .min(end - position)
                .min((next - position).max(1));
            let transport = oc_host::PluginTransport::new(position as u64, RATE, 120.0);
            source.render_into(count, RATE, transport, &mut output);
            audio.extend_from_slice(output.main());
            position += count;
        }
        audio
    }

    fn max_diff(expected: &[f32], actual: &[f32]) -> f32 {
        assert_eq!(expected.len(), actual.len(), "the takes have one length");
        expected
            .iter()
            .zip(actual)
            .map(|(expected, actual)| (expected - actual).abs())
            .fold(0.0, f32::max)
    }

    /// A short note under the mark, then a fresh note at beat 8, so a lane can
    /// join the middle of the ramp with no history.
    fn seek_fixture(font: &Path, opt_out: bool) -> Project {
        let engine = if opt_out {
            "  engine_params: {dynamic_layers: off}\n"
        } else {
            ""
        };
        let source = format!(
            r#"@ocp "0.2.0"
@project "sampled seek"
  tempo: 120
  time_signature: 4/4

@preset swell
  source: soundfont
  program: 0
  file: "{}"

@track strings | preset: swell, instrument: strings/violin
{engine}  @phrase swell [1:1 - 5:1]
    @0.0 cresc
    @0 C4 8th pp
    @8 C4 w
"#,
            font.display()
        );
        let project = oc_dsl::parse(&source).expect("the seek fixture parses");
        oc_dsl::validate(&project).expect("the seek fixture validates");
        project
    }

    #[test]
    fn a_streamed_hairpin_matches_the_offline_layer_blend() {
        let font = two_layer_font();
        let project = sampled_swell(&font, Some("@0.0 cresc"), None);
        let rendered = offline(&project, &font, PHRASE_FRAMES, true);
        let streamed = streaming(&project, &font, PHRASE_FRAMES, 0);
        let hard = streaming(
            &sampled_swell(&font, Some("@0.0 cresc"), Some("track")),
            &font,
            PHRASE_FRAMES,
            0,
        );
        let parity = max_diff(&rendered, &streamed);
        assert!(
            parity <= TOLERANCE,
            "streaming matches the offline blend: {parity:.3e}"
        );
        let blend = max_diff(&streamed, &hard);
        println!("streamed parity {parity:.3e}, layer blend {blend:.3e}");
        assert!(
            blend > AUDIBLE,
            "the streamed take really crossfades: {blend:.3e} against hard selection"
        );
    }

    #[test]
    fn a_streamed_opt_out_matches_the_offline_opt_out() {
        let font = two_layer_font();
        let disabled = sampled_swell(&font, Some("@0.0 cresc"), Some("track"));
        let rendered = offline(&disabled, &font, PHRASE_FRAMES, false);
        let streamed = streaming(&disabled, &font, PHRASE_FRAMES, 0);
        let parity = max_diff(&rendered, &streamed);
        assert!(
            parity <= TOLERANCE,
            "the opt-out matches offline: {parity:.3e}"
        );
        let enabled = offline(
            &sampled_swell(&font, Some("@0.0 cresc"), None),
            &font,
            PHRASE_FRAMES,
            true,
        );
        let blend = max_diff(&streamed, &enabled);
        println!("opt-out parity {parity:.3e}, crossfade gap {blend:.3e}");
        assert!(
            blend > AUDIBLE,
            "the opt-out drops the crossfade: {blend:.3e} against the enabled take"
        );
    }

    #[test]
    fn a_seek_into_the_ramp_reads_the_new_layer_on_the_next_block() {
        let font = two_layer_font();
        let project = seek_fixture(&font, false);
        let block = MAX_SOURCE_BLOCK_FRAMES;
        let whole = offline(&project, &font, SEEK_FRAME as usize + block, true);
        let reference = &whole[SEEK_FRAME as usize * 2..(SEEK_FRAME as usize + block) * 2];
        let streamed = streaming(&project, &font, block, SEEK_FRAME);
        let parity = max_diff(reference, &streamed);
        assert!(
            parity <= TOLERANCE,
            "the seeked block matches offline at that frame: {parity:.3e}"
        );
        let hard = offline(
            &seek_fixture(&font, true),
            &font,
            SEEK_FRAME as usize + block,
            false,
        );
        let hard_block = &hard[SEEK_FRAME as usize * 2..(SEEK_FRAME as usize + block) * 2];
        let blend = max_diff(reference, hard_block);
        let level = rms(reference).max(1e-9) as f32;
        println!(
            "seeked block parity {parity:.3e}, layer blend {blend:.3e} ({:.3e} relative)",
            blend / level
        );
        assert!(
            blend / level > AUDIBLE,
            "the seeked block blends layers instead of hard-selecting: {blend:.3e} at level {level:.3e}"
        );
    }
}

#[test]
fn a_crescendo_moves_the_sampled_layer_mix_while_the_level_rises() {
    let font = two_layer_font();
    let audio = render_with_font(&sampled_swell(&font, Some("@0.0 cresc"), None));
    let levels = ladder(&audio);
    let centroids = centroid_ladder(&audio);
    println!("level ladder: {levels:?}");
    println!("centroid ladder: {centroids:?}");
    for pair in levels.windows(2) {
        assert!(
            pair[1] > pair[0],
            "the sampled swell rises in every window: {levels:?}"
        );
    }
    assert!(
        levels[3] - levels[0] >= 12.0,
        "the sampled swell spans at least 12 dB: {} dB",
        levels[3] - levels[0]
    );
    for pair in centroids.windows(2) {
        assert!(
            pair[1] >= pair[0],
            "the layer mix moves upward with the ramp: {centroids:?}"
        );
    }
    assert!(
        centroids[3] - centroids[0] >= 400.0,
        "the sampled swell gets brighter: {centroids:?}"
    );
}

#[test]
fn disabling_dynamic_layers_keeps_the_centroid_flat() {
    let font = two_layer_font();
    let swelled = render_with_font(&sampled_swell(&font, Some("@0.0 cresc"), None));
    let track_off = render_with_font(&sampled_swell(&font, Some("@0.0 cresc"), Some("track")));
    let preset_off = render_with_font(&sampled_swell(&font, Some("@0.0 cresc"), Some("preset")));
    let swelled_levels = ladder(&swelled);
    for audio in [&track_off, &preset_off] {
        let levels = ladder(audio);
        let centroids = centroid_ladder(audio);
        println!("opt-out level ladder: {levels:?}");
        println!("opt-out centroid ladder: {centroids:?}");
        assert!(
            levels[3] - levels[0] >= 12.0,
            "the opt-out keeps the level ramp: {} dB",
            levels[3] - levels[0]
        );
        for (enabled, disabled) in swelled_levels.iter().zip(&levels) {
            assert!(
                (enabled - disabled).abs() < 1.0,
                "the opt-out keeps the same level law: {swelled_levels:?} vs {levels:?}"
            );
        }
        assert!(
            spread(&centroids) < 10.0,
            "the opt-out keeps the centroid flat: {centroids:?}"
        );
    }
}

// ---- Pattern hairpins and hit velocity --------------------------------------

/// Four whole notes in a named pattern, played by one clip. `None` is the flat
/// control take; the marked take lets the ramp carry the notes after the first.
fn pattern_project(mark: Option<&str>, first: &str, rest: &str) -> Project {
    let mark = mark.map(|mark| format!("    {mark}\n")).unwrap_or_default();
    let source = format!(
        r#"@ocp "0.2.0"
@project "pattern swell"
  tempo: 120
  time_signature: 4/4

@preset swell_pad
  source: builtin
  plugin: "builtin.open_synth"
  params: {{ osc.a.waveform: 1, osc.a.level: 0.8, amp.attack: 0.01, amp.decay: 0.1, amp.sustain: 0.9, amp.release: 0.05, filter.cutoff: 12000, filter.env.amount: 0, voice.polyphony: 8 }}

@pattern line | instrument: strings/violin, preset: swell_pad
  events:
{mark}    @0 C4 w {first}
    @4 C4 w {rest}
    @8 C4 w {rest}
    @12 C4 w {rest}

@track strings | preset: swell_pad, instrument: strings/violin
  clip: line [1:1 - 5:1]
"#
    );
    let project = oc_dsl::parse(&source).expect("the pattern swell fixture parses");
    oc_dsl::validate(&project).expect("the pattern swell fixture validates");
    project
}

#[test]
fn a_pattern_crescendo_raises_the_level_in_four_consecutive_windows() {
    assert_flat(&ladder(&render(&pattern_project(None, "pp", "pp"))));

    let swelled = ladder(&render(&pattern_project(Some("@0.0 cresc"), "pp", "")));
    for pair in swelled.windows(2) {
        assert!(
            pair[1] > pair[0],
            "every window is louder than the one before: {swelled:?}"
        );
    }
}

/// One hit through a builtin voice, so the measured level is the velocity the
/// hit path sent. The written dynamic is the same in both takes.
fn hit_project(velocity: Option<u8>, dynamic: &str) -> Project {
    let velocity = velocity
        .map(|value| format!(" v={value}"))
        .unwrap_or_default();
    let source = format!(
        r#"@ocp "0.2.0"
@project "hit velocity"
  tempo: 120
  time_signature: 4/4

@preset kit
  source: builtin
  plugin: "builtin.open_synth"
  params: {{ osc.a.waveform: 0, osc.a.level: 0.8, amp.attack: 0.001, amp.decay: 0.2, amp.sustain: 0.0, amp.release: 0.05, filter.cutoff: 12000, filter.env.amount: 0, voice.polyphony: 8 }}

@pattern beat | instrument: drums/acoustic_kit, preset: kit
  hits:
    kick 1:0 {dynamic}{velocity}

@track drums | preset: kit, instrument: drums/acoustic_kit
  clip: beat [1:1 - 2:1]
"#
    );
    let project = oc_dsl::parse(&source).expect("the hit fixture parses");
    oc_dsl::validate(&project).expect("the hit fixture validates");
    project
}

/// RMS over the first `seconds` of the render, where the hit is still sounding.
fn head_level(audio: &[f32], seconds: f64) -> f64 {
    let frames = ((seconds * f64::from(RATE)) as usize).min(audio.len() / 2);
    let mono: Vec<f32> = (0..frames)
        .map(|frame| {
            let left = audio.get(frame * 2).copied().unwrap_or(0.0);
            let right = audio.get(frame * 2 + 1).copied().unwrap_or(0.0);
            (left + right) * 0.5
        })
        .collect();
    rms(&mono)
}

#[test]
fn a_hit_exact_velocity_overrides_its_written_dynamic() {
    let exact = head_level(&render(&hit_project(Some(120), "mp")), 0.1);
    let written = head_level(&render(&hit_project(None, "mp")), 0.1);
    assert!(
        exact > written * 1.2,
        "v120 must be louder than mp: exact {exact}, written {written}"
    );
    assert!(
        written > 0.0,
        "the control hit sounds, so the comparison is not two silences"
    );
}
