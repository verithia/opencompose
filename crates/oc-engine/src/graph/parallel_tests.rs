//! Streaming blocks on block workers against the serial prepared block.

use super::*;
use std::fmt::Write as _;

const RATE: u32 = 48_000;
const MAX_FRAMES: usize = 4096;

/// Deterministic generator so a failing layout can be rebuilt from its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// A delay that reports its latency, so routes need compensation lines.
struct Latent {
    line: StereoDelayCompensator,
    frames: u32,
}

impl AudioEffect for Latent {
    fn process(&mut self, buffer: &mut [f32], _: u32) {
        self.line.process(buffer);
    }
    fn latency_samples(&self) -> u32 {
        self.frames
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {
        self.line = StereoDelayCompensator::new(self.frames);
    }
    fn name(&self) -> &str {
        "Test latency"
    }
}

/// A random routing graph: tracks into buses, buses into later buses and the
/// master, some buses keyed by earlier buses, some tracks keyed by tracks.
fn layout(seed: u64) -> (Project, Vec<(String, u32)>) {
    let mut rng = Lcg(seed);
    let buses = 6 + rng.below(14) as usize;
    let tracks = 3 + rng.below(5) as usize;
    let mut text =
        String::from("@project\n  tempo: 128\n  tempo_map:\n    2:1 + 0.1875q = 97 bpm\n");
    for track in 0..tracks {
        let target = rng.below(buses as u64);
        writeln!(text, "@track t{track} | instrument: audio/clip").unwrap();
        text.push_str("  effects:\n");
        writeln!(
            text,
            "    - gain: factor={}",
            0.4 + rng.below(50) as f64 / 100.0
        )
        .unwrap();
        if track > 0 && rng.below(3) == 0 {
            writeln!(
                text,
                "    - compression: threshold=-30dB, ratio=4:1, attack=1ms, release=30ms, sidechain=t{}",
                rng.below(track as u64)
            )
            .unwrap();
        }
        if rng.below(2) == 0 {
            let send = rng.below(buses as u64);
            writeln!(text, "  mix: output=b{target}, send(b{send}=35%)").unwrap();
        } else {
            writeln!(text, "  mix: output=b{target}").unwrap();
        }
    }
    let mut latent = Vec::new();
    for bus in 0..buses {
        let mut routes = Vec::new();
        for later in bus + 1..buses {
            if rng.below(4) == 0 {
                routes.push(format!("b{later}"));
            }
        }
        if routes.is_empty() || rng.below(3) == 0 {
            routes.push("master".into());
        }
        writeln!(text, "@bus b{bus}\n  routes: [{}]", routes.join(", ")).unwrap();
        text.push_str("  effects:\n");
        writeln!(
            text,
            "    - gain: factor={}",
            0.5 + rng.below(40) as f64 / 100.0
        )
        .unwrap();
        if bus > 0 && rng.below(3) == 0 {
            writeln!(
                text,
                "    - compression: threshold=-26dB, ratio=3:1, attack=2ms, release=40ms, sidechain=b{}",
                rng.below(bus as u64)
            )
            .unwrap();
        }
        if rng.below(3) == 0 {
            latent.push((format!("b{bus}"), 1 + rng.below(90) as u32));
        }
    }
    (
        oc_dsl::parse(&text).expect("generated layout parses"),
        latent,
    )
}

fn prepared_graph(project: &Project, latent: &[(String, u32)]) -> AudioGraph {
    let mut graph = AudioGraph::from_project(project, RATE);
    for (bus, frames) in latent {
        graph
            .bus_chains
            .get_mut(bus)
            .expect("generated bus")
            .chain
            .effects
            .push(Box::new(Latent {
                line: StereoDelayCompensator::new(*frames),
                frames: *frames,
            }));
    }
    graph.invalidate_latency_plan();
    graph.prepare(MAX_FRAMES).expect("prepared");
    graph
}

fn tracks(count: usize, start: u64, frames: usize, seed: u64) -> Vec<Vec<f32>> {
    (0..count)
        .map(|track| {
            (0..frames * 2)
                .map(|sample| {
                    let time = (start * 2 + sample as u64) as f32;
                    let rate = 0.011 + (seed % 7) as f32 * 0.003 + track as f32 * 0.0017;
                    (time * rate).sin() * 0.3
                })
                .collect()
        })
        .collect()
}

#[test]
fn worker_blocks_match_the_serial_block_bit_for_bit_on_random_layouts() {
    let mut workers = BlockWorkers::new(4, "ocp-test-block", || {}).expect("workers");
    for seed in 1..=24u64 {
        let (project, latent) = layout(seed);
        let mut serial = prepared_graph(&project, &latent);
        let mut parallel = prepared_graph(&project, &latent);
        let shape = parallel.parallel_shape();
        assert_eq!(shape.scheduled_buses, project.buses.len(), "seed {seed}");
        let clock = Arc::new(oc_dsl::timing::ProjectClock::new(&project.properties).unwrap());
        let transport = PluginTransport::new(0, RATE, 128.0).with_clock(clock);
        let mut cursor = 0u64;
        for (block, frames) in [1024, 4096, 37, 4096, 4096, 1, 4096]
            .into_iter()
            .enumerate()
        {
            // Silence a bus mid-stream so its return drops out and comes back.
            let toggled = format!("b{}", seed as usize % project.buses.len());
            let audible = block != 3;
            serial.bus_chains.get_mut(&toggled).unwrap().audible = audible;
            parallel.bus_chains.get_mut(&toggled).unwrap().audible = audible;
            let input = tracks(project.tracks.len(), cursor, frames, seed);
            let at = transport.at_sample(cursor);
            let aux = HashMap::new();
            let mut expected = vec![0.0; frames * 2];
            let mut serial_input = input.clone();
            serial
                .process_prepared(PreparedGraphBlock {
                    tracks: &mut serial_input,
                    transport: Some(&at),
                    aux: &aux,
                    output: &mut expected,
                })
                .unwrap();
            let mut actual = vec![f32::NAN; frames * 2];
            let mut parallel_input = input;
            let mut result = Ok(());
            // The render thread takes jobs too, so this also covers the
            // buses it runs, besides ranking, tables and the master gather.
            let operations = crate::test_alloc::operations(|| {
                result = parallel.process_prepared_with_workers(
                    PreparedGraphBlock {
                        tracks: &mut parallel_input,
                        transport: Some(&at),
                        aux: &aux,
                        output: &mut actual,
                    },
                    &mut workers,
                );
            });
            result.unwrap();
            assert_eq!(operations, 0, "seed {seed} block {block}");
            assert!(expected.iter().any(|sample| *sample != 0.0), "seed {seed}");
            let same = expected
                .iter()
                .zip(&actual)
                .all(|(a, b)| a.to_bits() == b.to_bits());
            assert!(same, "seed {seed} block {block}");
            assert_eq!(serial_input, parallel_input, "seed {seed} block {block}");
            cursor += frames as u64;
        }
    }
}

#[test]
fn buses_are_ranked_by_the_costliest_path_to_the_master() {
    let project = oc_dsl::parse(
        "@project\n@track t | instrument: audio/clip\n  mix: output=head\n\
         @bus head\n  routes: [tail]\n@bus wide\n  routes: [master]\n\
         @bus tail\n  routes: [master]\n",
    )
    .unwrap();
    let mut graph = AudioGraph::from_project(&project, RATE);
    graph.prepare(MAX_FRAMES).unwrap();
    let prepared = graph.prepared.as_mut().unwrap();
    let names: Vec<String> = prepared.buses.iter().map(|bus| bus.name.clone()).collect();
    let plan = prepared
        .parallel
        .as_mut()
        .and_then(|plan| plan.buses.as_mut())
        .expect("scheduled buses");
    let index = |name: &str| names.iter().position(|bus| bus == name).unwrap();
    // `wide` alone outweighs `head`, but `head` starts the heavier chain.
    for (name, cost) in [("head", 10), ("wide", 500), ("tail", 900)] {
        plan.queue.cost[index(name)].store(cost, Ordering::Relaxed);
    }
    plan.rank_nodes();
    let order: Vec<&str> = plan.order.iter().map(|&bus| names[bus].as_str()).collect();
    assert_eq!(order, ["head", "tail", "wide"]);
    assert_eq!(plan.rank[index("head")], 910);
}

/// A bus device that fails on the audio thread.
struct Panicking;

impl AudioEffect for Panicking {
    fn process(&mut self, _: &mut [f32], _: u32) {
        panic!("device failure");
    }
    fn set_param(&mut self, _: &str, _: f64) {}
    fn get_param(&self, _: &str) -> Option<f64> {
        None
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Test panic"
    }
}

#[test]
fn a_panicking_bus_unwinds_the_block_without_stranding_workers() {
    let mut workers = BlockWorkers::new(4, "ocp-test-block", || {}).expect("workers");
    let (project, latent) = layout(7);
    let mut failing = AudioGraph::from_project(&project, RATE);
    failing
        .bus_chains
        .get_mut("b0")
        .unwrap()
        .chain
        .effects
        .push(Box::new(Panicking));
    failing.prepare(MAX_FRAMES).unwrap();
    let frames = 512;
    let mut input = tracks(project.tracks.len(), 0, frames, 7);
    let mut output = vec![0.0; frames * 2];
    let aux = HashMap::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        failing.process_prepared_with_workers(
            PreparedGraphBlock {
                tracks: &mut input,
                transport: None,
                aux: &aux,
                output: &mut output,
            },
            &mut workers,
        )
    }));
    assert!(outcome.is_err(), "the failure reaches the render thread");

    // The same workers still render a healthy graph exactly.
    let mut serial = prepared_graph(&project, &latent);
    let mut parallel = prepared_graph(&project, &latent);
    let input = tracks(project.tracks.len(), 0, frames, 7);
    let mut expected = vec![0.0; frames * 2];
    serial
        .process_prepared(PreparedGraphBlock {
            tracks: &mut input.clone(),
            transport: None,
            aux: &aux,
            output: &mut expected,
        })
        .unwrap();
    let mut actual = vec![0.0; frames * 2];
    parallel
        .process_prepared_with_workers(
            PreparedGraphBlock {
                tracks: &mut input.clone(),
                transport: None,
                aux: &aux,
                output: &mut actual,
            },
            &mut workers,
        )
        .unwrap();
    assert_eq!(expected, actual);
}

