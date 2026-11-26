use super::*;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn output(value: f32) -> Vst2GroupOutput {
    Vst2GroupOutput {
        buffers: vec![vec![value; 16]],
        aux: Default::default(),
        latency: 7,
    }
}

#[test]
fn vst2_batch_capacity_bounds_native_instances_by_workers() {
    assert_eq!(capacity(0), 1);
    assert_eq!(capacity(1), 1);
    assert_eq!(capacity(2), 2);
    assert_eq!(capacity(128), 4);
}

#[test]
fn vst2_batch_prepared_processors_run_concurrently_but_keep_source_order() {
    let entered = Arc::new(std::sync::Barrier::new(4));
    let prepared = (0..4)
        .map(|index| {
            let entered = entered.clone();
            PreparedGroup::new(
                WarmupDeadline::after_preparation(0.0).unwrap(),
                Box::new(move |_| {
                    entered.wait();
                    Ok(output(index as f32))
                }),
            )
        })
        .collect();
    let results = render(prepared, None).unwrap();
    assert_eq!(
        results.iter().map(|r| r.buffers[0][0]).collect::<Vec<_>>(),
        vec![0., 1., 2., 3.]
    );
    assert!(results.iter().all(|r| r.latency == 7));
}

#[test]
fn vst2_batch_never_sends_authored_work_before_each_instance_deadline() {
    let mut groups = Vec::new();
    for _ in 0..2 {
        let restored = Instant::now();
        groups.push(PreparedGroup::new(
            WarmupDeadline::after_preparation(0.015).unwrap(),
            Box::new(move |_| {
                assert!(restored.elapsed() >= Duration::from_millis(15));
                Ok(output(0.25))
            }),
        ));
    }
    assert_eq!(render(groups, None).unwrap().len(), 2);
}

