//! Touch override acceptance: a held touch owns an automated parameter, and a
//! release hands it back to the lane within the fade window.

use std::sync::Arc;

use oc_audio::PluginTransport;
use oc_dsl::ast::Project;
use oc_engine::graph::AudioGraph;
use oc_engine::touch::{self, TouchRegistry};

const RATE: u32 = 48_000;
const BLOCK: usize = 240; // 5 ms, so touch edges land within one block
const TOTAL: usize = 5 * RATE as usize; // 5 s

/// A 1 Hz square lane on a track `gain` insert.
fn project() -> Project {
    oc_dsl::parse(
        r#"@project "touch override"
  tempo: 120
@track t | instrument: piano/acoustic_grand
  effects:
    - gain: factor=1
  automation: gain.factor {1:1=1, 1:2=0.25, 1:3=1, 1:4=0.25, 2:1=1, 2:2=0.25, 2:3=1, 2:4=0.25, 3:1=1, 3:2=0.25, 3:3=1, 3:4=0.25, 4:1=1, 4:2=0.25, 4:3=1, 4:4=0.25, 5:1=1}
"#,
    )
    .expect("acceptance project parses")
}

/// Render the track chain over unity DC so every output sample is the applied
/// gain. `script` runs before each block at the block's start frame.
fn render(
    project: &Project,
    registry: Option<&Arc<TouchRegistry>>,
    mut script: impl FnMut(u64, &TouchRegistry),
) -> Vec<f32> {
    let mut graph = match registry {
        Some(registry) => AudioGraph::try_from_project_with_source_latencies_and_touch(
            project,
            RATE,
            &[],
            Arc::clone(registry),
        )
        .unwrap(),
        None => AudioGraph::try_from_project_with_source_latencies(project, RATE, &[]).unwrap(),
    };
    let mut out = vec![0.0f32; TOTAL * 2];
    let mut position = 0u64;
    while (position as usize) < TOTAL {
        let frames = BLOCK.min(TOTAL - position as usize);
        if let Some(registry) = registry {
            script(position, registry);
        }
        let mut block = vec![1.0f32; frames * 2];
        graph.track_chains[0].process_with_transport(
            &mut block,
            RATE,
            Some(PluginTransport::new(position, RATE, 120.0)),
        );
        let start = position as usize * 2;
        out[start..start + frames * 2].copy_from_slice(&block);
        position += frames as u64;
    }
    out
}

#[test]
fn the_hand_holds_for_two_seconds_and_the_lane_returns_within_fifty_ms() {
    let project = project();
    let key = touch::effect_key("track:0", 0, "factor");
    let registry = Arc::new(TouchRegistry::new());
    const HOLD_START: u64 = 48_000; // 1 s
    const HOLD_END: u64 = 144_000; // 3 s, exactly on a lane step
    let hand = 0.1; // -20 dB
    let touched = render(&project, Some(&registry), |frame, registry| {
        if frame == HOLD_START {
            registry.begin(&key, hand);
        }
        if frame == HOLD_END {
            registry.release(&key);
        }
    });
    let lane = render(&project, None, |_, _| {});

    // Before the touch the lane is in charge.
    assert_eq!(
        &touched[..HOLD_START as usize * 2],
        &lane[..HOLD_START as usize * 2]
    );

    // Two seconds of held touch: every sample stays at the hand value.
    let held = &touched[HOLD_START as usize * 2..HOLD_END as usize * 2];
    assert!(
        held.iter().all(|sample| *sample == hand as f32),
        "held section must stay at -20 dB"
    );
    let lane_held = &lane[HOLD_START as usize * 2..HOLD_END as usize * 2];
    assert!(
        lane_held.iter().any(|sample| *sample != hand as f32),
        "the lane must actually be driving the parameter"
    );

    // The release fades back over 20 ms, so the lane is back within 50 ms.
    let returned_at = HOLD_END as usize + RATE as usize * 50 / 1000;
    for index in returned_at..TOTAL {
        assert_eq!(
            touched[index * 2],
            lane[index * 2],
            "sample {index}: the lane must own the parameter again"
        );
    }

    // No click: the release is an even ramp, so its largest adjacent-sample
    // step is one fade step of the 0.9 range. On unity DC that is the gain
    // step itself; the lane's own 0.75 step is never taken.
    let fade = touch::release_fade_samples(RATE) as usize;
    let window = &touched[HOLD_END as usize * 2..(HOLD_END as usize + fade) * 2];
    let step = (1.0 - hand) / (fade / 8) as f64;
    let mut worst = 0.0f64;
    // The stream is interleaved, so adjacent samples of one channel are two
    // apart; comparing neighbours would compare the two channels of a frame.
    for frame in window.windows(4) {
        let left = (f64::from(frame[2]) - f64::from(frame[0])).abs();
        let right = (f64::from(frame[3]) - f64::from(frame[1])).abs();
        worst = worst.max(left).max(right);
    }
    assert!(
        worst <= step + 1e-6,
        "release stepped {worst} per sample, one fade step is {step}"
    );
    assert!(worst < 0.008, "a 20 dB release must not click: {worst}");
}