/// A layout whose tracks read no keys, so its sources join the schedule:
/// pre- and post-fader sends, direct tracks, polarity, buses keyed by tracks
/// and by earlier buses, and auxiliary sends.
fn flow_layout(seed: u64) -> (Project, Vec<(String, u32)>) {
    let mut rng = Lcg(seed);
    let buses = 2 + rng.below(12) as usize;
    let tracks = 3 + rng.below(8) as usize;
    let mut text = String::from("@project\n  tempo: 120\n");
    for track in 0..tracks {
        writeln!(text, "@track t{track} | instrument: audio/clip").unwrap();
        text.push_str("  effects:\n");
        writeln!(
            text,
            "    - gain: factor={}",
            0.4 + rng.below(50) as f64 / 100.0
        )
        .unwrap();
        let mut mix = Vec::new();
        if rng.below(4) != 0 {
            mix.push(format!("output=b{}", rng.below(buses as u64)));
        }
        for _ in 0..rng.below(3) {
            let tap = if rng.below(2) == 0 { ", pre" } else { "" };
            mix.push(format!(
                "send(b{}={}%{tap})",
                rng.below(buses as u64),
                10 + rng.below(80)
            ));
        }
        if rng.below(4) == 0 {
            mix.push("polarity=invert".into());
        }
        mix.push(format!(
            "vol=-{}dB, pan={}",
            rng.below(12),
            rng.below(21) as f64 / 10.0 - 1.0
        ));
        writeln!(text, "  mix: {}", mix.join(", ")).unwrap();
    }
    let mut latent = Vec::new();
    for bus in 0..buses {
        let mut routes = Vec::new();
        for later in bus + 1..buses {
            if rng.below(4) == 0 {
                routes.push(format!("b{later}"));
            }
        }
        if routes.is_empty() || rng.below(3) == 0 {
            routes.push("master".into());
        }
        writeln!(text, "@bus b{bus}\n  routes: [{}]", routes.join(", ")).unwrap();
        text.push_str("  effects:\n");
        writeln!(
            text,
            "    - gain: factor={}",
            0.5 + rng.below(40) as f64 / 100.0
        )
        .unwrap();
        if rng.below(3) == 0 {
            let key = if bus > 0 && rng.below(2) == 0 {
                format!("b{}", rng.below(bus as u64))
            } else {
                format!("t{}", rng.below(tracks as u64))
            };
            writeln!(
                text,
                "    - compression: threshold=-26dB, ratio=3:1, attack=2ms, release=40ms, sidechain={key}"
            )
            .unwrap();
        }
        if rng.below(3) == 0 {
            latent.push((format!("b{bus}"), 1 + rng.below(90) as u32));
        }
    }
    (
        oc_dsl::parse(&text).expect("generated layout parses"),
        latent,
    )
}

