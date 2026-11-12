//! Edit preview: the proposal as a document, the base beside it.
//!
//! Every case drives the command bodies the panel calls. A swap must install
//! the other side without touching history, an accept must leave one undo
//! entry, a decline must put the base back byte for byte, and a mutation must
//! be refused by the command layer rather than hidden by the UI.

use crate::audio::PlaybackState;
use crate::commands::ai::apply_edit_to_project;
use crate::commands::preview::{accept_inner, begin_inner, cancel_inner, show_inner, state_inner};
use crate::commands::save_file_inner;
use crate::preview::{PreviewSide, READ_ONLY};
use crate::state::AppState;

use super::support::{TempDir, state_with_project};

/// A four-note pattern in one bar and one clip that plays it twice.
const SONG: &str = r#"@ocp "0.2.0"

@project "preview song"
  tempo: 120
  time_signature: 4/4

@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf

@track keys | instrument: piano/acoustic_grand
  effects:
    - gain: factor=0.5
  clip: riff [1:1 - 3:1]
"#;

/// The same pattern with one extra note written in bar 3.
const RIFF_EXTRA: &str = r#"@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @3 F4 q mf
    @8 A4 q mf
"#;

/// The same pattern with its last note moved up a fourth, in bar 2.
const RIFF_BUMPER: &str = r#"@pattern riff | instrument: piano/acoustic_grand
  events:
    C4 q mf
    @1 D4 q mf
    @2 E4 q mf
    @5 A4 q mf
"#;

/// The track with the gain half as loud and the clip twice as long.
const TRACK_QUIETER: &str = r#"@track keys | instrument: piano/acoustic_grand
  effects:
    - gain: factor=0.25
  clip: riff [1:1 - 5:1]
"#;

fn fixture(label: &str) -> (AppState, TempDir) {
    let dir = TempDir::new(label);
    let state = state_with_project(SONG, dir.path());
    {
        // Every load path installs the default instrument presets; a fixture
        // without them would make that installation look like a changed subtree.
        let mut inner = state.lock_inner();
        oc_dsl::instruments::apply_defaults(&mut inner.project);
        inner.dsl_source = oc_dsl::serialize(&inner.project);
    }
    (state, dir)
}

#[test]
fn a_side_that_defers_loading_silences_the_previous_graph() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let dir = TempDir::new("preview-deferred");
    let state = state_with_project(
        r#"@project "Deferred preview"
  tempo: 120
@preset tone
  source: builtin
  plugin: "builtin.test_tone"
@preset alternate
  source: builtin
  plugin: "builtin.test_tone"
@track lead | preset: tone, instrument: synth/lead
  @phrase melody [1:1 - 3:1]
    C4 w mf
    D4 w mf
"#,
        dir.path(),
    );
    state.lock_inner().audio.enable_simulated_output();
    crate::commands::transport::play_inner(&state).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut inner = loop {
        let inner = state.lock_inner();
        if inner.audio.simulated_output_heard_blocks() > 0 {
            break inner;
        }
        drop(inner);
        assert!(Instant::now() < deadline, "the base side never played");
        std::thread::sleep(Duration::from_millis(1));
    };
    let playback = inner.audio.render_thread.as_ref().unwrap().playback.clone();
    let before = playback.play_cursor_frames.load(Ordering::Acquire);
    let base = inner.dsl_source.clone();
    // An unused hosted preset takes the deferred path without loading a plug-in.
    let opened = begin_inner(
        &mut inner,
        "@preset alternate\n  source: clap\n  plugin: \"test.deferred\"\n".into(),
        "preset:alternate".into(),
        base,
    )
    .unwrap();
    assert_eq!(opened.preview.side, PreviewSide::Proposal);
    assert!(inner.audio.render_dirty);
    assert_eq!(inner.audio.playback_state, PlaybackState::Paused);
    assert!(playback.output_suspended.load(Ordering::Acquire));
    assert!(playback.play_cursor_frames.load(Ordering::Acquire) >= before);
    assert_eq!(playback.project_loads.load(Ordering::Acquire), 1);
    assert_eq!(
        show_inner(&mut inner, PreviewSide::Base)
            .unwrap()
            .preview
            .side,
        PreviewSide::Base
    );
    assert_eq!(inner.audio.playback_state, PlaybackState::Paused);
    let parked = playback.play_cursor_frames.load(Ordering::Acquire);
    let heard_before = inner.audio.simulated_output_heard_blocks();
    drop(inner);

    crate::commands::transport::play_inner(&state).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if state.lock_inner().audio.simulated_output_heard_blocks() > heard_before
            && playback.play_cursor_frames.load(Ordering::Acquire) > parked
        {
            break;
        }
        assert!(Instant::now() < deadline, "the base side did not resume");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(playback.project_loads.load(Ordering::Acquire), 2);
}

