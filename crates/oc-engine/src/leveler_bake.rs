//! Bake a track leveler's own gain decision into the track's gain automation.
//!
//! The dry pass renders the track with the leveler removed, steps the same
//! [`Leveler`] law over that signal, and writes one gain point per control
//! tick. The device is then removed, so the project carries the decision
//! instead of a realtime rider.

use std::collections::HashMap;
use std::path::Path;

use oc_dsl::ast::{
    AutomationPoint, BarBeat, Curve, EffectType, GainAutomation, GainInterpolation, ParamValue,
    Project, TimeRange, Track,
};
use oc_dsl::timing::ProjectClock;
use oc_dsp::leveler::Leveler;
use oc_host::clap_instance::ClapInstance;

use crate::dsp::AudioEffect;

/// A point survives thinning only when the lane reproduces it within this.
const THINNING_TOLERANCE_DB: f64 = 0.05;
/// Quarter-note ticks used to place control points; about 0.1 ms at 120 BPM.
const BAKE_PPQ: u32 = 4_800;
/// The device clamps its range at 24 dB, so a factor above this cannot occur.
const MAX_GAIN_FACTOR: f64 = 16.0;

/// What one bake wrote, for the CLI report and its tests.
#[derive(Debug, Clone)]
pub struct LevelerBake {
    pub track: String,
    pub sample_rate: u32,
    pub control_period_seconds: f64,
    pub control_ticks: usize,
    pub points_before_thinning: usize,
    pub points: usize,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub min_gain_db: f64,
    pub max_gain_db: f64,
    /// Device parameters the bake ran with, in dB or device units.
    pub params: Vec<(String, f64)>,
}

/// Bake `track_name`'s leveler into its gain lane and remove the device.
///
/// The leveler must be the track's last effect: the dry pass reads the chain
/// output, which is what the device sees only when nothing follows it.
pub fn bake(
    project: &mut Project,
    track_name: &str,
    sample_rate: u32,
    resource_root: Option<&Path>,
    soundfont: &Path,
    plugins: &mut HashMap<String, ClapInstance>,
) -> Result<LevelerBake, String> {
    if sample_rate == 0 {
        return Err("sample rate must be positive".into());
    }
    let index = project
        .tracks
        .iter()
        .position(|track| track.name == track_name)
        .ok_or_else(|| format!("track '{track_name}' not found"))?;
    if !has_content(project, index) {
        return Err(format!("track '{track_name}' has no clip to bake"));
    }
    // An authored lane carries the operator's decision; a bake is a new term,
    // never a replacement, so it is refused rather than merged silently.
    if !project.tracks[index].gain_automation.is_empty() {
        return Err(format!(
            "track '{track_name}' already has a gain lane; bake into a new lane?"
        ));
    }
    let (leveler_index, params) = leveler_params(&project.tracks[index], track_name)?;
    let dry = dry_project(project, index, leveler_index);
    let samples = render_dry(
        &dry,
        track_name,
        sample_rate,
        resource_root,
        soundfont,
        plugins,
    )?;
    let clock = ProjectClock::new(&project.properties)?;
    let law = run_law(&samples, sample_rate, &params)?;
    let lane = build_lane(&clock, &law)?;
    let track = &mut project.tracks[index];
    track.effects.remove(leveler_index);
    track.gain_automation = vec![lane];
    Ok(LevelerBake {
        track: track_name.to_string(),
        sample_rate,
        control_period_seconds: law.period_seconds,
        control_ticks: law.ticks,
        points_before_thinning: law.times.len(),
        points: law.taken.len(),
        start_seconds: law.start_seconds,
        end_seconds: law.end_seconds + law.period_seconds,
        min_gain_db: law.min_gain_db,
        max_gain_db: law.max_gain_db,
        params: law.params,
    })
}

/// True when the track carries audio, notes or a performance to ride.
fn has_content(project: &Project, index: usize) -> bool {
    let track = &project.tracks[index];
    !track.phrases.is_empty()
        || !track.patterns.is_empty()
        || !track.inline_patterns.is_empty()
        || track.pattern_clip_count() > 0
        || track.performance.is_some()
        || project
            .audio_clips()
            .iter()
            .any(|clip| clip.track == track.name)
}