/// Test sources: each renders a tone into its own buffer; a track's
/// auxiliary output is half the sum of its sources.
struct ToneSources {
    tracks: Vec<usize>,
    rendered: Vec<std::cell::UnsafeCell<Vec<f32>>>,
    aux: Vec<std::cell::UnsafeCell<Vec<f32>>>,
    start: u64,
    frames: usize,
}

// SAFETY: every cell is touched by the one job the `BlockSources` contract
// gives it, or only read after that job.
unsafe impl Sync for ToneSources {}

impl ToneSources {
    fn new(tracks: Vec<usize>, track_count: usize) -> Self {
        let buffer = || std::cell::UnsafeCell::new(Vec::with_capacity(MAX_FRAMES * 2));
        Self {
            rendered: tracks.iter().map(|_| buffer()).collect(),
            aux: (0..track_count).map(|_| buffer()).collect(),
            tracks,
            start: 0,
            frames: 0,
        }
    }

    fn tone(&self, source: usize, out: &mut Vec<f32>) {
        out.clear();
        out.extend((0..self.frames * 2).map(|sample| {
            let time = (self.start * 2 + sample as u64) as f32;
            let side = if sample % 2 == 0 { 1.0 } else { 0.8 };
            (time * (0.007 + source as f32 * 0.0013)).sin() * 0.25 * side
        }));
    }