#[test]
fn a_preview_swaps_sides_and_a_decline_restores_the_base_byte_for_byte() {
    let (state, _dir) = fixture("preview-decline");
    let base_dsl = state.lock_inner().dsl_source.clone();
    let base_project = state.lock_inner().project.clone();

    let opened = {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl.clone(),
        )
        .expect("the proposal applies")
    };
    assert_eq!(opened.preview.side, PreviewSide::Proposal);
    assert_eq!(opened.preview.target_path, "pattern:riff");
    assert_ne!(opened.snapshot.dsl_source, base_dsl, "the proposal is live");
    assert!(
        opened.snapshot.dsl_source.contains("A4"),
        "{}",
        opened.snapshot.dsl_source
    );

    let rewound = {
        let mut inner = state.lock_inner();
        show_inner(&mut inner, PreviewSide::Base).expect("the base is one swap away")
    };
    assert_eq!(rewound.preview.side, PreviewSide::Base);
    assert_eq!(rewound.snapshot.dsl_source, base_dsl);

    let replayed = {
        let mut inner = state.lock_inner();
        show_inner(&mut inner, PreviewSide::Proposal).expect("the proposal is still in memory")
    };
    assert_eq!(replayed.preview.side, PreviewSide::Proposal);
    assert_ne!(replayed.snapshot.dsl_source, base_dsl);

    let restored = {
        let mut inner = state.lock_inner();
        cancel_inner(&mut inner).expect("decline restores the base")
    };
    assert_eq!(restored.dsl_source, base_dsl, "decline is byte-identical");
    assert_eq!(restored.project, base_project);

    let inner = state.lock_inner();
    assert!(inner.preview.is_none(), "decline closes the preview");
    assert_eq!(inner.dsl_source, base_dsl);
    assert_eq!(oc_dsl::serialize(&inner.project), base_dsl);
    assert_eq!(inner.undo.undo.len(), 0, "a swap records no undo entry");
}

#[test]
fn a_preview_accept_leaves_exactly_one_undo_entry() {
    let (state, _dir) = fixture("preview-accept");
    let base_dsl = state.lock_inner().dsl_source.clone();
    let base_project = state.lock_inner().project.clone();

    {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl.clone(),
        )
        .unwrap();
        // Auditioning both sides records nothing.
        show_inner(&mut inner, PreviewSide::Base).unwrap();
        show_inner(&mut inner, PreviewSide::Proposal).unwrap();
        assert_eq!(inner.undo.undo.len(), 0);
    }

    let accepted = {
        let mut inner = state.lock_inner();
        accept_inner(&mut inner).expect("accept commits the proposal")
    };
    assert!(accepted.dsl_source.contains("A4"));
    {
        let inner = state.lock_inner();
        assert_eq!(inner.undo.undo.len(), 1, "accept is one edit");
        assert_eq!(inner.undo.undo[0].label, "Preview edit");
        assert!(inner.preview.is_none());
        assert!(inner.dirty);
    }

    // Accepting twice must not add a second entry.
    let again = {
        let mut inner = state.lock_inner();
        accept_inner(&mut inner).expect("a second accept is a no-op")
    };
    assert_eq!(again.dsl_source, accepted.dsl_source);
    assert_eq!(state.lock_inner().undo.undo.len(), 1);

    // The entry undoes the proposal back to the base document.
    let undone = {
        let mut inner = state.lock_inner();
        inner.undo_step().expect("the preview edit undoes")
    };
    assert_eq!(undone, "Preview edit");
    let inner = state.lock_inner();
    assert_eq!(inner.dsl_source, base_dsl);
    assert_eq!(inner.project, base_project);
}

