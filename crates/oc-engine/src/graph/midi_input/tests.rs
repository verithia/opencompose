use super::super::effect_blocks::process_effect_context;
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

#[test]
fn midi_wrapper_forwards_prepared_parameter_identity_without_named_fallback() {
    struct ParameterRecorder {
        owner: oc_audio::PreparedParameterOwner,
        value: f64,
    }
    impl AudioEffect for ParameterRecorder {
        fn process(&mut self, buffer: &mut [f32], _: u32) {
            buffer.fill(self.value as f32);
        }
        fn prepare_parameter(&mut self, _: &str) -> Option<oc_audio::PreparedParameterHandle> {
            self.owner.bind(0)
        }
        fn set_prepared_parameter(
            &mut self,
            handle: oc_audio::PreparedParameterHandle,
            value: f64,
        ) -> oc_audio::PreparedParameterOutcome {
            assert_eq!(self.owner.resolve(handle), Ok(0));
            self.value = value;
            oc_audio::PreparedParameterOutcome::Applied
        }
        fn set_param(&mut self, _: &str, _: f64) {
            panic!("prepared MIDI route must not resolve names");
        }
        fn get_param(&self, _: &str) -> Option<f64> {
            Some(self.value)
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "prepared MIDI fixture"
        }
    }
    let inner = ParameterRecorder {
        owner: oc_audio::PreparedParameterOwner::new().unwrap(),
        value: 0.,
    };
    let mut effect =
        RoutedEffect::new(Box::new(inner), Schedule::new(Vec::new(), 8, 1024).unwrap());
    let handle = effect.prepare_parameter("gain").unwrap();
    assert_eq!(
        effect.set_prepared_parameter(handle, 0.4),
        oc_audio::PreparedParameterOutcome::Applied
    );
    assert_eq!(effect.get_param("gain"), Some(0.4));
}

