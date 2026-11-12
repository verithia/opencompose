//! Post-insert, sample-accurate gain clips. Unity outside explicit clip ranges.
use crate::dsp::AudioEffect;
use crate::touch::{self, TouchLane, TouchPlan, TouchRegistry};
use oc_audio::{AudioProcessContext, PluginTransport};
use oc_dsl::ast::{BarBeat, GainAutomation, GainInterpolation, ParamValue, ProjectProperties};
use std::sync::Arc;

struct Clip {
    start: f64,
    end: f64,
    linear: bool,
    points: Vec<(f64, f64)>,
}

/// Live touch state for the gain stage of one bus or the master.
struct TouchHold {
    registry: Arc<TouchRegistry>,
    lane: TouchLane,
}

pub(super) struct GainEnvelope {
    clips: Vec<Clip>,
    cursor: u64,
    touch: Option<TouchHold>,
}

impl GainEnvelope {
    #[cfg(test)]
    pub(super) fn new(clips: &[GainAutomation], tempo: f64, sample_rate: u32) -> Self {
        let properties = ProjectProperties {
            tempo,
            ..Default::default()
        };
        Self::build(clips, &properties, sample_rate, None, None).expect("valid gain clock")
    }

    /// Build with live touch arbitration; offline callers use `with_project`.
    pub(super) fn with_project_touch(
        clips: &[GainAutomation],
        properties: &ProjectProperties,
        sample_rate: u32,
        owner: &str,
        touch: Option<Arc<TouchRegistry>>,
    ) -> Result<Self, String> {
        Self::build(clips, properties, sample_rate, Some(owner), touch)
    }

    fn build(
        clips: &[GainAutomation],
        properties: &ProjectProperties,
        sample_rate: u32,
        owner: Option<&str>,
        registry: Option<Arc<TouchRegistry>>,
    ) -> Result<Self, String> {
        let clock = oc_dsl::timing::ProjectClock::new(properties)?;
        let sample = |p: &BarBeat, ppq: u32| -> Result<f64, String> {
            Ok(clock.sample_at_quarter(clock.quarter_notes_at_ppq(p, ppq)?, sample_rate)? as f64)
        };
        let touch = match (owner, registry) {
            (Some(owner), Some(registry)) => Some(TouchHold {
                registry,
                lane: TouchLane::new(touch::gain_key(owner)),
            }),
            _ => None,
        };
        Ok(Self {
            cursor: 0,
            touch,
            clips: clips
                .iter()
                .map(|c| {
                    Ok(Clip {
                        start: sample(&c.time_range.start, c.ppq)?,
                        end: sample(&c.time_range.end, c.ppq)?,
                        linear: c.interpolation == GainInterpolation::Linear,
                        points: c
                            .points
                            .iter()
                            .map(|p| {
                                Ok((
                                    sample(&p.position, c.ppq)?,
                                    match p.value {
                                        ParamValue::Number(v) => v,
                                        _ => 1.0, // Public render admission validates numeric gain first.
                                    },
                                ))
                            })
                            .collect::<Result<_, String>>()?,
                    })
                })
                .collect::<Result<_, String>>()?,
        })
    }

    /// Write one span with the arbitration plan already decided.
    fn apply(&mut self, buffer: &mut [f32], plan: TouchPlan) {
        for (index, frame) in buffer.chunks_exact_mut(2).enumerate() {
            let lane = self.factor(self.cursor as f64);
            let gain = plan.value(lane, index as u32).unwrap_or(lane) as f32;
            frame[0] *= gain;
            frame[1] *= gain;
            self.cursor += 1;
        }
    }

    fn factor(&self, sample: f64) -> f64 {
        let Some(clip) = self
            .clips
            .iter()
            .find(|c| sample >= c.start && sample < c.end)
        else {
            return 1.0;
        };
        let index = clip
            .points
            .partition_point(|(position, _)| *position <= sample);
        if index == 0 {
            // A clip declares no start value, so the window before its first
            // point holds that point instead of muting the clip range.
            return clip.points.first().map_or(1.0, |(_, value)| *value);
        }
        let (t, v) = clip.points[index - 1];
        if clip.linear && index < clip.points.len() {
            let (next_t, next_v) = clip.points[index];
            v + (next_v - v) * (sample - t) / (next_t - t)
        } else {
            v
        }
    }
}

