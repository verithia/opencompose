use super::*;

const RATE: u32 = 48_000;

/// Two hosted instruments that sound only while a note is held: one plays
/// from the first bar, the other from bar 3 (4 s at 120 bpm), outside the
/// window a load builds before playback.
fn later_lane_project(late_bar: u32) -> Project {
    oc_dsl::parse(&format!(
        "@project\n  tempo: 120\n@preset gated\n  source: vst3\n  plugin: \"test.fake.note_gated\"\n\
         @track lead | preset: gated, instrument: keys/kontakt\n  @phrase a [1:1 - 2:1]\n    C4 w\n\
         @track pad | preset: gated, instrument: synth/pad\n  @phrase b [{late_bar}:1 - {}:1]\n    E4 w\n",
        late_bar + 1
    ))
    .unwrap()
}

fn loaded(project: &Project, defer: bool) -> RenderState {
    let mut state = RenderState::new(RATE);
    state.defer_lanes = defer;
    state
        .load_project_from(project, PathBuf::new(), None, 0)
        .unwrap();
    state
}

/// Take the lane the state asks the loader for and build it like the loader.
fn load_next(state: &mut RenderState) -> usize {
    let (tx, rx) = std::sync::mpsc::channel();
    state.request_deferred_lane(&tx, None);
    let Ok(SourceWork::Lane(request)) = rx.try_recv() else {
        panic!("the state asked for no lane");
    };
    let built = request.build().unwrap();
    state.admit_lane(request.lane, built).unwrap();
    request.lane
}

fn render_until(state: &mut RenderState, frame: u64, out: &mut Vec<f32>) {
    while state.render_cursor < frame {
        let frames = CHUNK_FRAMES.min((frame - state.render_cursor) as usize);
        let chunk = state.render_chunk(frames);
        assert!(
            !chunk.is_empty(),
            "no block may wait at {}",
            state.render_cursor
        );
        out.extend(chunk);
    }
}

fn instantiations(state: &RenderState) -> u64 {
    state.instrument_instantiations.load(Ordering::Acquire)
}

#[test]
fn a_lane_whose_first_note_is_later_loads_after_the_first_block_and_sounds_the_same() {
    let project = later_lane_project(3);
    let mut state = loaded(&project, true);
    assert_eq!(instantiations(&state), 1, "only the opening lane is built");
    let reserved = state.track_latency_plan[1];
    assert!(
        reserved > 0,
        "the mixer reserves latency for the loading lane"
    );

    let mut joined = Vec::new();
    render_until(&mut state, 3 * u64::from(RATE), &mut joined);
    assert_eq!(load_next(&mut state), 1);
    assert_eq!(instantiations(&state), 2);
    assert!(state.deferred.is_none(), "nothing is left to load");
    render_until(&mut state, 7 * u64::from(RATE), &mut joined);

    // A full load reserves nothing, so the joined mix is the same audio
    // behind the reserved delay.
    let mut full = loaded(&project, false);
    assert_eq!(full.track_latency_plan[1], 0);
    let mut whole = Vec::new();
    render_until(&mut full, 7 * u64::from(RATE), &mut whole);
    let shift = reserved as usize * 2;
    assert!(joined[..shift].iter().all(|sample| *sample == 0.0));
    assert_eq!(joined[shift..], whole[..whole.len() - shift]);
    let pad = 4 * RATE as usize * 2 + shift;
    assert!(
        joined[pad..pad + 256]
            .iter()
            .any(|sample| sample.abs() > 0.1)
    );
}

#[test]
fn a_block_that_needs_a_loading_lane_waits_for_it() {
    let project = later_lane_project(3);
    let mut state = loaded(&project, true);
    let mut out = Vec::new();
    render_until(&mut state, 4 * u64::from(RATE), &mut out);
    // The pad's note starts here and its instrument has not loaded.
    assert!(state.render_chunk(CHUNK_FRAMES).is_empty());
    assert!(state.waiting_for_sources);
    assert_eq!(state.render_cursor, 4 * u64::from(RATE));

    load_next(&mut state);
    let mut late = Vec::new();
    render_until(&mut state, 5 * u64::from(RATE), &mut late);
    assert!(!state.waiting_for_sources);
    assert!(
        late.iter().any(|sample| sample.abs() > 0.1),
        "the pad sounds"
    );
}

