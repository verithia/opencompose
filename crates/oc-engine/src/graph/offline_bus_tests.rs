//! Deterministic offline scheduling uses the production graph and routing path.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn project() -> Project {
    oc_dsl::parse("@project\n@track left | instrument: audio/clip\n  mix: output=a\n@track right | instrument: audio/clip\n  mix: output=y\n@bus a\n  routes: [b, z, master]\n@bus b\n  routes: [z, master]\n  effects:\n    - gain: factor=0.00000001\n@bus y\n  routes: [z, master]\n@bus z\n  routes: [master]\n").unwrap()
}
fn graph() -> AudioGraph {
    AudioGraph::from_project(&project(), 48_000)
}
fn same_stems(a: &StemCapture, b: &StemCapture) {
    assert_eq!(a.tracks, b.tracks);
    assert_eq!(a.buses, b.buses);
    assert_eq!(a.master_inputs, b.master_inputs);
    assert_eq!(a.pre_master, b.pre_master);
}

#[test]
fn offline_bus_dag_preserves_serial_rounding_and_stem_order() {
    let mut serial = graph();
    let mut parallel = graph();
    assert_eq!(serial.bus_order, ["a", "b", "y", "z"]);
    assert_eq!(parallel.parallel_bus_barriers(4).unwrap(), [0, 1, 0, 3]);
    let tracks = vec![vec![100_000_000.0; 128], vec![-100_000_000.0; 128]];
    let mut a = StemCapture::default();
    let mut b = StemCapture::default();
    let x = serial.mix_processed_with_workers(
        &mut tracks.clone(),
        None,
        Some(&mut a),
        &HashMap::new(),
        1,
    );
    let y = parallel.mix_processed_with_workers(
        &mut tracks.clone(),
        None,
        Some(&mut b),
        &HashMap::new(),
        4,
    );
    assert_eq!(x, y);
    assert!(x.iter().all(|v| *v == 0.0));
    same_stems(&a, &b);
}

struct Delay {
    state: StereoDelayCompensator,
    frames: u32,
}
impl AudioEffect for Delay {
    fn process(&mut self, buffer: &mut [f32], _: u32) {
        self.state.process(buffer);
    }
    fn latency_samples(&self) -> u32 {
        self.frames
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {
        self.state = StereoDelayCompensator::new(self.frames);
    }
    fn name(&self) -> &str {
        "Test delay"
    }
}
fn dynamic_graph() -> AudioGraph {
    let mut p = project();
    p.buses[1].effects = oc_dsl::parse("@project\n@master\n  effects:\n    - compression: threshold=-24dB, ratio=4:1, attack=1ms, release=20ms, sidechain=a\n").unwrap().master.unwrap().effects;
    let mut graph = AudioGraph::from_project(&p, 48_000);
    for (name, frames) in [("a", 3), ("y", 7)] {
        graph
            .bus_chains
            .get_mut(name)
            .unwrap()
            .chain
            .effects
            .push(Box::new(Delay {
                state: StereoDelayCompensator::new(frames),
                frames,
            }));
    }
    graph
}
#[test]
fn offline_bus_dag_keeps_sidechain_pdc_ramps_and_multiblock_state_exact() {
    let mut a = dynamic_graph();
    let mut b = dynamic_graph();
    for (block, frames) in [37, 256, 1024, 17, 256, 1024].into_iter().enumerate() {
        if block == 2 {
            a.bus_chains.get_mut("y").unwrap().audible = false;
            b.bus_chains.get_mut("y").unwrap().audible = false;
        }
        if block == 4 {
            a.bus_chains.get_mut("y").unwrap().audible = true;
            b.bus_chains.get_mut("y").unwrap().audible = true;
        }
        let tracks: Vec<Vec<f32>> = (0..2)
            .map(|channel| {
                (0..frames * 2)
                    .map(|i| {
                        ((i + block * 1024) as f32 * 0.03).sin() * (0.2 + channel as f32 * 0.1)
                    })
                    .collect()
            })
            .collect();
        let mut sa = StemCapture::default();
        let mut sb = StemCapture::default();
        let x = a.mix_processed_with_workers(
            &mut tracks.clone(),
            None,
            Some(&mut sa),
            &HashMap::new(),
            1,
        );
        let y = b.mix_processed_with_workers(
            &mut tracks.clone(),
            None,
            Some(&mut sb),
            &HashMap::new(),
            4,
        );
        assert_eq!(x, y, "block {block}");
        same_stems(&sa, &sb);
        assert_eq!(
            a.pre_master_latency_samples(),
            b.pre_master_latency_samples()
        );
        assert!(a.pre_master_latency_samples() > 0);
    }
}

struct Dependency(&'static str);
impl AudioEffect for Dependency {
    fn process(&mut self, _: &mut [f32], _: u32) {}
    fn control_source(&self) -> Option<&str> {
        Some(self.0)
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Test control edge"
    }
}
#[test]
fn offline_bus_barriers_include_control_edges_and_refuse_backwards_plans() {
    let mut graph = graph();
    graph
        .bus_chains
        .get_mut("y")
        .unwrap()
        .chain
        .effects
        .push(Box::new(Dependency("b")));
    assert_eq!(graph.parallel_bus_barriers(4).unwrap()[2], 2);
    graph
        .bus_chains
        .get_mut("a")
        .unwrap()
        .chain
        .effects
        .push(Box::new(Dependency("z")));
    assert!(graph.parallel_bus_barriers(4).is_none());
    assert!(graph.parallel_bus_barriers(1).is_none());
}

struct Rendezvous {
    entered: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}
impl AudioEffect for Rendezvous {
    fn process(&mut self, _: &mut [f32], _: u32) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.entered.fetch_add(1, Ordering::SeqCst);
        let start = Instant::now();
        while self.entered.load(Ordering::SeqCst) < 2 && start.elapsed() < Duration::from_secs(1) {
            std::thread::yield_now();
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Test rendezvous"
    }
}
#[test]
fn offline_bus_dag_actually_overlaps_independent_ready_nodes() {
    let mut graph = graph();
    let entered = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    for name in ["a", "y"] {
        graph
            .bus_chains
            .get_mut(name)
            .unwrap()
            .chain
            .effects
            .push(Box::new(Rendezvous {
                entered: entered.clone(),
                active: active.clone(),
                peak: peak.clone(),
            }));
    }
    graph.mix_processed_with_workers(
        &mut vec![vec![0.1; 128], vec![0.2; 128]],
        None,
        None,
        &HashMap::new(),
        4,
    );
    assert!(
        peak.load(Ordering::SeqCst) >= 2,
        "ready nodes must execute concurrently"
    );
}