    /// The serial reference: every source summed into its track in source
    /// order, and the auxiliary map.
    fn mix_serially(&self, tracks: &mut [Vec<f32>]) -> HashMap<(usize, usize), Vec<f32>> {
        let mut aux: HashMap<(usize, usize), Vec<f32>> = HashMap::new();
        let mut tone = Vec::new();
        for track in 0..tracks.len() {
            aux.insert((track, 0), vec![0.0; self.frames * 2]);
        }
        for (source, &track) in self.tracks.iter().enumerate() {
            self.tone(source, &mut tone);
            for (out, sample) in tracks[track].iter_mut().zip(&tone) {
                *out += sample;
            }
            for (out, sample) in aux.get_mut(&(track, 0)).unwrap().iter_mut().zip(&tone) {
                *out += sample * 0.5;
            }
        }
        aux
    }
}

impl BlockSources for ToneSources {
    fn source_tracks(&self) -> &[usize] {
        &self.tracks
    }

    unsafe fn render(&self, source: usize) {
        // SAFETY: one job per source.
        self.tone(source, unsafe { &mut *self.rendered[source].get() });
    }

    unsafe fn mix_track(&self, track: usize, buffer: &mut [f32]) {
        // SAFETY: one job per track, after its sources.
        let aux = unsafe { &mut *self.aux[track].get() };
        aux.clear();
        aux.resize(self.frames * 2, 0.0);
        for (source, _) in self.tracks.iter().enumerate().filter(|(_, t)| **t == track) {
            // SAFETY: the source has rendered and nothing writes it now.
            let tone = unsafe { &*self.rendered[source].get() };
            for ((out, aux), sample) in buffer.iter_mut().zip(aux.iter_mut()).zip(tone) {
                *out += sample;
                *aux += sample * 0.5;
            }
        }
    }