/// Locate the single leveler and read the law it runs with.
fn leveler_params(track: &Track, name: &str) -> Result<(usize, Vec<(String, ParamValue)>), String> {
    let found: Vec<usize> = track
        .effects
        .iter()
        .enumerate()
        .filter(|(_, effect)| effect.effect_type == EffectType::Leveler)
        .map(|(index, _)| index)
        .collect();
    match found.as_slice() {
        [] => Err(format!("track '{name}' has no leveler effect to bake")),
        [single] => {
            if *single + 1 != track.effects.len() {
                let after = track.effects[*single + 1].effect_type.lane_name();
                return Err(format!(
                    "track '{name}': the leveler must be the last effect to bake; '{after}' follows it"
                ));
            }
            Ok((*single, track.effects[*single].params.clone()))
        }
        many => Err(format!(
            "track '{name}' has {} leveler effects; keep one to bake",
            many.len()
        )),
    }
}

/// The authored project with the leveler (and its own gain lane) removed and
/// every other track muted, so one track's chain output is captured alone.
fn dry_project(project: &Project, index: usize, leveler_index: usize) -> Project {
    let mut dry = project.clone();
    oc_dsl::instruments::apply_defaults(&mut dry);
    let track = &mut dry.tracks[index];
    track.effects.remove(leveler_index);
    track.gain_automation.clear();
    track.mix.volume_db = 0.0;
    track.mix.pan = 0.0;
    track.mix.mute = false;
    track.mix.solo = false;
    for (other, track) in dry.tracks.iter_mut().enumerate() {
        if other != index {
            track.mix.mute = true;
            track.mix.solo = false;
        }
    }
    for bus in &mut dry.buses {
        bus.solo = false;
    }
    if let Some(master) = &mut dry.master {
        master.loudness_target = None;
    }
    dry
}

/// Render the track's post-insert, pre-fader chain output.
fn render_dry(
    dry: &Project,
    track_name: &str,
    sample_rate: u32,
    resource_root: Option<&Path>,
    soundfont: &Path,
    plugins: &mut HashMap<String, ClapInstance>,
) -> Result<Vec<f32>, String> {
    let mut cache = crate::render_cache::RenderCache::new();
    cache.set_resource_root(resource_root.map(Path::to_path_buf));
    cache.capture_track_pre_fader();
    cache.render_full(dry, soundfont, sample_rate, plugins)?;
    let capture = cache
        .stems()
        .ok_or("the dry render produced no track capture")?;
    let samples = capture
        .tracks
        .iter()
        .find(|(name, _)| name == track_name)
        .map(|(_, samples)| samples.clone())
        .ok_or_else(|| format!("track '{track_name}' was not captured by the dry render"))?;
    if samples.is_empty() {
        return Err(format!("track '{track_name}' rendered no audio to bake"));
    }
    Ok(samples)
}

/// The device's decision at every control boundary, in dB.
struct Law {
    times: Vec<f64>,
    gains_db: Vec<f64>,
    taken: Vec<usize>,
    period_seconds: f64,
    ticks: usize,
    start_seconds: f64,
    end_seconds: f64,
    min_gain_db: f64,
    max_gain_db: f64,
    params: Vec<(String, f64)>,
}

/// Step the device law over the dry signal and read its gain at each tick.
fn run_law(
    samples: &[f32],
    sample_rate: u32,
    params: &[(String, ParamValue)],
) -> Result<Law, String> {
    let mut leveler = Leveler::new(sample_rate);
    let mut used = Vec::new();
    for (name, value) in params {
        if let Some(value) = crate::dsp::typed_builtin_value(&EffectType::Leveler, name, value) {
            leveler.set_param(name, value);
            used.push((name.clone(), value));
        }
    }
    let frames = samples.len() / 2;
    let period = leveler.control_frames().max(1);
    let period_seconds = period as f64 / f64::from(sample_rate);
    // targets[i] is the value decided at tick i; targets[0] is the initial gain.
    let mut targets = vec![leveler.current_gain_db()];
    let mut scratch = vec![0.0f32; period * 2];
    let mut offset = 0;
    while offset + period <= frames {
        scratch.copy_from_slice(&samples[offset * 2..(offset + period) * 2]);
        leveler.process(&mut scratch, sample_rate);
        targets.push(leveler.current_gain_db());
        offset += period;
    }
    let ticks = targets.len() - 1;
    // The value decided at a tick applies across the following period, so a
    // boundary carries the previous tick's value.
    let mut times: Vec<f64> = (0..targets.len())
        .map(|boundary| boundary as f64 * period_seconds)
        .collect();
    let mut gains_db: Vec<f64> = (0..targets.len())
        .map(|boundary| targets[boundary.saturating_sub(1)])
        .collect();
    let tail = frames - offset;
    if tail > 0 {
        let mut chunk = samples[offset * 2..].to_vec();
        leveler.process(&mut chunk, sample_rate);
        // The tail is inside the period the last tick started; the gain is
        // part way from the previous decision to the one it read.
        let last = targets.len() - 1;
        let (from, to) = if last == 0 {
            (targets[0], targets[0])
        } else {
            (targets[last - 1], targets[last])
        };
        let phase = tail as f64 / period as f64;
        let reached = amplitude(from) + (amplitude(to) - amplitude(from)) * phase;
        times.push(frames as f64 / f64::from(sample_rate));
        gains_db.push(20.0 * reached.log10());
    }
    let taken = thin(&times, &gains_db);
    let start_seconds = times[0];
    let end_seconds = *times.last().expect("a law has boundaries");
    let min_gain_db = gains_db.iter().copied().fold(f64::INFINITY, f64::min);
    let max_gain_db = gains_db.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok(Law {
        times,
        gains_db,
        taken,
        period_seconds,
        ticks,
        start_seconds,
        end_seconds,
        min_gain_db,
        max_gain_db,
        params: used,
    })
}

