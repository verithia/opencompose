use oc_dsl::ast::Articulation;

/// Every articulation round-trips through the token the DSL writes, so an
/// editor can set one by name without keeping a second table.
#[test]
fn every_articulation_round_trips_through_its_token() {
    for articulation in Articulation::ALL {
        let token = articulation.as_token();
        assert_eq!(
            Articulation::from_token(token),
            Some(articulation),
            "token '{token}' does not name {articulation:?}",
        );
    }
}

#[test]
fn a_note_keeps_its_articulation_through_parse_and_serialize() {
    for articulation in Articulation::ALL {
        let token = articulation.as_token();
        let source = format!(
            "@project \"tokens\"\n@track lead | instrument:piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    C4 q mf articulation={token}\n",
        );
        let project = oc_dsl::parse(&source).unwrap();
        oc_dsl::validate(&project).unwrap();
        let note = &project.tracks[0].phrases[0].events[0];
        assert_eq!(note.articulation, Some(articulation), "parsed {token}");
        let serialized = oc_dsl::serialize(&project);
        assert!(
            serialized.contains(&format!("articulation={token}")),
            "serialized {token}: {serialized}",
        );
    }
}

#[test]
fn an_unknown_token_names_no_articulation() {
    assert_eq!(Articulation::from_token("col_legno_ish"), None);
    assert_eq!(Articulation::from_token(""), None);
}

/// A hit's `articulation=` that names no known token stays a generic modifier,
/// so it must survive parse -> serialize -> parse like any other modifier.
#[test]
fn a_hit_keeps_an_unknown_articulation_modifier() {
    let source = "@ocp \"0.2.0\"\n@project \"hit tokens\"\n  tempo: 120\n@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    snare 4:0 f articulation=accent\n";
    let project = oc_dsl::parse(source).unwrap();
    let hit = &project.patterns[0].hits[0];
    assert_eq!(hit.articulation, None);
    assert_eq!(
        hit.modifiers,
        vec![(
            "articulation".to_string(),
            oc_dsl::ast::ParamValue::String("accent".into())
        )],
        "the parser keeps the modifier",
    );

    let serialized = oc_dsl::serialize(&project);
    assert!(
        serialized.contains("snare 4:0 f articulation=accent"),
        "the serializer writes it back: {serialized}",
    );
    let again = oc_dsl::parse(&serialized).unwrap();
    assert_eq!(
        again.patterns[0].hits[0].modifiers, hit.modifiers,
        "the modifier survives the round trip",
    );
}