#[test]
fn a_mutation_is_refused_by_the_command_layer_while_a_preview_is_open() {
    let (state, _dir) = fixture("preview-read-only");
    let base_dsl = state.lock_inner().dsl_source.clone();
    {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl.clone(),
        )
        .unwrap();
    }

    // Saving, the agent batch and the command bodies all answer with the same
    // sentence the panel shows.
    assert_eq!(save_file_inner(None, &state).unwrap_err(), READ_ONLY);
    assert_eq!(
        crate::agent_api::transaction_begin(&state, None, "test").unwrap_err(),
        READ_ONLY
    );
    assert_eq!(state.ensure_writable().unwrap_err(), READ_ONLY);

    let shown = state.lock_inner().dsl_source.clone();
    {
        let mut inner = state.lock_inner();
        assert_eq!(
            apply_edit_to_project(
                &mut inner,
                RIFF_BUMPER.to_string(),
                "pattern:riff".to_string(),
                shown,
            )
            .unwrap_err(),
            READ_ONLY
        );
        assert_eq!(inner.undo_step().unwrap_err(), READ_ONLY);
        // The refused commands left the shown document alone.
        assert!(inner.dsl_source.contains("A4"));
        assert_eq!(inner.undo.undo.len(), 0);
    }
}

#[test]
fn a_direct_document_mutation_is_refused_and_leaves_the_shown_side_alone() {
    let (state, _dir) = fixture("preview-direct-mutation");
    let base_dsl = state.lock_inner().dsl_source.clone();
    {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl,
        )
        .unwrap();
    }
    let shown = state.lock_inner().dsl_source.clone();

    // The shape every document command has: edit the AST, then resync. The
    // refusal comes from the command layer, not from a disabled control.
    let mut inner = state.lock_inner();
    let old_project = inner.project.clone();
    inner.project.tracks[0].mix.mute = true;
    let refusal = crate::commands::shared::resync_with_undo(
        &mut inner,
        old_project,
        crate::state::UndoEdit::new("Mute", crate::state::UndoScope::MixerLevels),
    )
    .expect_err("a document edit is refused while a preview holds the document");
    assert_eq!(refusal, READ_ONLY);
    assert_eq!(inner.dsl_source, shown, "the shown side is untouched");
    assert!(
        !inner.project.tracks[0].mix.mute,
        "the refused edit was rolled back"
    );
    assert_eq!(inner.undo.undo.len(), 0, "no undo entry was recorded");
}

#[test]
fn a_proposal_made_against_another_document_is_refused() {
    let (state, _dir) = fixture("preview-stale");
    let base_dsl = state.lock_inner().dsl_source.clone();
    let mut inner = state.lock_inner();
    let error = begin_inner(
        &mut inner,
        RIFF_BUMPER.to_string(),
        "pattern:riff".to_string(),
        format!("{base_dsl}\n# moved on\n"),
    )
    .expect_err("a stale base is refused");
    assert!(
        error.contains("changed since this proposal was made"),
        "{error}"
    );
    assert!(inner.preview.is_none(), "a refused begin opens nothing");
    assert_eq!(inner.dsl_source, base_dsl, "the document did not move");
}

#[test]
fn an_accept_is_refused_when_the_base_moved_under_the_preview() {
    let (state, _dir) = fixture("preview-moved");
    let base_dsl = state.lock_inner().dsl_source.clone();
    {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl.clone(),
        )
        .unwrap();
        // An edit that bypassed the guard: the preview must not commit over it.
        inner.dsl_source = format!("{}\n# edited elsewhere\n", inner.dsl_source);
    }
    let mut inner = state.lock_inner();
    let error = accept_inner(&mut inner).expect_err("accept refuses a moved document");
    assert!(
        error.contains("changed while the edit preview was open"),
        "{error}"
    );
    assert!(inner.preview.is_some(), "the preview stays open to decline");
    assert_eq!(inner.undo.undo.len(), 0);
}