/// Greedy thinning: keep a point only when the lane cannot reproduce it within
/// the tolerance. The lane interpolates linear gain, so the test does too.
fn thin(times: &[f64], gains_db: &[f64]) -> Vec<usize> {
    let amplitudes: Vec<f64> = gains_db.iter().map(|db| amplitude(*db)).collect();
    let count = amplitudes.len();
    if count <= 2 {
        return (0..count).collect();
    }
    let mut kept = vec![0];
    let mut anchor = 0;
    for candidate in 2..count {
        let start = times[anchor];
        let span = times[candidate] - start;
        let from = amplitudes[anchor];
        let to = amplitudes[candidate];
        let fits = (anchor + 1..candidate).all(|index| {
            let phase = (times[index] - start) / span;
            let value = from + (to - from) * phase;
            20.0 * (value / amplitudes[index]).log10().abs() <= THINNING_TOLERANCE_DB
        });
        if !fits {
            kept.push(candidate - 1);
            anchor = candidate - 1;
        }
    }
    if kept.last() != Some(&(count - 1)) {
        kept.push(count - 1);
    }
    kept
}

/// The thin threshold is in dB, so compare amplitudes on a log scale.
fn amplitude(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Build the gain lane from the thinned decisions.
fn build_lane(clock: &ProjectClock, law: &Law) -> Result<GainAutomation, String> {
    let mut points: Vec<AutomationPoint> = Vec::with_capacity(law.taken.len());
    for &index in &law.taken {
        let position = position_at(clock, law.times[index], BAKE_PPQ)?;
        if let Some(point) = points.last_mut()
            && point.position == position
        {
            // Two decisions can land on one tick at extreme tempos.
            point.value = ParamValue::Number(factor(law.gains_db[index]));
            continue;
        }
        points.push(AutomationPoint {
            position,
            value: ParamValue::Number(factor(law.gains_db[index])),
            curve: Curve::Linear,
            tension: None,
        });
    }
    if !points.windows(2).all(|pair| {
        let before = clock.quarter_notes_at_ppq(&pair[0].position, BAKE_PPQ);
        let after = clock.quarter_notes_at_ppq(&pair[1].position, BAKE_PPQ);
        matches!((before, after), (Ok(before), Ok(after)) if after > before)
    }) {
        return Err("the bake produced out-of-order gain points".into());
    }
    let start = points
        .first()
        .map(|point| point.position)
        .ok_or("the bake produced no points")?;
    // The last decision holds through the buffer's tail, so the clip covers it.
    let end = position_at(clock, law.end_seconds + law.period_seconds, BAKE_PPQ)?;
    if end == start {
        return Err("the baked range is shorter than one notated tick".into());
    }
    Ok(GainAutomation {
        time_range: TimeRange { start, end },
        interpolation: GainInterpolation::Linear,
        ppq: BAKE_PPQ,
        points,
    })
}

/// A linear gain factor the DSL accepts, clamped to the validated range.
fn factor(db: f64) -> f64 {
    amplitude(db).clamp(0.0, MAX_GAIN_FACTOR)
}

/// Convert a time to the nearest PPQ subdivision of the project clock.
fn position_at(clock: &ProjectClock, seconds: f64, ppq: u32) -> Result<BarBeat, String> {
    let quarter = clock.quarter_at_seconds(seconds)?;
    let base = clock.bar_beat_floor_at_quarter(quarter)?;
    let signature = clock.meter_at_bar(base.bar)?;
    // Subdivisions count from the beat start, not from the floored 16th.
    let base_quarter = clock.quarter_notes_at(&BarBeat::new(base.bar, base.beat, 0))?;
    let per_beat = (u64::from(ppq) * 4 / u64::from(signature.denominator)).max(1);
    let subdivision = ((quarter - base_quarter) * f64::from(ppq)).round().max(0.0) as u64;
    if subdivision >= per_beat {
        return if base.beat < u32::from(signature.numerator) {
            Ok(BarBeat::new(base.bar, base.beat + 1, 0))
        } else {
            Ok(BarBeat::new(base.bar + 1, 1, 0))
        };
    }
    Ok(BarBeat::new(base.bar, base.beat, subdivision as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const RATE: u32 = 48_000;

    fn temp_dir(name: &str) -> PathBuf {
        oc_test_support::temp_dir("oc-leveler-bake", name)
    }

    /// A tone whose amplitude follows `envelope(seconds)`, interleaved stereo.
    fn take(seconds: f64, envelope: impl Fn(f64) -> f64) -> Vec<f32> {
        let frames = (f64::from(RATE) * seconds) as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for index in 0..frames {
            let time = index as f64 / f64::from(RATE);
            let value = (envelope(time) * (std::f64::consts::TAU * 220.0 * time).sin()) as f32;
            samples.push(value);
            samples.push(value);
        }
        samples
    }

    fn project(span: &str, effects: &str) -> String {
        format!(
            "@project \"Leveler bake\"\n  tempo: 120\n\
@audio_clip take [{span}] | track: vocal\n  file: \"take.wav\"\n\
@track vocal | instrument: audio/clip\n  mix: vol=0dB\n{effects}"
        )
    }

    fn render(project: &Project, root: &Path) -> Vec<f32> {
        crate::render::render_to_buffer_with_plugins_at(
            project,
            Path::new(""),
            RATE,
            &mut HashMap::new(),
            Some(root),
        )
        .unwrap()
    }

    fn rms_windows(samples: &[f32]) -> Vec<f64> {
        let frames = (f64::from(RATE) * 0.100) as usize;
        samples
            .chunks_exact(frames * 2)
            .map(|window| {
                let power = window
                    .iter()
                    .map(|sample| f64::from(*sample) * f64::from(*sample))
                    .sum::<f64>()
                    / window.len() as f64;
                10.0 * power.max(1e-30).log10()
            })
            .collect()
    }

    #[test]
    fn the_bake_matches_the_device_and_removes_the_leveler() {
        let dir = temp_dir("match");
        let seconds = 20.0;
        let samples = take(seconds, |time| {
            let db = -10.0 * time / seconds;
            0.5 * 10f64.powf(db / 20.0)
        });
        crate::wav::write_stereo_wav(dir.join("take.wav"), &samples, RATE).unwrap();
        let text = project(
            "1:1 - 11:1",
            "  effects:\n    - leveler: target=-18dB, speed=4, window=800ms, range=18, gate=-45dB, gate_release=1200ms\n",
        );
        let authored = oc_dsl::parse(&text).unwrap();
        oc_dsl::validate(&authored).unwrap();
        let with_device = render(&authored, &dir);

        let mut baked = authored.clone();
        let report = bake(
            &mut baked,
            "vocal",
            RATE,
            Some(&dir),
            Path::new(""),
            &mut HashMap::new(),
        )
        .unwrap();
        assert!(
            baked.tracks[0]
                .effects
                .iter()
                .all(|effect| effect.effect_type != EffectType::Leveler),
            "the baked project still carries a leveler"
        );
        assert_eq!(baked.tracks[0].gain_automation.len(), 1);
        assert!(report.points > 0, "the bake wrote no points");

        let baked_output = render(&baked, &dir);
        assert_eq!(with_device.len(), baked_output.len());
        let device = rms_windows(&with_device);
        let lane = rms_windows(&baked_output);
        assert_eq!(device.len(), lane.len());
        for (index, (device, lane)) in device.iter().zip(&lane).enumerate() {
            let delta = (device - lane).abs();
            assert!(
                delta <= 0.2,
                "window {index} is {delta:.3} dB apart ({device:.3} vs {lane:.3})"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_lane_is_thinned_monotone_and_survives_a_format_round_trip() {
        let dir = temp_dir("lane");
        let seconds = 20.0;
        let samples = take(seconds, |time| {
            // 4 s steady, 12 s of a 10 dB fade, 4 s steady at the new level.
            let fade = ((time - 4.0) / 12.0).clamp(0.0, 1.0);
            0.5 * 10f64.powf(-10.0 * fade / 20.0)
        });
        crate::wav::write_stereo_wav(dir.join("take.wav"), &samples, RATE).unwrap();
        let text = project(
            "1:1 - 11:1",
            "  effects:\n    - leveler: target=-18dB, speed=4, window=800ms, range=18, gate=-45dB, gate_release=1200ms\n",
        );
        let mut baked = oc_dsl::parse(&text).unwrap();
        oc_dsl::validate(&baked).unwrap();
        let report = bake(
            &mut baked,
            "vocal",
            RATE,
            Some(&dir),
            Path::new(""),
            &mut HashMap::new(),
        )
        .unwrap();
        assert!(report.points > 1, "a moving law needs several points");

        let lane = &baked.tracks[0].gain_automation[0];
        assert!(!lane.points.is_empty());
        assert!(
            report.points < report.points_before_thinning,
            "thinning kept every point ({} of {})",
            report.points,
            report.points_before_thinning
        );
        let clock = ProjectClock::new(&baked.properties).unwrap();
        let seconds_at = |point: &AutomationPoint| {
            let quarter = clock
                .quarter_notes_at_ppq(&point.position, lane.ppq)
                .unwrap();
            clock.seconds_at_quarter(quarter).unwrap()
        };
        let db_at = |point: &AutomationPoint| match point.value {
            ParamValue::Number(value) => 20.0 * value.log10(),
            _ => panic!("the lane stores linear factors"),
        };
        // Outside the fade the leveler is still settling, so compare the fade.
        let fade: Vec<f64> = lane
            .points
            .iter()
            .filter(|point| (4.0..=16.0).contains(&seconds_at(point)))
            .map(db_at)
            .collect();
        assert!(fade.len() > 4, "the fade kept only {} points", fade.len());
        for pair in fade.windows(2) {
            assert!(
                pair[1] >= pair[0] - 1e-9,
                "the baked gain moves back up during a monotone fade: {pair:?}"
            );
        }

        let formatted = oc_dsl::serialize(&baked);
        let reopened = oc_dsl::parse(&formatted).unwrap();
        oc_dsl::validate(&reopened).unwrap();
        assert_eq!(reopened, baked, "the baked lane did not round-trip");
        assert_eq!(oc_dsl::serialize(&reopened), formatted);
        assert_eq!(
            reopened.tracks[0].gain_automation[0].points.len(),
            lane.points.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_track_is_refused_by_name() {
        let mut project =
            oc_dsl::parse("@project\n@track vocal | instrument: audio/clip\n").unwrap();
        let error = bake(
            &mut project,
            "lead vocal",
            RATE,
            None,
            Path::new(""),
            &mut HashMap::new(),
        )
        .unwrap_err();
        assert!(error.contains("lead vocal"), "{error}");
        assert!(error.contains("not found"), "{error}");
    }

    #[test]
    fn a_track_without_a_clip_is_refused_by_name() {
        let mut project =
            oc_dsl::parse("@project\n@track vocal | instrument: audio/clip\n").unwrap();
        let error = bake(
            &mut project,
            "vocal",
            RATE,
            None,
            Path::new(""),
            &mut HashMap::new(),
        )
        .unwrap_err();
        assert!(error.contains("vocal"), "{error}");
        assert!(error.contains("no clip"), "{error}");
    }

    #[test]
    fn an_authored_gain_lane_is_refused_and_left_alone() {
        let text = project(
            "1:1 - 11:1",
            "  effects:\n    - leveler: target=-18dB, speed=4, window=800ms, range=18, gate=-45dB, gate_release=1200ms\n  gain_automation: linear [1:1 - 3:1] {1:1=0.5, 3:1=1}\n",
        );
        let mut authored = oc_dsl::parse(&text).unwrap();
        oc_dsl::validate(&authored).unwrap();
        let before = authored.tracks[0].clone();
        let error = bake(
            &mut authored,
            "vocal",
            RATE,
            None,
            Path::new(""),
            &mut HashMap::new(),
        )
        .unwrap_err();
        assert!(
            error.contains("already has a gain lane; bake into a new lane?"),
            "{error}"
        );
        assert_eq!(
            authored.tracks[0].gain_automation.len(),
            1,
            "the refusal left more than one lane"
        );
        assert_eq!(
            authored.tracks[0].gain_automation[0].points.len(),
            before.gain_automation[0].points.len(),
            "the refusal changed the authored points"
        );
        assert!(
            authored.tracks[0]
                .effects
                .iter()
                .any(|effect| effect.effect_type == EffectType::Leveler),
            "the refusal removed the leveler"
        );
    }
}