struct Trace {
    count: AtomicUsize,
    events: [AtomicU64; 4096],
}
impl Trace {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            count: AtomicUsize::new(0),
            events: std::array::from_fn(|_| AtomicU64::new(0)),
        })
    }
    fn messages(&self) -> Vec<(u64, [u8; 3])> {
        (0..self.count.load(Ordering::Relaxed))
            .map(|i| {
                let value = self.events[i].load(Ordering::Relaxed);
                (
                    value >> 24,
                    [(value >> 16) as u8, (value >> 8) as u8, value as u8],
                )
            })
            .collect()
    }
}
struct Recorder {
    trace: Arc<Trace>,
    pending: [Option<MidiInputEvent>; 1024],
    count: usize,
    active: [[bool; 128]; 16],
    honors_channel_mode: bool,
}
impl Recorder {
    fn new(trace: Arc<Trace>) -> Self {
        Self {
            trace,
            pending: [None; 1024],
            count: 0,
            active: [[false; 128]; 16],
            honors_channel_mode: true,
        }
    }
}
impl AudioEffect for Recorder {
    fn midi_input_capacity(&self) -> usize {
        1024
    }
    fn max_block_frames(&self) -> u32 {
        8
    }
    fn queue_midi_input(&mut self, event: MidiInputEvent) -> Result<(), MidiInputError> {
        if self.count == self.pending.len() {
            return Err(MidiInputError::CapacityExceeded);
        }
        event.message.bytes()?;
        self.pending[self.count] = Some(event);
        self.count += 1;
        Ok(())
    }
    fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
        let base = context.transport.map_or(0, |t| t.sample_pos);
        for i in 0..self.count {
            let e = self.pending[i].unwrap();
            let b = e.message.bytes().unwrap();
            assert!((e.sample_offset as usize) < buffer.len() / 2);
            let index = self.trace.count.fetch_add(1, Ordering::Relaxed);
            self.trace.events[index].store(
                ((base + u64::from(e.sample_offset)) << 24)
                    | (u64::from(b[0]) << 16)
                    | (u64::from(b[1]) << 8)
                    | u64::from(b[2]),
                Ordering::Relaxed,
            );
        }
        for (frame, out) in buffer.chunks_exact_mut(2).enumerate() {
            for i in 0..self.count {
                let e = self.pending[i].unwrap();
                if e.sample_offset as usize != frame {
                    continue;
                }
                match e.message {
                    MidiInputMessage::NoteOn {
                        channel,
                        key,
                        velocity,
                    } => self.active[channel as usize][key as usize] = velocity != 0,
                    MidiInputMessage::NoteOff { channel, key } => {
                        self.active[channel as usize][key as usize] = false
                    }
                    MidiInputMessage::ControlChange {
                        channel,
                        controller: 120,
                        ..
                    } if self.honors_channel_mode => self.active[channel as usize].fill(false),
                    _ => {}
                }
            }
            out.fill(if self.active.iter().flatten().any(|&x| x) {
                1.
            } else {
                0.
            });
        }
        self.count = 0;
    }
    fn process(&mut self, b: &mut [f32], r: u32) {
        self.process_with_context(
            b,
            AudioProcessContext {
                sample_rate: r,
                transport: None,
                input: AudioProcessInput::Main,
            },
        );
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {
        self.count = 0;
        self.active.fill([false; 128]);
    }
    fn name(&self) -> &str {
        "MIDI input recorder"
    }
}
fn on(at: u64, key: u8) -> Timed {
    Timed {
        at,
        message: MidiInputMessage::NoteOn {
            channel: 2,
            key,
            velocity: 100,
        },
    }
}
fn off(at: u64, key: u8) -> Timed {
    Timed {
        at,
        message: MidiInputMessage::NoteOff { channel: 2, key },
    }
}
fn routed(events: Vec<Timed>) -> (RoutedEffect, Arc<Trace>) {
    let trace = Trace::new();
    let inner = Box::new(Recorder::new(trace.clone()));
    let schedule = Schedule::new(events, 8, 1024).unwrap();
    (RoutedEffect::new(inner, schedule), trace)
}
fn process(effect: &mut RoutedEffect, start: u64, buffer: &mut [f32]) {
    let transport = PluginTransport::new(start, 1000, 60.);
    process_effect_context(
        effect,
        buffer,
        1000,
        Some(&transport),
        AudioProcessInput::Main,
    );
}
#[test]
fn midi_offsets_survive_native_block_splitting_without_heap_operations() {
    let (mut effect, trace) = routed(vec![on(2, 60), off(7, 60), on(8, 64), off(17, 64)]);
    let mut buffer = [0.; 48];
    let operations = crate::test_alloc::operations(|| process(&mut effect, 0, &mut buffer));
    assert_eq!(operations, 0);
    for (i, frame) in buffer.chunks_exact(2).enumerate() {
        assert_eq!(
            frame,
            [if (2..7).contains(&i) || (8..17).contains(&i) {
                1.
            } else {
                0.
            }; 2]
        );
    }
    let notes: Vec<_> = trace
        .messages()
        .into_iter()
        .filter(|(_, b)| b[0] & 0xf0 == 0x90 || b[0] & 0xf0 == 0x80)
        .collect();
    assert_eq!(
        notes,
        vec![
            (2, [0x92, 60, 100]),
            (7, [0x82, 60, 0]),
            (8, [0x92, 64, 100]),
            (17, [0x82, 64, 0])
        ]
    );
}
#[test]
fn seek_chases_held_notes_clears_old_ones_and_honors_release_boundaries() {
    let (mut effect, _) = routed(vec![on(2, 60), off(30, 60), on(40, 64), off(50, 64)]);
    let mut b = [0.; 16];
    let operations = crate::test_alloc::operations(|| {
        process(&mut effect, 10, &mut b);
        assert!(b.iter().all(|&v| v == 1.));
        process(&mut effect, 30, &mut b);
        assert!(b.iter().all(|&v| v == 0.));
        process(&mut effect, 0, &mut b);
    });
    assert_eq!(operations, 0);
    assert!(b[..4].iter().all(|&v| v == 0.));
    assert!(b[4..].iter().all(|&v| v == 1.));
    effect.reset();
    process(&mut effect, 40, &mut b);
    assert!(b.iter().all(|&v| v == 1.));
}
#[test]
fn midi_follows_audio_input_latency_without_changing_note_durations() {
    let (mut effect, trace) = routed(vec![on(0, 60), off(5, 60)]);
    effect.configure_midi_input_latency(6);
    let mut b = [0.; 32];
    process(&mut effect, 0, &mut b);
    for (i, frame) in b.chunks_exact(2).enumerate() {
        assert_eq!(frame, [if (6..11).contains(&i) { 1. } else { 0. }; 2]);
    }
    let notes: Vec<_> = trace
        .messages()
        .into_iter()
        .filter(|(_, b)| b[0] & 0xf0 == 0x90 || b[0] & 0xf0 == 0x80)
        .collect();
    assert_eq!(notes, vec![(6, [0x92, 60, 100]), (11, [0x82, 60, 0])]);
}
#[test]
fn sustain_and_controller_chase_restore_released_but_held_notes() {
    let cc = |at, controller, value| Timed {
        at,
        message: MidiInputMessage::ControlChange {
            channel: 2,
            controller,
            value,
        },
    };
    let schedule = Schedule::new(
        vec![
            cc(0, 64, 127),
            cc(0, 7, 127),
            cc(1, 7, 90),
            on(2, 60),
            off(4, 60),
            cc(20, 64, 0),
        ],
        8,
        1024,
    )
    .unwrap();
    let mut chased = Vec::new();
    schedule
        .chase(10, |e| {
            chased.push(e);
            Ok(())
        })
        .unwrap();
    let note = chased
        .iter()
        .position(|m| matches!(m, MidiInputMessage::NoteOn { key: 60, .. }))
        .unwrap();
    assert!(chased[..note].contains(&cc(0, 64, 127).message));
    assert_eq!(chased[note + 1], off(4, 60).message);
    chased.clear();
    schedule
        .chase(20, |e| {
            chased.push(e);
            Ok(())
        })
        .unwrap();
    assert!(
        !chased
            .iter()
            .any(|m| matches!(m, MidiInputMessage::NoteOn { .. }))
    );
}
#[test]
fn invalid_sources_and_excessive_bursts_fail_before_publication() {
    assert!(Schedule::new(vec![on(5, 60), off(4, 60)], 8, 1024).is_err());
    assert!(Schedule::new((0..1100).map(|_| on(0, 60)).collect(), 8, 1024).is_err());
    assert!(
        Schedule::new(
            vec![Timed {
                at: 0,
                message: MidiInputMessage::NoteOn {
                    channel: 16,
                    key: 60,
                    velocity: 1
                }
            }],
            8,
            1024
        )
        .is_err()
    );
    let p = oc_dsl::parse(
        "@project\n@track control | instrument: midi/out\n  @phrase notes [1:1 - 2:1]\n    C4 q\n",
    )
    .unwrap();
    assert!(source_events(&p, "missing", 1000).is_err());
    let events = source_events(&p, "control", 1000).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| matches!(
        e.message,
        MidiInputMessage::NoteOn { .. } | MidiInputMessage::NoteOff { .. }
    )));
}
#[test]
fn source_edits_and_mute_change_the_route_schedule() {
    let before = oc_dsl::parse(
        "@project\n@track notes | instrument: midi/out\n  @phrase p [1:1 - 2:1]\n    C4 q\n",
    )
    .unwrap();
    let mut after = before.clone();
    let routes = vec![oc_dsl::ast::InputRoute {
        kind: oc_dsl::ast::RouteKind::Midi,
        source: "notes".into(),
        target: "fx1".into(),
    }];
    assert!(!source_changed(&before, &after, &routes, 1000).unwrap());
    after.tracks[0].mix.mute = true;
    assert!(source_changed(&before, &after, &routes, 1000).unwrap());
    assert!(source_events(&after, "notes", 1000).unwrap().is_empty());
}

