use super::*;
use crate::dsp::AudioEffect;
use oc_audio::ProcessingFailureReport;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

fn workers(n: usize) -> RenderWorkerOptions {
    RenderWorkerOptions::new(std::num::NonZeroUsize::new(n).unwrap())
}
fn chains() -> Vec<EffectChain> {
    [
        "- allpass: coefficient=0.8",
        "- limiter: ceiling=-0.8dB",
        "- delay: time=1ms, feedback=20%, mix=30%",
        "- gain: factor=0.3",
    ]
    .into_iter()
    .map(|line| {
        let p = oc_dsl::parse(&format!("@project\n@master\n  effects:\n    {line}\n")).unwrap();
        EffectChain::try_from_effects(&p.master.unwrap().effects, 8000).unwrap()
    })
    .collect()
}
fn run(
    chains: &mut [EffectChain],
    buffers: &mut [Option<Vec<f32>>],
    latencies: &mut [u32],
    n: usize,
    cancel: Option<&CancelToken>,
) -> Result<(), String> {
    let extra: Vec<_> = chains
        .iter()
        .map(|c| u64::from(c.latency_samples()) + u64::from(c.tail_samples()).min(30 * 8000))
        .collect();
    let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&Default::default()).unwrap());
    let transport = PluginTransport::new(1234, 8000, 120.).with_clock(clock);
    process_source_inserts(
        SourceInsertLanes {
            chains,
            buffers,
            latencies,
            extra_frames: &extra,
        },
        SourceInsertOptions {
            sample_rate: 8000,
            transport: &transport,
            cancel,
            workers: workers(n),
        },
    )
}
#[test]
fn source_insert_workers_preserve_exact_pcm_latency_tail_and_source_order() {
    let mut a = chains();
    let mut b = chains();
    let original: Vec<_> = (0..4)
        .map(|i| {
            Some(
                (0..256)
                    .map(|frame| {
                        if frame >= 254 {
                            0.2 + i as f32 * 0.01
                        } else {
                            0.0
                        }
                    })
                    .collect(),
            )
        })
        .collect();
    let mut x = original.clone();
    let mut y = original.clone();
    let mut la = vec![1, 2, 3, 4];
    let mut lb = la.clone();
    let expected: Vec<_> = a
        .iter()
        .zip(&la)
        .map(|(c, l)| l + c.latency_samples())
        .collect();
    run(&mut a, &mut x, &mut la, 1, None).unwrap();
    run(&mut b, &mut y, &mut lb, 4, None).unwrap();
    assert_eq!(x, y);
    assert_eq!(la, lb);
    assert_eq!(la, expected);
    assert!(x[0].as_ref().unwrap()[256..].iter().any(|v| v.abs() > 1e-6));
    for (index, c) in a.iter().enumerate() {
        assert_eq!(
            x[index].as_ref().unwrap().len(),
            256 + (c.latency_samples() as usize + c.tail_samples().min(30 * 8000) as usize) * 2
        );
    }
    let mut absent = chains();
    let mut buffers = vec![None, None, None, None];
    let mut latency = vec![4; 4];
    run(&mut absent, &mut buffers, &mut latency, 4, None).unwrap();
    assert_eq!(buffers, vec![None; 4]);
    assert_eq!(latency, vec![4; 4]);
}

struct Probe {
    entered: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
}
impl AudioEffect for Probe {
    fn process(&mut self, _: &mut [f32], _: u32) {
        let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(count, Ordering::SeqCst);
        self.entered.fetch_add(1, Ordering::SeqCst);
        let start = Instant::now();
        while self.entered.load(Ordering::SeqCst) < 2 && start.elapsed() < Duration::from_secs(1) {
            std::thread::yield_now();
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
    fn end_processing(&mut self) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Test source rendezvous"
    }
}
#[test]
fn source_insert_workers_are_concurrent_bounded_and_release_every_processing_owner() {
    let entered = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicUsize::new(0));
    let mut chains: Vec<_> = (0..4)
        .map(|_| {
            let mut c = EffectChain::new();
            c.effects.push(Box::new(Probe {
                entered: entered.clone(),
                active: active.clone(),
                peak: peak.clone(),
                released: released.clone(),
            }));
            c
        })
        .collect();
    let mut buffers = vec![Some(vec![0.1; 64]); 4];
    let mut latencies = vec![0; 4];
    run(&mut chains, &mut buffers, &mut latencies, 2, None).unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(entered.load(Ordering::SeqCst), 4);
    assert_eq!(released.load(Ordering::SeqCst), 4);
    let cancel = CancelToken::new();
    cancel.cancel();
    let before = buffers.clone();
    let calls = entered.load(Ordering::SeqCst);
    assert_eq!(
        run(&mut chains, &mut buffers, &mut latencies, 4, Some(&cancel)),
        Err(crate::render_cancel::cancelled())
    );
    assert_eq!(buffers, before);
    assert_eq!(entered.load(Ordering::SeqCst), calls);
}
struct Fail {
    failed: bool,
    released: Arc<AtomicUsize>,
}
impl AudioEffect for Fail {
    fn process(&mut self, _: &mut [f32], _: u32) {
        self.failed = true;
    }
    fn processing_failed(&self) -> bool {
        self.failed
    }
    fn take_processing_failure(&self) -> ProcessingFailureReport {
        ProcessingFailureReport::Ready("controlled source insert failure".into())
    }
    fn end_processing(&mut self) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Test failed source"
    }
}
#[test]
fn source_insert_failure_is_not_replaced_by_a_peer_stop_and_native_owner_is_released() {
    let released = Arc::new(AtomicUsize::new(0));
    let mut c = EffectChain::new();
    c.effects.push(Box::new(Fail {
        failed: false,
        released: released.clone(),
    }));
    let mut chains = vec![EffectChain::new(), c];
    let mut buffers = vec![Some(vec![0.2; 64]); 2];
    let mut latency = vec![0; 2];
    assert_eq!(
        run(&mut chains, &mut buffers, &mut latency, 4, None),
        Err("controlled source insert failure".into())
    );
    assert_eq!(released.load(Ordering::SeqCst), 1);
}
#[test]
fn source_insert_error_keeps_existing_cache_unpublished() {
    let base = "@project\n@preset p\n  source: builtin\n  plugin: \"builtin.test_tone\"\n@track t | preset: p, instrument: synth/lead\n  @phrase p [1:1 - 2:1]\n    C4 q\n";
    let mut cache = crate::render_cache::RenderCache::new();
    let project = oc_dsl::parse(base).unwrap();
    cache
        .render_full(
            &project,
            std::path::Path::new("unused.sf2"),
            8000,
            &mut std::collections::HashMap::new(),
        )
        .unwrap();
    assert!(cache.master_buffer().is_some());
    let mut bad = project.clone();
    bad.presets[0].effects =
        oc_dsl::parse("@project\n@master\n  effects:\n    - transfer_shaper: curve=\"{}\"\n")
            .unwrap()
            .master
            .unwrap()
            .effects;
    cache.mark_all_dirty();
    assert!(
        cache
            .render_full(
                &bad,
                std::path::Path::new("unused.sf2"),
                8000,
                &mut std::collections::HashMap::new()
            )
            .is_err()
    );
    assert!(cache.master_buffer().is_none());
    assert!(cache.is_dirty());
}