    unsafe fn aux(&self, track: usize, pair: usize) -> Option<&[f32]> {
        // SAFETY: the track has mixed; readers only read.
        (pair == 0).then(|| unsafe { (*self.aux[track].get()).as_slice() })
    }
}

#[test]
fn sources_scheduled_with_tracks_and_buses_match_the_serial_block_bit_for_bit() {
    let mut workers = BlockWorkers::new(4, "ocp-test-block", || {}).expect("workers");
    let mut scheduled_layouts = 0;
    for seed in 1..=32u64 {
        let (project, latent) = flow_layout(seed);
        let track_count = project.tracks.len();
        let mut rng = Lcg(seed ^ 0x5eed);
        // Some tracks hear several sources, some none.
        let source_tracks: Vec<usize> = (0..track_count + rng.below(6) as usize)
            .map(|_| rng.below(track_count as u64) as usize)
            .collect();
        let build = || {
            let mut graph = AudioGraph::from_project(&project, RATE);
            for (track, route) in graph.routing.iter_mut().enumerate() {
                if (track + seed as usize).is_multiple_of(3) {
                    route.aux_sends.push(AuxSendRoute {
                        pair: 0,
                        bus: format!("b{}", (track + 1) % project.buses.len()),
                        amount: 0.6,
                        pan: -0.3,
                    });
                }
            }
            // Latent tracks give the other tracks' direct and send routes
            // compensation delays.
            for (track, chain) in graph.track_chains.iter_mut().enumerate() {
                if (track + seed as usize) % 4 == 1 {
                    let frames = 7 + track as u32 * 5;
                    chain.effects.push(Box::new(Latent {
                        line: StereoDelayCompensator::new(frames),
                        frames,
                    }));
                }
            }
            for (bus, frames) in &latent {
                graph
                    .bus_chains
                    .get_mut(bus)
                    .unwrap()
                    .chain
                    .effects
                    .push(Box::new(Latent {
                        line: StereoDelayCompensator::new(*frames),
                        frames: *frames,
                    }));
            }
            graph.invalidate_latency_plan();
            graph.prepare(MAX_FRAMES).expect("prepared");
            graph
        };
        let mut serial = build();
        let mut flow = build();
        flow.set_block_sources(Some(&source_tracks));
        let shape = flow.parallel_shape();
        assert_eq!(shape.scheduled_sources, source_tracks.len(), "seed {seed}");
        scheduled_layouts += 1;
        let mut sources = ToneSources::new(source_tracks.clone(), track_count);
        let mut cursor = 0u64;
        for (block, frames) in [1024, 4096, 37, 4096, 4096, 1, 4096]
            .into_iter()
            .enumerate()
        {
            // Mute a track mid-stream so its gate ramps out, sits silent
            // while its delay lines advance, and ramps back in.
            let muted = seed as usize % track_count;
            let mute = (2..=4).contains(&block);
            serial.routing[muted].mute = mute;
            flow.routing[muted].mute = mute;
            // Move every send level once, so the send ramps glide.
            if block == 3 {
                for graph in [&mut serial, &mut flow] {
                    for route in &mut graph.routing {
                        for send in &mut route.sends {
                            send.amount *= 0.5;
                        }
                    }
                }
            }
            sources.start = cursor;
            sources.frames = frames;
            let at = PluginTransport::new(cursor, RATE, 120.0);
            let mut serial_tracks = vec![vec![0.0; frames * 2]; track_count];
            let aux = sources.mix_serially(&mut serial_tracks);
            let mut expected = vec![0.0; frames * 2];
            serial
                .process_prepared(PreparedGraphBlock {
                    tracks: &mut serial_tracks,
                    transport: Some(&at),
                    aux: &aux,
                    output: &mut expected,
                })
                .unwrap();
            let mut flow_tracks = vec![vec![0.0; frames * 2]; track_count];
            let mut actual = vec![f32::NAN; frames * 2];
            let mut result = Ok(false);
            let operations = crate::test_alloc::operations(|| {
                result = flow.process_prepared_with_sources(
                    &mut flow_tracks,
                    Some(&at),
                    &mut actual,
                    &mut workers,
                    &sources,
                );
            });
            assert!(result.unwrap(), "seed {seed} block {block} is scheduled");
            assert_eq!(operations, 0, "seed {seed} block {block}");
            assert!(expected.iter().any(|sample| *sample != 0.0), "seed {seed}");
            let same = expected
                .iter()
                .zip(&actual)
                .all(|(a, b)| a.to_bits() == b.to_bits());
            assert!(same, "seed {seed} block {block}");
            assert_eq!(serial_tracks, flow_tracks, "seed {seed} block {block}");
            cursor += frames as u64;
        }
    }
    assert!(scheduled_layouts > 0);
}