#[test]
fn a_held_touch_suspends_the_lane_smoother_and_resumes_it_on_release() {
    let project = oc_dsl::parse(
        r#"@project "touch and smoothing"
  tempo: 120
@track t | instrument: piano/acoustic_grand
  effects:
    - gain: factor=1
  automation: gain.factor smoothing=100ms {1:1=0 curve=stairs, 2:1=1}
"#,
    )
    .expect("smoothed touch project parses");
    let key = touch::effect_key("track:0", 0, "factor");
    let registry = Arc::new(TouchRegistry::new());
    const HOLD_START: u64 = 48_000; // 1 s, before the 2:1 step at 96_000
    const HOLD_END: u64 = 144_000; // 3 s
    let hand = 0.5;
    let touched = render(&project, Some(&registry), |frame, registry| {
        if frame == HOLD_START {
            registry.begin(&key, hand);
        }
        if frame == HOLD_END {
            registry.release(&key);
        }
    });
    let lane = render(&project, None, |_, _| {});

    // While held the hand owns every sample.
    assert!(
        touched[HOLD_START as usize * 2..HOLD_END as usize * 2]
            .iter()
            .all(|sample| *sample == hand as f32),
        "the hand value must own the held span"
    );
    // The lane stepped from 0 to 1 during the hold. A smoother that kept
    // running would already be at the new target; the lane instead resumes
    // from where it stopped, so the 20 ms fade ends near the frozen value.
    let fade = touch::release_fade_samples(RATE) as usize;
    let after_fade = HOLD_END as usize + fade;
    assert!(
        touched[after_fade * 2] < 0.25,
        "the smoother resumed from its frozen state: {}",
        touched[after_fade * 2]
    );
    assert!(
        lane[after_fade * 2] > 0.99,
        "the untouched lane is already at the step value: {}",
        lane[after_fade * 2]
    );
    // The release is a ramp on the fixed fade grid: no sample steps further
    // than one 20 ms fade step.
    let bound = (1.0 - hand) / (fade / 8) as f64 + 1e-6;
    let mut worst = 0.0f64;
    for frame in HOLD_END as usize..after_fade {
        worst =
            worst.max((f64::from(touched[(frame + 1) * 2]) - f64::from(touched[frame * 2])).abs());
    }
    assert!(
        worst <= bound,
        "release stepped {worst} per sample, one fade step is {bound}"
    );
    // After the hand-back the lane's own 100 ms time constant finishes the
    // move; four time constants later it is within 2 % of the target.
    let settled = HOLD_END as usize + RATE as usize / 10 * 4;
    assert!(
        touched[settled * 2] > 0.98,
        "the lane smoothing finishes the return: {}",
        touched[settled * 2]
    );
}

#[test]
fn an_offline_render_ignores_a_held_touch() {
    let project = project();
    let key = touch::effect_key("track:0", 0, "factor");
    let registry = Arc::new(TouchRegistry::new());
    registry.begin(&key, 0.1);
    assert!(registry.is_held(&key));

    // The offline constructor never receives the registry.
    let reference = render(&project, None, |_, _| {});
    let offline = render(&project, None, |_, _| {});
    assert_eq!(offline, reference);

    // A live graph holding the touch differs, proving the touch state is live.
    let live = render(&project, Some(&registry), |_, _| {});
    assert!(live.contains(&0.1f32));
    assert!(live.iter().zip(&reference).any(|(a, b)| a != b));
}