#[test]
fn preview_commands_are_idempotent() {
    let (state, _dir) = fixture("preview-idempotent");
    let base_dsl = state.lock_inner().dsl_source.clone();
    let first = {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl.clone(),
        )
        .unwrap()
    };
    // The same press twice shows the same preview instead of re-installing it.
    let second = {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl.clone(),
        )
        .expect("a repeated begin is a no-op")
    };
    assert_eq!(second.preview.side, first.preview.side);
    assert_eq!(second.snapshot.dsl_source, first.snapshot.dsl_source);
    assert_eq!(second.snapshot.revision, first.snapshot.revision);

    // A swap to the side already on screen does not install anything.
    let replayed = {
        let mut inner = state.lock_inner();
        show_inner(&mut inner, PreviewSide::Proposal).unwrap()
    };
    assert_eq!(replayed.snapshot.revision, first.snapshot.revision);

    // A second decline after the first has nothing left to restore.
    {
        let mut inner = state.lock_inner();
        cancel_inner(&mut inner).unwrap();
    }
    let twice = {
        let mut inner = state.lock_inner();
        cancel_inner(&mut inner).expect("a repeated decline is a no-op")
    };
    assert_eq!(twice.dsl_source, base_dsl);
    assert_eq!(state.lock_inner().undo.undo.len(), 0);
}

#[test]
fn a_preview_highlight_names_the_changed_pattern_and_its_bar() {
    let (state, _dir) = fixture("preview-highlight-notes");
    let base_dsl = state.lock_inner().dsl_source.clone();
    let opened = {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl,
        )
        .unwrap()
    };
    let highlight = opened.preview.highlight;
    assert_eq!(highlight.patterns, vec!["riff".to_string()]);
    assert!(
        highlight.tracks.is_empty(),
        "the track itself did not change"
    );
    // The last note left bar 1 and landed on beat 5, in bar 2, so both bars are
    // in the span: the user hears the note go and come back.
    assert_eq!(highlight.notes.len(), 1, "{highlight:?}");
    let span = &highlight.notes[0];
    assert_eq!(span.pattern, "riff");
    assert_eq!((span.start_bar, span.end_bar), (1, 2));

    // A note the proposal adds where the base had none marks one bar only.
    let (state, _dir) = fixture("preview-highlight-added-note");
    let base_dsl = state.lock_inner().dsl_source.clone();
    let opened = {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_EXTRA.to_string(),
            "pattern:riff".to_string(),
            base_dsl,
        )
        .unwrap()
    };
    let span = &opened.preview.highlight.notes[0];
    assert_eq!((span.start_bar, span.end_bar), (3, 3), "{span:?}");
}

#[test]
fn a_preview_highlight_names_the_changed_track_clip_and_effect() {
    let (state, _dir) = fixture("preview-highlight-track");
    let base_dsl = state.lock_inner().dsl_source.clone();
    let opened = {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            TRACK_QUIETER.to_string(),
            "track:keys".to_string(),
            base_dsl,
        )
        .unwrap()
    };
    let highlight = opened.preview.highlight;
    assert_eq!(highlight.tracks, vec!["keys".to_string()]);
    assert!(highlight.patterns.is_empty(), "the pattern did not change");
    // The gain is index 0 of the track's chain; the widened clip is the same
    // placement with a different range, so both indices resolve.
    assert_eq!(
        highlight.effects,
        vec![crate::preview::PreviewEffectRef {
            target: "track:keys".to_string(),
            index: 0,
        }]
    );
    assert_eq!(highlight.clips.len(), 1, "{highlight:?}");
    let clip = &highlight.clips[0];
    assert_eq!(clip.track, "keys");
    assert_eq!(clip.kind, "pattern");
    assert_eq!(clip.name, "riff");
    assert_eq!((clip.base_index, clip.proposal_index), (Some(0), Some(0)));
}

#[test]
fn the_preview_state_query_tracks_the_open_side() {
    let (state, _dir) = fixture("preview-state");
    let base_dsl = state.lock_inner().dsl_source.clone();
    assert!(state_inner(&state.lock_inner()).is_none());
    {
        let mut inner = state.lock_inner();
        begin_inner(
            &mut inner,
            RIFF_BUMPER.to_string(),
            "pattern:riff".to_string(),
            base_dsl,
        )
        .unwrap();
        assert_eq!(
            state_inner(&inner).expect("a preview is open").side,
            PreviewSide::Proposal
        );
        show_inner(&mut inner, PreviewSide::Base).unwrap();
        assert_eq!(
            state_inner(&inner).expect("a preview is open").side,
            PreviewSide::Base
        );
    }
    let mut inner = state.lock_inner();
    cancel_inner(&mut inner).unwrap();
    assert!(state_inner(&inner).is_none());
}