#[test]
fn graphs_the_schedule_cannot_order_leave_their_sources_untouched() {
    let mut workers = BlockWorkers::new(4, "ocp-test-block", || {}).expect("workers");
    // A track keyed by another track runs in key order, outside the schedule.
    let project = oc_dsl::parse(
        "@project\n@track kick | instrument: audio/clip\n\
         @track bass | instrument: audio/clip\n  effects:\n    \
         - compression: threshold=-30dB, ratio=4:1, attack=1ms, release=30ms, sidechain=kick\n\
         @bus b0\n  routes: [master]\n@bus b1\n  routes: [master]\n",
    )
    .unwrap();
    let mut graph = AudioGraph::from_project(&project, RATE);
    graph.prepare(MAX_FRAMES).unwrap();
    graph.set_block_sources(Some(&[0, 1]));
    assert_eq!(graph.parallel_shape().scheduled_sources, 0);
    let mut sources = ToneSources::new(vec![0, 1], 2);
    sources.frames = 256;
    let mut tracks = vec![vec![0.0; 512]; 2];
    let mut output = vec![0.0; 512];
    let scheduled = graph
        .process_prepared_with_sources(&mut tracks, None, &mut output, &mut workers, &sources)
        .unwrap();
    assert!(!scheduled);
    assert!(sources.rendered.iter().all(|cell| {
        // SAFETY: no job is running.
        unsafe { (*cell.get()).is_empty() }
    }));
    assert!(tracks.iter().flatten().all(|sample| *sample == 0.0));
}

/// A project without tracks still runs its block: the master chain and
/// fader process the block's silence.
#[test]
fn a_project_without_tracks_schedules_an_empty_block() {
    let mut workers = BlockWorkers::new(4, "ocp-test-block", || {}).expect("workers");
    let project = oc_dsl::parse(
        "@project\n@bus b0\n  routes: [master]\n@master\n  effects:\n    - gain: factor=0.5\n",
    )
    .unwrap();
    let mut graph = AudioGraph::from_project(&project, RATE);
    graph.prepare(MAX_FRAMES).unwrap();
    graph.set_block_sources(Some(&[]));
    let sources = ToneSources::new(Vec::new(), 0);
    let mut output = vec![f32::NAN; 2048];
    let scheduled = graph
        .process_prepared_with_sources(&mut [], None, &mut output, &mut workers, &sources)
        .unwrap();
    assert!(scheduled);
    assert!(output.iter().all(|sample| *sample == 0.0));
}
