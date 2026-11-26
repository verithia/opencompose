mod prepared_parameter_tests {
    use super::*;
    use oc_audio::{PreparedParameterError, PreparedParameterOwner};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    type Write = (usize, u64, u32);
    type Segment = (u64, usize, [u32; 2], u8, usize);

    struct Trace {
        resolved: AtomicUsize,
        named: AtomicUsize,
        failed: AtomicBool,
        invalidate: AtomicBool,
        fail_after_write: AtomicBool,
        writes: Mutex<Vec<Write>>,
        segments: Mutex<Vec<Segment>>,
    }

    impl Trace {
        fn new() -> Arc<Self> {
            let trace = Arc::new(Self {
                resolved: AtomicUsize::new(0),
                named: AtomicUsize::new(0),
                failed: AtomicBool::new(false),
                invalidate: AtomicBool::new(false),
                fail_after_write: AtomicBool::new(false),
                writes: Mutex::new(Vec::with_capacity(8192)),
                segments: Mutex::new(Vec::with_capacity(8192)),
            });
            // The recorder's platform mutex storage is prepared with its trace
            // buffers, so instrumentation does not allocate on the first write.
            drop(trace.writes.lock().unwrap());
            drop(trace.segments.lock().unwrap());
            trace
        }
    }

    struct BoundRecorder {
        owner: PreparedParameterOwner,
        enabled: bool,
        values: [f32; 2],
        trace: Arc<Trace>,
    }

    impl BoundRecorder {
        fn write(&mut self, slot: usize, value: f64) {
            self.values[slot] = value as f32;
            self.trace.writes.lock().unwrap().push((
                slot,
                value.to_bits(),
                (value as f32).to_bits(),
            ));
        }
    }

    impl AudioEffect for BoundRecorder {
        fn process(&mut self, buffer: &mut [f32], sample_rate: u32) {
            self.process_with_context(
                buffer,
                AudioProcessContext {
                    sample_rate,
                    transport: None,
                    input: AudioProcessInput::Main,
                },
            );
        }
        fn process_with_context(&mut self, buffer: &mut [f32], context: AudioProcessContext<'_>) {
            let (kind, key_len) = match context.input {
                AudioProcessInput::Main => (0, 0),
                AudioProcessInput::Sidechain(key) => (1, key.len()),
                AudioProcessInput::Control(key) => (2, key.len()),
            };
            self.trace.segments.lock().unwrap().push((
                context.transport.map_or(0, |t| t.sample_pos),
                buffer.len() / 2,
                self.values.map(f32::to_bits),
                kind,
                key_len,
            ));
            for value in buffer {
                *value = *value * self.values[0] + self.values[1];
            }
        }
        fn prepare_parameter(&mut self, name: &str) -> Option<PreparedParameterHandle> {
            if !self.enabled {
                return None;
            }
            self.trace.resolved.fetch_add(1, Ordering::Relaxed);
            self.owner.bind(if name == "a" { 0 } else { 1 })
        }
        fn set_prepared_parameter(
            &mut self,
            handle: PreparedParameterHandle,
            value: f64,
        ) -> PreparedParameterOutcome {
            if self.trace.invalidate.swap(false, Ordering::Relaxed) {
                self.owner.invalidate();
            }
            let slot = match self.owner.resolve(handle) {
                Ok(slot) if slot < 2 => slot as usize,
                _ => {
                    self.trace.failed.store(true, Ordering::Relaxed);
                    return PreparedParameterOutcome::Invalidated(
                        PreparedParameterError::Invalidated,
                    );
                }
            };
            self.write(slot, value);
            if self.trace.fail_after_write.swap(false, Ordering::Relaxed) {
                self.trace.failed.store(true, Ordering::Relaxed);
                return PreparedParameterOutcome::Failed;
            }
            PreparedParameterOutcome::Applied
        }
        fn set_param(&mut self, name: &str, value: f64) {
            self.trace.named.fetch_add(1, Ordering::Relaxed);
            self.write(if name == "a" { 0 } else { 1 }, value);
        }
        fn processing_failed(&self) -> bool {
            self.trace.failed.load(Ordering::Relaxed)
        }
        fn get_param(&self, name: &str) -> Option<f64> {
            match name {
                "a" => Some(f64::from(self.values[0])),
                "b" => Some(f64::from(self.values[1])),
                _ => None,
            }
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "prepared parameter recorder"
        }
    }

    fn make(enabled: bool, touch: Option<Arc<TouchRegistry>>) -> (AutomatedEffect, Arc<Trace>) {
        let trace = Trace::new();
        let a = lane(
            "fx1.a",
            96000,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(0.25)),
                (BarBeat::new(1, 1, 512), ParamValue::Number(0.25 + 1e-8)),
            ],
        );
        let b = lane_with(
            "fx1.b",
            96000,
            2.0,
            &[
                (BarBeat::new(1, 1, 0), ParamValue::Number(0.1)),
                (BarBeat::new(1, 1, 512), ParamValue::Number(0.3)),
            ],
        );
        let p = project("@project \"prepared\"\n  tempo: 120\n");
        let compiled = compile(
            &plugin_effect(),
            &[&a, &b],
            &p.properties,
            48000,
            "track:0",
            0,
        )
        .unwrap();
        let inner = BoundRecorder {
            owner: PreparedParameterOwner::new().unwrap(),
            enabled,
            values: [0.; 2],
            trace: trace.clone(),
        };
        (
            AutomatedEffect::new(
                Box::new(inner),
                compiled.lanes,
                48000,
                touch,
                compiled.clock,
            )
            .unwrap(),
            trace,
        )
    }

    #[test]
    fn prepared_parameters_preserve_every_write_and_detector_partition() {
        for detector in 0..3 {
            let (mut old, before) = make(false, None);
            let (mut new, after) = make(true, None);
            let mut left = [0.125; 256];
            let mut right = left;
            let key = [0.25; 256];
            let clock = PluginTransport::new(0, 48000, 120.);
            let input = match detector {
                1 => Detector::Key(&key),
                2 => Detector::Control(&key[..128]),
                _ => Detector::None,
            };
            old.process_block(&mut left, input, Some(&clock));
            new.process_block(&mut right, input, Some(&clock));
            assert_eq!(left, right);
            assert_eq!(
                *before.writes.lock().unwrap(),
                *after.writes.lock().unwrap()
            );
            assert_eq!(
                *before.segments.lock().unwrap(),
                *after.segments.lock().unwrap()
            );
            assert_eq!(after.resolved.load(Ordering::Relaxed), 2);
            assert_eq!(after.named.load(Ordering::Relaxed), 0);
            assert!(
                after.writes.lock().unwrap().len() > 200,
                "f64 changes sharing f32 payloads are not coalesced"
            );
        }
    }

    #[test]
    fn prepared_parameters_fail_without_name_retry_or_partial_output() {
        for after_callback in [false, true] {
            let (mut effect, trace) = make(true, None);
            if after_callback {
                trace.fail_after_write.store(true, Ordering::Relaxed);
            } else {
                trace.invalidate.store(true, Ordering::Relaxed);
            }
            let mut pcm = [0.125; 256];
            effect.process(&mut pcm, 48000);
            assert!(effect.processing_failed());
            assert!(pcm.iter().all(|v| *v == 0.));
            assert_eq!(trace.named.load(Ordering::Relaxed), 0);
            assert_eq!(
                trace.writes.lock().unwrap().len(),
                usize::from(after_callback)
            );
        }
    }

    #[test]
    fn prepared_parameter_and_touch_scratch_are_allocation_free_from_first_block() {
        for touched in [false, true] {
            let touch = touched.then(|| Arc::new(TouchRegistry::new()));
            let key = touch::effect_key("track:0", 0, "a");
            if let Some(registry) = &touch {
                registry.begin(&key, 0.6);
            }
            let (mut effect, trace) = make(true, touch.clone());
            let mut pcm = [0.125; 512];
            let count = crate::test_alloc::operations(|| {
                effect.process_with_transport(
                    &mut pcm,
                    48000,
                    Some(PluginTransport::new(0, 48000, 120.)),
                );
                effect.reset();
                assert_eq!(effect.get_param("a"), Some(0.0));
                assert_eq!(effect.get_param("b"), Some(0.0));
                effect.process_with_transport(
                    &mut pcm,
                    48000,
                    Some(PluginTransport::new(64, 48000, 120.)),
                );
            });
            assert_eq!(
                count, 0,
                "first block, reset and seek retain prepared scratch"
            );
            if let Some(registry) = &touch {
                registry.release(&key);
                assert_eq!(
                    crate::test_alloc::operations(|| effect.process(&mut pcm, 48000)),
                    0,
                    "touch release reuses plans"
                );
            }
            assert_eq!(trace.resolved.load(Ordering::Relaxed), 2);
            assert_eq!(trace.named.load(Ordering::Relaxed), 0);
        }
    }
}