#[test]
fn a_seek_loads_the_lane_needed_there_first_and_it_joins_inside_its_note() {
    // Lanes 1 and 2 load later; the seek lands inside lane 2's note.
    let project = oc_dsl::parse(
        "@project\n  tempo: 120\n@preset gated\n  source: vst3\n  plugin: \"test.fake.note_gated\"\n\
         @track lead | preset: gated, instrument: keys/kontakt\n  @phrase a [1:1 - 2:1]\n    C4 w\n\
         @track near | preset: gated, instrument: synth/pad\n  @phrase b [3:1 - 4:1]\n    E4 w\n\
         @track far | preset: gated, instrument: synth/lead\n  @phrase c [6:1 - 8:1]\n    G4 w\n    G4 w\n",
    )
    .unwrap();
    let mut state = loaded(&project, true);
    assert_eq!(instantiations(&state), 1);
    let target = 11 * u64::from(RATE);
    state.seek_to(target).unwrap();
    while state.seek_target.is_some() {
        state.preroll_chunk();
    }
    assert!(
        state.render_chunk(CHUNK_FRAMES).is_empty(),
        "the far lane's held note waits for its instrument"
    );
    assert_eq!(
        load_next(&mut state),
        2,
        "the lane sounding at the target first"
    );
    let mut out = Vec::new();
    render_until(&mut state, target + 2 * CHUNK_FRAMES as u64, &mut out);
    // The joining lane starts its held note where it joins, behind the delay
    // the mixer reserved for it.
    let reserved = state.track_latency_plan[2] as usize;
    assert!(out[reserved * 2..].iter().all(|sample| sample.abs() > 0.1));
    assert_eq!(load_next(&mut state), 1);
}

#[test]
fn a_note_edit_keeps_loading_the_lanes_it_reuses() {
    let project = later_lane_project(3);
    let mut state = loaded(&project, true);
    let generation = state.deferred.as_ref().unwrap().generation;
    // Move the pad's note later: no instrument changes, so nothing rebuilds.
    let edited = later_lane_project(4);
    state
        .load_project_from(&edited, PathBuf::new(), None, 0)
        .unwrap();
    assert_eq!(instantiations(&state), 1);
    let deferred = state.deferred.as_ref().expect("the pad still loads");
    assert_eq!(
        deferred.generation, generation,
        "a build in flight stays valid"
    );
    assert!(!deferred.blocks(4 * u64::from(RATE), 6 * u64::from(RATE)));
    assert!(deferred.blocks(6 * u64::from(RATE), 6 * u64::from(RATE) + 1));
    assert_eq!(load_next(&mut state), 1);
}

#[test]
fn only_the_render_worker_defers_lanes() {
    let project = later_lane_project(3);
    let state = loaded(&project, false);
    assert!(state.deferred.is_none());
    assert_eq!(instantiations(&state), 2);
}

#[test]
fn the_render_worker_publishes_the_project_before_its_later_lanes_load() {
    let handle = RenderThreadHandle::spawn(RATE);
    // The loader takes this long per lane, so a load that waited for every
    // instrument could not be published before it.
    handle.test_build_delay_ms.store(400, Ordering::Release);
    handle.send(RenderCommand::ProjectChanged {
        project: Box::new(later_lane_project(3)),
        soundfont_path: PathBuf::new(),
        root: None,
        warm: false,
    });
    handle
        .wait_project_loaded(std::time::Duration::from_secs(10))
        .unwrap();
    let instantiations = &handle.playback.instrument_instantiations;
    assert_eq!(instantiations.load(Ordering::Acquire), 1);
    let start = std::time::Instant::now();
    while instantiations.load(Ordering::Acquire) < 2 {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "the later lane never loaded"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