#[test]
fn pause_and_resume_chase_without_sticking_or_doubling_notes() {
    let (mut effect, _) = routed(vec![on(0, 60), off(40, 60)]);
    let mut buffer = [0.; 16];
    process(&mut effect, 0, &mut buffer);
    let mut paused = PluginTransport::new(8, 1000, 60.);
    paused.playing = false;
    let operations = crate::test_alloc::operations(|| {
        process_effect_context(
            &mut effect,
            &mut buffer,
            1000,
            Some(&paused),
            AudioProcessInput::Main,
        );
        assert!(buffer.iter().all(|&sample| sample == 0.));
        process(&mut effect, 8, &mut buffer);
    });
    assert_eq!(operations, 0);
    assert!(buffer.iter().all(|&sample| sample == 1.));
}

#[test]
fn reset_controllers_does_not_chase_stale_controller_values_or_sustained_notes() {
    let cc = |at, controller, value| Timed {
        at,
        message: MidiInputMessage::ControlChange {
            channel: 2,
            controller,
            value,
        },
    };
    let schedule = Schedule::new(
        vec![
            cc(0, 64, 127),
            cc(0, 7, 127),
            cc(1, 7, 90),
            on(2, 60),
            off(4, 60),
            cc(10, 121, 0),
        ],
        8,
        1024,
    )
    .unwrap();
    let mut messages = Vec::new();
    schedule
        .chase(11, |message| {
            messages.push(message);
            Ok(())
        })
        .unwrap();
    assert!(messages.contains(&cc(1, 7, 90).message));
    assert!(
        !messages
            .iter()
            .any(|m| matches!(m, MidiInputMessage::NoteOn { .. }))
    );
}