struct Release(Arc<AtomicUsize>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn vst2_batch_cancelled_loading_drops_all_prepared_work_without_midi() {
    let cancel = CancelToken::new();
    let released = Arc::new(AtomicUsize::new(0));
    let processed = Arc::new(AtomicBool::new(false));
    let guard = Release(released.clone());
    let called = processed.clone();
    let group = PreparedGroup::new(
        WarmupDeadline::after_preparation(600.0).unwrap(),
        Box::new(move |_| {
            let _guard = guard;
            called.store(true, Ordering::SeqCst);
            Ok(output(0.))
        }),
    );
    cancel.cancel();
    assert!(render(vec![group], Some(&cancel)).is_err());
    assert!(!processed.load(Ordering::SeqCst));
    assert_eq!(released.load(Ordering::SeqCst), 1);
}

#[test]
fn vst2_batch_processing_failure_cancels_peers_and_releases_every_owner() {
    let released = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(std::sync::Barrier::new(2));
    let mut groups = Vec::new();
    for index in 0..2 {
        let guard = Release(released.clone());
        let entered = entered.clone();
        groups.push(PreparedGroup::new(
            WarmupDeadline::after_preparation(0.0).unwrap(),
            Box::new(move |cancel| {
                let _guard = guard;
                entered.wait();
                if index == 1 {
                    return Err("controlled preparation owner failed".into());
                }
                while !cancel.is_cancelled() {
                    std::thread::yield_now();
                }
                Err(crate::render_cancel::cancelled())
            }),
        ));
    }
    let error = render(groups, None).err().unwrap();
    assert_eq!(error, "controlled preparation owner failed");
    assert_eq!(released.load(Ordering::SeqCst), 2);
}

#[test]
fn vst2_batch_external_cancellation_stops_running_processors_and_joins_them() {
    let cancel = CancelToken::new();
    let signal = cancel.clone();
    let released = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut groups = Vec::new();
    for _ in 0..2 {
        let sender = sender.clone();
        let guard = Release(released.clone());
        groups.push(PreparedGroup::new(
            WarmupDeadline::after_preparation(0.0).unwrap(),
            Box::new(move |cancel| {
                let _guard = guard;
                sender.send(()).unwrap();
                while !cancel.is_cancelled() {
                    std::thread::yield_now();
                }
                Err(crate::render_cancel::cancelled())
            }),
        ));
    }
    let canceller = std::thread::spawn(move || {
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        signal.cancel();
    });
    assert_eq!(
        render(groups, Some(&cancel)).err().unwrap(),
        crate::render_cancel::cancelled()
    );
    canceller.join().unwrap();
    assert_eq!(released.load(Ordering::SeqCst), 2);
}

#[test]
fn vst2_batch_failed_retirement_cannot_admit_the_next_cohort() {
    let mut preparations = 0;
    let mut acknowledgements = 0;
    let result = (|| -> Result<(), String> {
        for _ in 0..2 {
            preparations += 1;
            let group = PreparedGroup::native(
                WarmupDeadline::after_preparation(0.).unwrap(),
                Box::new(|_| Ok(output(0.25))),
            );
            render_with_retirement(vec![group], None, || {
                acknowledgements += 1;
                Err(oc_host::vst2::Vst2OwnedError::RetirementIncomplete { resident: 1 }.to_string())
            })?;
        }
        Ok(())
    })();
    assert!(result.is_err());
    assert_eq!(preparations, 1);
    assert_eq!(acknowledgements, 1);
}

#[derive(Clone)]
struct Audit {
    prepared: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    processed: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
}

thread_local! {
    static AUDIT: std::cell::RefCell<Option<Audit>> = const { std::cell::RefCell::new(None) };
}

fn prepare_controlled(
    request: super::super::vst2_group::Vst2GroupRequest<'_>,
) -> Result<PreparedGroup, String> {
    let audit = AUDIT.with(|audit| audit.borrow().as_ref().unwrap().clone());
    let state = request.plugin.state.unwrap();
    audit.prepared.lock().unwrap().push(state.clone());
    if state == [255] {
        return Err("controlled state preparation refused".into());
    }
    assert!(request.events.iter().any(|events| !events.is_empty()));
    let frames = request.total_frames;
    let tracks = request.tracks.len();
    let guard = Release(audit.released.clone());
    Ok(PreparedGroup::new(
        WarmupDeadline::after_preparation(request.pre_roll_seconds)?,
        Box::new(move |_| {
            let _guard = guard;
            audit.processed.fetch_add(1, Ordering::SeqCst);
            Ok(Vst2GroupOutput {
                buffers: vec![vec![state[0] as f32 / 100.; frames * 2]; tracks],
                aux: Default::default(),
                latency: 0,
            })
        }),
    ))
}

fn controlled_project(second: &str) -> oc_dsl::ast::Project {
    oc_dsl::parse(&format!(
        "@project\n  tempo: 120\n@preset first\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n  params: {{state_hex: \"01\"}}\n@preset second\n  source: vst2\n  plugin: \"test.fixture.vst2.tone\"\n  params: {{state_hex: \"{second}\"}}\n@track first | preset: first, instrument: synth/lead\n  @phrase first [1:1 - 2:1]\n    C4 q\n@track second | preset: second, instrument: synth/lead\n  @phrase second [1:1 - 2:1]\n    E4 q\n"
    )).unwrap()
}

#[test]
fn vst2_batch_cache_keeps_state_identity_and_preparation_failure_atomic() {
    let audit = Audit {
        prepared: Arc::new(std::sync::Mutex::new(Vec::new())),
        processed: Arc::new(AtomicUsize::new(0)),
        released: Arc::new(AtomicUsize::new(0)),
    };
    AUDIT.with(|cell| *cell.borrow_mut() = Some(audit.clone()));
    let mut cache = super::super::RenderCache::new();
    cache.set_worker_options(crate::render_parallel::RenderWorkerOptions::new(
        std::num::NonZeroUsize::new(2).unwrap(),
    ));
    cache.set_vst2_tail_probe(|_| Ok(None));
    cache.set_vst2_group_preparer(prepare_controlled);
    let mut plugins = std::collections::HashMap::new();
    let healthy = controlled_project("02");
    let audio = cache
        .render_full(
            &healthy,
            std::path::Path::new("unused.sf2"),
            8_000,
            &mut plugins,
        )
        .unwrap();
    assert!(audio.iter().any(|sample| *sample != 0.));
    assert_eq!(*audit.prepared.lock().unwrap(), vec![vec![1], vec![2]]);
    assert_eq!(audit.processed.load(Ordering::SeqCst), 2);
    assert_eq!(audit.released.load(Ordering::SeqCst), 2);
    cache.mark_all_dirty();
    let error = cache
        .render_full(
            &controlled_project("ff"),
            std::path::Path::new("unused.sf2"),
            8_000,
            &mut plugins,
        )
        .unwrap_err();
    assert_eq!(error, "controlled state preparation refused");
    assert_eq!(
        audit.processed.load(Ordering::SeqCst),
        2,
        "failed cohort cannot process its earlier prepared member"
    );
    assert_eq!(audit.released.load(Ordering::SeqCst), 3);
    assert!(
        cache.get_track_buffer(0).is_none(),
        "failure cannot publish partial audio"
    );
    let repaired = controlled_project("03");
    cache
        .render_full(
            &repaired,
            std::path::Path::new("unused.sf2"),
            8_000,
            &mut plugins,
        )
        .unwrap();
    assert!(
        audit.prepared.lock().unwrap().contains(&vec![3]),
        "new authored state must be prepared rather than reuse the prior readiness token"
    );
    AUDIT.with(|cell| *cell.borrow_mut() = None);
}
