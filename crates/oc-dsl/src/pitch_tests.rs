//! Pitch domain and persisted-notation regressions.
use crate::ast::{Accidental, NoteName, Pitch, Project};

#[test]
fn midi_pitch_round_trip_covers_all_128_notes_without_low_octave_transposition() {
    for midi in 0..=127 {
        let pitch = Pitch::from_midi(midi).unwrap();
        assert_eq!(pitch.checked_to_midi(), Some(midi));
        assert_eq!(pitch.to_midi(), midi);
        assert_eq!(Pitch::from_text(&pitch.to_text()), Some(pitch));
        let parsed = project_with(&pitch.to_text());
        assert_eq!(
            parsed.tracks[0].phrases[0].events[0]
                .pitch
                .checked_to_midi(),
            Some(midi)
        );
        let reparsed = crate::parse(&crate::serialize(&parsed)).unwrap();
        assert_eq!(
            reparsed.tracks[0].phrases[0].events[0]
                .pitch
                .checked_to_midi(),
            Some(midi)
        );
        let json = serde_json::to_value(pitch).unwrap();
        assert_eq!(serde_json::from_value::<Pitch>(json).unwrap(), pitch);
    }
    for midi in 128..=255 {
        assert!(Pitch::from_midi(midi).is_none());
    }
    assert_eq!(Pitch::from_midi(0).unwrap().to_text(), "C-1");
    assert_eq!(Pitch::from_midi(11).unwrap().to_text(), "B-1");
    assert_eq!(Pitch::from_midi(63).unwrap().to_text(), "Eb4");
    assert_eq!(Pitch::from_midi(68).unwrap().to_text(), "Ab4");
    assert_eq!(Pitch::from_midi(70).unwrap().to_text(), "Bb4");
    assert_eq!(Pitch::from_midi(127).unwrap().to_text(), "G9");
}

#[test]
fn written_pitch_domain_is_distinct_from_checked_midi_domain() {
    for text in ["A9", "B9", "B#9", "Cb-1", "Cbb-1", "G##9"] {
        let pitch = Pitch::from_text(text).unwrap();
        assert_eq!(pitch.to_text(), text);
        assert_eq!(pitch.checked_to_midi(), None, "{text}");
    }
    for text in ["C-2", "C10", "C+4", "C-0", "C-01", "C4extra", "C", "é4"] {
        assert!(Pitch::from_text(text).is_none(), "{text}");
    }
    assert_eq!(
        Pitch::from_text("B#-1").unwrap().checked_to_midi(),
        Some(12)
    );
    assert_eq!(Pitch::from_text("Cb0").unwrap().checked_to_midi(), Some(11));
    let pitch = Pitch::from_text("F#3").unwrap();
    assert_eq!(
        serde_json::to_value(pitch).unwrap(),
        serde_json::json!({"note":"F", "accidental":"Sharp", "octave":3})
    );
    assert_eq!(pitch.to_midi(), 54);
    assert_eq!(Pitch::from_text("A9").unwrap().to_midi(), 129);
    let below = Pitch::from_text("Cb-1").unwrap();
    assert_eq!(below.semitone_number(), -1);
    assert!(below.to_freq() < Pitch::from_midi(0).unwrap().to_freq());
    assert!(crate::validate(&project_with("Cb-1")).is_err());
}

fn project_with(text: &str) -> Project {
    crate::parse(&format!("@project \"Pitch\"\n  tempo: 120\n@track piano | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 3:1]\n    {text} q mf\n")).unwrap()
}

#[test]
fn octave_minus_one_survives_dsl_chords_and_json_project_boundaries() {
    let project = project_with("[C-1 Eb-1 B-1]");
    crate::validate(&project).unwrap();
    assert_eq!(
        project.tracks[0].phrases[0]
            .events
            .iter()
            .map(|note| note.pitch.to_midi())
            .collect::<Vec<_>>(),
        [0, 3, 11]
    );
    let serialized = crate::serialize(&project);
    let reparsed = crate::parse(&serialized).unwrap();
    assert_eq!(
        reparsed.tracks[0].phrases[0].events,
        project.tracks[0].phrases[0].events
    );
    assert!(
        crate::validate(&project_with("A9")).is_ok(),
        "existing positive musical spelling remains valid"
    );
    for octave in [-128, -2, 10, 127] {
        let mut json = serde_json::to_value(&project).unwrap();
        json["tracks"][0]["phrases"][0]["events"][0]["pitch"]["octave"] = serde_json::json!(octave);
        let invalid: Project = serde_json::from_value(json).unwrap();
        let errors = crate::validate(&invalid).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.to_string().contains("pitch octave")),
            "{errors:?}"
        );
    }
    for octave in [-129, 128, 255] {
        let mut json = serde_json::to_value(&project).unwrap();
        json["tracks"][0]["phrases"][0]["events"][0]["pitch"]["octave"] = serde_json::json!(octave);
        assert!(serde_json::from_value::<Project>(json).is_err());
    }
    let mut preset_project = project.clone();
    let mut preset = crate::ast::PresetDef {
        name: "sample".into(),
        source: Some("sample".into()),
        plugin: None,
        program: None,
        patch: None,
        file: Some("sample.wav".into()),
        root_note: Some(Pitch {
            note: NoteName::C,
            accidental: Accidental::Natural,
            octave: 127,
        }),
        params: vec![],
        effects: vec![],
        extends: None,
    };
    preset_project.presets.push(preset.clone());
    assert!(
        crate::validate(&preset_project)
            .unwrap_err()
            .iter()
            .any(|error| error.to_string().contains("pitch octave"))
    );
    preset.root_note = Pitch::from_midi(0);
    preset_project.presets[0] = preset;
    assert!(crate::validate(&preset_project).is_ok());
    let extreme = Pitch {
        note: NoteName::C,
        accidental: Accidental::Natural,
        octave: -128,
    };
    assert_eq!(extreme.checked_to_midi(), None);
}

#[test]
fn vocal_pitch_targets_validate_their_midi_domain_not_scientific_octave() {
    let source = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  notes:\n    - at=0s tgt=C-1\n@track vocal | instrument: audio/clip\n";
    let mut project = crate::parse(source).unwrap();
    for midi in [0, 127] {
        project.properties.vocal_tunes[0].notes[0].target =
            Some(crate::ast::VocalTarget::Pitch(midi));
        crate::validate(&project).unwrap();
    }
    for midi in [128, 255] {
        project.properties.vocal_tunes[0].notes[0].target =
            Some(crate::ast::VocalTarget::Pitch(midi));
        assert!(
            crate::validate(&project)
                .unwrap_err()
                .iter()
                .any(|error| error.to_string().contains("MIDI note"))
        );
    }
}