#[test]
fn source_clock_channel_and_rename_are_preserved_by_projection() {
    let text = "@project\n  tempo: 120\n  tempo_map: 1:3 = 60 bpm\n@track audio | instrument: audio/clip\n@track notes | instrument: midi/out, channel: 2\n  @phrase p [2:1 - 3:1]\n    C4 q v=100\n";
    let project = oc_dsl::parse(text).unwrap();
    let messages = source_events(&project, "notes", 1000).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0], on(3000, 60));
    assert_eq!(messages[1], off(4000, 60));
    let mut renamed = project.clone();
    renamed.tracks[1].name = "renamed".into();
    let routes = vec![oc_dsl::ast::InputRoute {
        kind: oc_dsl::ast::RouteKind::Midi,
        source: "renamed".into(),
        target: "fx1".into(),
    }];
    assert!(source_changed(&project, &renamed, &routes, 1000).unwrap());
}

#[test]
fn equal_timestamp_controller_resets_keep_authored_event_order() {
    let cc = |controller, value| Timed {
        at: 0,
        message: MidiInputMessage::ControlChange {
            channel: 2,
            controller,
            value,
        },
    };
    for (events, retained) in [
        (vec![cc(1, 90), cc(121, 0)], false),
        (vec![cc(121, 0), cc(1, 90)], true),
    ] {
        let schedule = Schedule::new(events, 8, 1024).unwrap();
        let mut output = Vec::new();
        schedule
            .chase(1, |message| {
                output.push(message);
                Ok(())
            })
            .unwrap();
        assert_eq!(output.contains(&cc(1, 90).message), retained);
    }
    assert!(
        Schedule::new(
            vec![Timed {
                at: 10,
                message: MidiInputMessage::ProgramChange {
                    channel: 2,
                    program: 4
                }
            }],
            8,
            1024
        )
        .is_err()
    );
}

#[test]
fn unqualified_pedals_and_parameter_sequences_refuse_preparation() {
    for controller in [6, 38, 66, 69, 96, 97, 98, 99, 100, 101] {
        let event = Timed {
            at: 0,
            message: MidiInputMessage::ControlChange {
                channel: 2,
                controller,
                value: 127,
            },
        };
        assert!(
            Schedule::new(vec![event], 8, 1024).is_err(),
            "CC {controller}"
        );
    }
}

#[test]
fn explicit_note_releases_cover_pause_seek_reset_and_channel_mode_ignoring_processors() {
    let trace = Trace::new();
    let mut recorder = Recorder::new(trace.clone());
    recorder.honors_channel_mode = false;
    let mut effect = RoutedEffect::new(
        Box::new(recorder),
        Schedule::new(
            vec![
                on(0, 60),
                on(0, 60),
                on(0, 64),
                Timed {
                    at: 40,
                    message: MidiInputMessage::ControlChange {
                        channel: 2,
                        controller: 123,
                        value: 0,
                    },
                },
                on(60, 67),
                off(100, 67),
            ],
            8,
            1024,
        )
        .unwrap(),
    );
    let mut b = [0.; 16];
    let operations = crate::test_alloc::operations(|| {
        process(&mut effect, 0, &mut b);
        assert!(b.iter().all(|&v| v == 1.));
        let mut transport = PluginTransport::new(8, 1000, 60.);
        transport.playing = false;
        process_effect_context(
            &mut effect,
            &mut b,
            1000,
            Some(&transport),
            AudioProcessInput::Main,
        );
        assert!(b.iter().all(|&v| v == 0.));
        process(&mut effect, 8, &mut b);
        assert!(b.iter().all(|&v| v == 1.));
        process(&mut effect, 40, &mut b);
        assert!(b.iter().all(|&v| v == 0.));
        process(&mut effect, 60, &mut b);
        assert!(b.iter().all(|&v| v == 1.));
        process(&mut effect, 110, &mut b);
        assert!(b.iter().all(|&v| v == 0.));
        process(&mut effect, 60, &mut b);
        effect.reset();
        process(&mut effect, 110, &mut b);
        assert!(b.iter().all(|&v| v == 0.));
    });
    assert_eq!(operations, 0);
    let messages = trace.messages();
    assert_eq!(
        messages
            .iter()
            .filter(|(at, bytes)| *at == 8 && *bytes == [0x82, 60, 0])
            .count(),
        2
    );
}