#[test]
fn a_master_gain_clip_uses_the_same_arbitration() {
    let project = oc_dsl::parse(
        r#"@project "gain clip touch"
  tempo: 120
@master
  gain_automation: linear [1:1 - 5:1] {1:1=1, 1:3=0.25, 2:1=1, 2:3=0.25, 3:1=1}
"#,
    )
    .expect("gain clip project parses");
    let key = touch::gain_key("master");
    let registry = Arc::new(TouchRegistry::new());
    let mut graph = AudioGraph::try_from_project_with_source_latencies_and_touch(
        &project,
        RATE,
        &[],
        Arc::clone(&registry),
    )
    .unwrap();

    let mut graph_lane =
        AudioGraph::try_from_project_with_source_latencies(&project, RATE, &[]).unwrap();
    let block = 240;
    let mut held = Vec::new();
    let mut lane = Vec::new();
    let mut position = 0u64;
    while (position as usize) < TOTAL {
        if position == 48_000 {
            registry.begin(&key, 0.1);
        }
        if position == 144_000 {
            registry.release(&key);
        }
        let transport = Some(PluginTransport::new(position, RATE, 120.0));
        let mut a = vec![1.0f32; block * 2];
        let mut b = vec![1.0f32; block * 2];
        graph
            .master_chain
            .process_with_transport(&mut a, RATE, transport.clone());
        graph_lane
            .master_chain
            .process_with_transport(&mut b, RATE, transport);
        held.extend_from_slice(&a);
        lane.extend_from_slice(&b);
        position += block as u64;
    }

    assert!(
        held[48_000 * 2..144_000 * 2]
            .iter()
            .all(|sample| *sample == 0.1f32),
        "a held gain clip must stay at the hand value"
    );
    assert_eq!(
        &held[144_000 * 2 + 2_400 * 2..],
        &lane[144_000 * 2 + 2_400 * 2..],
        "the clip must own the gain again 50 ms after the release"
    );
}

/// The same arbitration covers an LFO lane: the hand owns the value while it
/// is held, and on release the lane resumes at the absolute phase it would
/// have had anyway. That is what a suspended lane clock buys: the wobble does
/// not restart under the hand.
#[test]
fn an_lfo_lane_keeps_its_phase_while_the_hand_holds_it() {
    let project = oc_dsl::parse(
        r#"@project "lfo touch"
  tempo: 120
@track t | instrument: piano/acoustic_grand
  effects:
    - gain: factor=1
  automation: gain.factor mode=lfo speed=1/2 {1:1=1, 1:2=0.25, 1:3=1, 1:4=0.25}
"#,
    )
    .expect("LFO touch project parses");
    let key = touch::effect_key("track:0", 0, "factor");
    let registry = Arc::new(TouchRegistry::new());
    const HOLD_START: u64 = 48_000;
    const HOLD_END: u64 = 144_000;
    let hand = 0.1;
    let touched = render(&project, Some(&registry), |frame, registry| {
        if frame == HOLD_START {
            registry.begin(&key, hand);
        }
        if frame == HOLD_END {
            registry.release(&key);
        }
    });
    let lane = render(&project, None, |_, _| {});

    assert_eq!(
        &touched[..HOLD_START as usize * 2],
        &lane[..HOLD_START as usize * 2],
        "the lane is in charge before the hold"
    );
    assert!(
        touched[HOLD_START as usize * 2..HOLD_END as usize * 2]
            .iter()
            .all(|sample| *sample == hand as f32),
        "a held LFO lane must stay at the hand value"
    );
    assert_eq!(
        &touched[HOLD_END as usize * 2 + 2_400 * 2..],
        &lane[HOLD_END as usize * 2 + 2_400 * 2..],
        "after the fade the LFO is back at its absolute phase, not restarted"
    );
    // The resumed section really is a wobble, not a constant.
    let resumed = &touched[HOLD_END as usize * 2 + 2_400 * 2..];
    let low = resumed.iter().copied().fold(f32::INFINITY, f32::min);
    let high = resumed.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    assert!(high - low > 0.5, "resumed wobble depth {}", high - low);
}
