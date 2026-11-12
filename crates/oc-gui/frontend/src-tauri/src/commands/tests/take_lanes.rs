//! Take lanes: comping one range and muting a whole take.

use oc_dsl::ast::{AudioClip, BarBeat, TimeRange};

use crate::commands::arrangement::{comp_audio_takes_inner, set_audio_take_mute_inner};
use crate::commands::tests::support::{default_project, make_audio_track, test_state};

fn take_clip(take: u32) -> AudioClip {
    AudioClip {
        name: format!("vox take {take}"),
        track: "vox".into(),
        file: format!("media/take-vox-{take}.wav"),
        time_range: TimeRange {
            start: BarBeat::new(1, 1, 0),
            end: BarBeat::new(5, 1, 0),
        },
        placement: Default::default(),
        offset_seconds: 0.0,
        gain_db: 0.0,
        fade_in_seconds: 0.0,
        fade_out_seconds: 0.0,
        invert: false,
        crossfade_seconds: 0.0,
        stretch: 1.0,
        stretch_mode: oc_dsl::ast::StretchMode::default(),
        tempo_follow: false,
        pitch: 0.0,
        formant: 0.0,
        bypass_processing: false,
        preserve_formants: false,
        warp_markers: Vec::new(),
        loop_seconds: 0.0,
        take_group: Some("vox take 1".into()),
        take: Some(take),
        muted: take != 3,
        gain_envelope: Vec::new(),
    }
}

fn take_project() -> crate::state::AppStateInner {
    let mut inner = test_state();
    let mut project = default_project();
    project.tracks.push(make_audio_track("vox"));
    project
        .set_audio_clips(vec![take_clip(1), take_clip(2), take_clip(3)])
        .unwrap();
    inner.project = project;
    inner
}

fn found(inner: &crate::state::AppStateInner, take: u32, start: BarBeat, end: BarBeat) -> usize {
    inner
        .project
        .audio_clips()
        .iter()
        .filter(|clip| clip.take == Some(take) && clip.time_range == TimeRange { start, end })
        .count()
}

#[test]
fn comping_a_range_splits_at_the_edges_and_mutes_the_other_takes() {
    let mut inner = take_project();
    let before = inner.undo.undo.len();
    comp_audio_takes_inner(
        &mut inner,
        "vox",
        2,
        BarBeat::new(2, 1, 0),
        BarBeat::new(3, 1, 0),
    )
    .expect("comp");

    // Three takes, each split into before / comp / after.
    assert_eq!(inner.project.audio_clip_count(), 9);
    assert_eq!(
        inner.undo.undo.len(),
        before + 1,
        "one gesture, one history entry"
    );

    let middle = |take: u32| {
        inner
            .project
            .audio_clips()
            .iter()
            .find(|clip| {
                clip.take == Some(take)
                    && clip.time_range
                        == TimeRange {
                            start: BarBeat::new(2, 1, 0),
                            end: BarBeat::new(3, 1, 0),
                        }
            })
            .unwrap_or_else(|| panic!("take {take} has a comp segment"))
            .muted
    };
    assert!(!middle(2), "the chosen take plays over the range");
    assert!(middle(1), "the first take is muted over the range");
    assert!(middle(3), "the third take is muted over the range");

    // The parts outside the comp keep their previous flags: take 1 was muted,
    // take 2 was muted, take 3 played.
    assert_eq!(
        found(&inner, 1, BarBeat::new(1, 1, 0), BarBeat::new(2, 1, 0)),
        1
    );
    assert_eq!(
        found(&inner, 2, BarBeat::new(1, 1, 0), BarBeat::new(2, 1, 0)),
        1
    );
    assert!(
        inner
            .project
            .audio_clips()
            .iter()
            .filter(|clip| clip.take == Some(2) && clip.time_range.end == BarBeat::new(2, 1, 0))
            .all(|clip| clip.muted)
    );
    assert!(
        inner
            .project
            .audio_clips()
            .iter()
            .filter(|clip| clip.take == Some(3) && clip.time_range.start == BarBeat::new(3, 1, 0))
            .all(|clip| !clip.muted)
    );
}

#[test]
fn a_take_mute_is_one_undo_entry_and_covers_every_segment() {
    let mut inner = take_project();
    comp_audio_takes_inner(
        &mut inner,
        "vox",
        2,
        BarBeat::new(2, 1, 0),
        BarBeat::new(3, 1, 0),
    )
    .expect("comp");
    let before = inner.undo.undo.len();
    set_audio_take_mute_inner(&mut inner, "vox", 2, true).expect("mute take 2");
    assert_eq!(inner.undo.undo.len(), before + 1);
    assert!(
        inner
            .project
            .audio_clips()
            .iter()
            .filter(|clip| clip.take == Some(2))
            .all(|clip| clip.muted),
        "every segment of the take is muted"
    );
    inner.undo.undo(&mut inner.project).expect("undo the mute");
    assert!(
        inner
            .project
            .audio_clips()
            .iter()
            .filter(|clip| clip.take == Some(2))
            .any(|clip| !clip.muted),
        "one undo restores the comp"
    );
}

#[test]
fn comping_a_range_no_take_covers_is_refused() {
    let mut inner = take_project();
    let before = inner.project.clone();
    let error = comp_audio_takes_inner(
        &mut inner,
        "vox",
        2,
        BarBeat::new(9, 1, 0),
        BarBeat::new(10, 1, 0),
    )
    .unwrap_err();
    assert!(error.contains("No take 2"), "{error}");
    assert_eq!(inner.project, before);
}

#[test]
fn take_solo_is_a_session_overlay_and_leaves_the_document_alone() {
    let inner = take_project();
    let before = inner.project.clone();
    let audition = crate::audio::take_solo_project(&inner.project, "vox", 2);
    assert_eq!(inner.project, before, "the document is untouched");
    assert!(
        audition
            .audio_clips()
            .iter()
            .filter(|clip| clip.take != Some(2))
            .all(|clip| clip.muted),
        "the other passes are muted in the audition"
    );
    assert!(
        audition
            .audio_clips()
            .iter()
            .filter(|clip| clip.take == Some(2))
            .all(|clip| !clip.muted),
        "the soloed pass plays"
    );
    let dsl = oc_dsl::serialize(&inner.project);
    assert_eq!(dsl, oc_dsl::serialize(&before), "solo writes no DSL");
}