impl AudioEffect for GainEnvelope {
    fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
        if self.touch.is_none() {
            self.apply(buffer, TouchPlan::Lane);
            return;
        }
        let frames = buffer.len() / 2;
        let mut offset = 0;
        while offset < frames {
            let span = (frames - offset).min(touch::POLL_FRAMES as usize).max(1);
            let plan = {
                let hold = self.touch.as_mut().expect("touch checked above");
                let status = hold.registry.status(hold.lane.key());
                hold.lane.plan(status, span as u32, sample_rate)
            };
            let range = offset * 2..(offset + span) * 2;
            self.apply(&mut buffer[range], plan);
            offset += span;
        }
    }
    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        if let Some(transport) = context.transport {
            self.cursor = transport.sample_pos;
        }
        self.process(buffer, context.sample_rate);
    }
    fn process_with_transport(
        &mut self,
        buffer: &mut [f32],
        sample_rate: u32,
        transport: Option<PluginTransport>,
    ) {
        if let Some(transport) = transport {
            self.cursor = transport.sample_pos;
        }
        self.process(buffer, sample_rate);
    }
    fn reset(&mut self) {
        self.cursor = 0;
    }
    fn set_param(&mut self, _name: &str, _value: f64) {}
    fn get_param(&self, _name: &str) -> Option<f64> {
        None
    }
    fn name(&self) -> &str {
        "gain_automation"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::AudioGraph;

    fn project() -> oc_dsl::ast::Project {
        oc_dsl::parse(
            r#"@ocp "0.2.0"
@project "gain automation test"
  tempo: 120
@track a | instrument: piano/acoustic_grand
  mix: output=group
@bus group
  routes: [master]
  effects:
    - gain: factor=0.5
  gain_automation: linear [1:2 - 2:1] {1:2=0, 1:3=1}
@master
  gain_automation: step [1:1 - 2:1] {1:1=0.5, 1:4=1}
"#,
        )
        .unwrap()
    }

    #[test]
    fn bus_and_master_gain_are_post_insert_and_sample_accurate() {
        let p = project();
        let mut graph = AudioGraph::from_project(&p, 8);
        let out = graph.mix_processed(
            &mut [vec![1.0; 40]],
            Some(PluginTransport::new(0, 8, 120.0)),
        );
        let pan = std::f32::consts::FRAC_1_SQRT_2;
        for frame in 0..20 {
            let bus = match frame {
                0..=3 | 8..=19 => 1.0,
                4..=7 => (frame - 4) as f32 / 4.0,
                _ => unreachable!(),
            };
            let master = if frame < 12 { 0.5 } else { 1.0 };
            let expected = pan * 0.5 * bus * master;
            assert!((out[frame * 2] - expected).abs() < 1e-6, "frame {frame}");
            assert_eq!(out[frame * 2], out[frame * 2 + 1]);
        }
        // Stateless positioning makes seek independent of prior playback.
        let seek =
            graph.mix_processed(&mut [vec![1.0; 4]], Some(PluginTransport::new(6, 8, 120.0)));
        assert_eq!(seek, out[12..16]);
    }

    #[test]
    fn a_gain_clip_holds_its_first_point_and_accepts_one_on_its_end() {
        // The reported shape: a range whose first point is one bar inside it.
        let head = oc_dsl::parse(
            r#"@project
  tempo: 120
@track a | instrument: piano/acoustic_grand
  mix: output=group
@bus group
  routes: [master]
  gain_automation: linear [1:1 - 3:1] {1:3=1, 2:3=0.5}
@master
"#,
        )
        .unwrap();
        assert!(oc_dsl::validate(&head).is_ok());
        let mut graph = AudioGraph::from_project(&head, 8);
        // 4 frames per quarter: 1:3 is frame 8, 2:3 is frame 24.
        let out = graph.mix_processed(
            &mut [vec![1.0; 80]],
            Some(PluginTransport::new(0, 8, 120.0)),
        );
        let pan = std::f32::consts::FRAC_1_SQRT_2;
        for frame in 0..8 {
            assert!(
                (out[frame * 2] - pan).abs() < 1e-6,
                "frame {frame} was silent before the clip's first point"
            );
        }
        assert!((out[24 * 2] - pan * 0.5).abs() < 1e-6);

        let tail = oc_dsl::parse(
            r#"@project
  tempo: 120
@track a | instrument: piano/acoustic_grand
  mix: output=group
@bus group
  routes: [master]
  gain_automation: linear [1:1 - 3:1] {1:1=1, 3:1=0.5}
@master
"#,
        )
        .unwrap();
        assert!(
            oc_dsl::validate(&tail).is_ok(),
            "a point exactly on the range end is legal"
        );
        let mut graph = AudioGraph::from_project(&tail, 8);
        let out = graph.mix_processed(
            &mut [vec![1.0; 80]],
            Some(PluginTransport::new(0, 8, 120.0)),
        );
        // The end point is the ramp's target: the last frame inside the range
        // has almost reached it, and the clip is over after frame 31.
        assert!(out[31 * 2] < pan * 0.6 && out[31 * 2] > pan * 0.5);
        assert!((out[32 * 2] - pan).abs() < 1e-6);
    }

    #[test]
    fn mapped_meter_ppq_ramp_and_seek_follow_integrated_sample_positions() {
        let p = oc_dsl::parse(
            r#"@project
  tempo: 120
  time_signature: 7/8
  tempo_map:
    1:3 = 60 bpm
  time_sig_map:
    2:1 = 3/4
@track a | instrument: piano/acoustic_grand
  mix: output=group
@bus group
  routes: [master]
  effects:
    - gain: factor=0.5
  gain_automation: linear ppq=48 [1:2 - 2:3] {1:2=0, 2:2=1}
@master
  gain_automation: step ppq=48 [1:1 - 2:3] {1:1=0.5, 2:1=1}
"#,
        )
        .unwrap();
        assert!(oc_dsl::validate(&p).is_ok());
        let mut graph = AudioGraph::try_from_project_with_source_latencies(&p, 8, &[]).unwrap();
        let out = graph.mix_processed(
            &mut [vec![1.0; 88]],
            Some(PluginTransport::new(0, 8, 120.0)),
        );
        // q0.5 -> sample2; q3.5/new bar2 -> sample24; q4.5 ->32;
        // end q5.5 ->40. Linear ramps retain interpolation in audio time.
        for frame in 0..44 {
            let bus = if !(2..40).contains(&frame) {
                1.0
            } else if frame < 32 {
                (frame - 2) as f32 / 30.0
            } else {
                1.0
            };
            let master = if frame < 24 { 0.5 } else { 1.0 };
            let expected = std::f32::consts::FRAC_1_SQRT_2 * 0.5 * bus * master;
            assert!((out[frame * 2] - expected).abs() < 1e-6, "frame {frame}");
        }
        let seek = graph.mix_processed(
            &mut [vec![1.0; 8]],
            Some(PluginTransport::new(23, 8, 120.0)),
        );
        assert_eq!(seek, out[46..54]);
        let mut invalid = p.clone();
        invalid.buses[0].gain_automation[0].points[0]
            .position
            .subdivision = 24;
        assert!(
            oc_dsl::validate(&invalid).is_err(),
            "24/48 quarter is a whole eighth beat, not a subdivision"
        );
        invalid.buses[0].gain_automation[0].points[0]
            .position
            .subdivision = 3;
        assert!(oc_dsl::validate(&invalid).is_ok());
        let clock = oc_dsl::timing::ProjectClock::new(&p.properties).unwrap();
        assert_eq!(
            clock
                .quarter_notes_at_ppq(&BarBeat::new(1, 2, 3), 48)
                .unwrap(),
            0.5625
        );
    }

    #[test]
    fn inherited_preset_effects_reach_track_graph_once() {
        let p = oc_dsl::parse("@project\n@preset parent\n  effects:\n    - gain: factor=0.5\n@preset child\n  extends: parent\n  effects:\n    - gain: factor=0.25\n@track a | preset: child, instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=0.5\n").unwrap();
        assert!(oc_dsl::validate(&p).is_ok());
        let mut graph = AudioGraph::from_project(&p, 48000);
        assert_eq!(graph.track_chains[0].effects.len(), 3);
        let result = graph.mix(&mut [vec![1.0; 4]]);
        let expected = std::f32::consts::FRAC_1_SQRT_2 * 0.5 * 0.25 * 0.5;
        assert!(result.iter().all(|v| (*v - expected).abs() < 1e-6));
    }

    #[test]
    fn fine_resolution_keeps_sub_sixteenth_points() {
        let p = oc_dsl::parse("@project\n  tempo: 120\n@master\n  gain_automation: linear ppq=48 [1:1 - 1:2] {1:1:0=0, 1:1:3=1}\n").unwrap();
        assert!(oc_dsl::validate(&p).is_ok());
        let mut envelope = GainEnvelope::new(&p.master.unwrap().gain_automation, 120.0, 96);
        let mut audio = vec![1.0; 10];
        envelope.process(&mut audio, 96);
        for (i, expected) in [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0, 1.0]
            .into_iter()
            .enumerate()
        {
            assert!((audio[i * 2] - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn envelope_block_partition_and_reset_are_exact() {
        let p = project();
        let clips = &p.buses[0].gain_automation;
        let mut whole = GainEnvelope::new(clips, 120.0, 8);
        let mut expected = vec![1.0; 40];
        whole.process(&mut expected, 8);
        let mut chunked = GainEnvelope::new(clips, 120.0, 8);
        let mut actual = vec![1.0; 40];
        for block in actual.chunks_mut(6) {
            chunked.process(block, 8);
        }
        assert_eq!(actual, expected);
        chunked.reset();
        actual.fill(1.0);
        chunked.process(&mut actual, 8);
        assert_eq!(actual, expected);
    }
}
