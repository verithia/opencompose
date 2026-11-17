use oc_dsl::ops::AstPath;
use oc_dsl::search::{SearchKind, SearchRequest, search};

const FIXTURE: &str = r#"@project "Search Fixture"
  tempo: 120
  time_signature: 4/4

@preset "Warm Pad"
  source: builtin
  plugin: "builtin.open_synth"
  patch: "Pads/Warm"

@pattern "Bass Line" | instrument: bass/electric_bass
  events:
    C2 q mf

@section chorus [1:1 - 9:1]
  label: "Big Chorus"

@marker "Drop Two" at 9:1

@bus reverb
  effects:
    - reverb: mix=20%

@track "Lead Guitar" | instrument: guitar/electric, preset: "Warm Pad"
  effects:
    - delay: time=200ms
  @phrase "Main Hook" [1:1 - 3:1]
    C4 q mf

@master
  effects:
    - limiter: ceiling=-1dB
"#;

fn fixture() -> oc_dsl::ast::Project {
    oc_dsl::parse(FIXTURE).expect("search fixture parses")
}

#[test]
fn exact_prefix_and_substring_matches_rank_in_that_order() {
    let project = oc_dsl::parse(
        r#"@project "Ranking"
@track bass | instrument: synth/bass
@track bassline | instrument: synth/bass
@track subbass | instrument: synth/bass
"#,
    )
    .expect("ranking fixture parses");
    let hits = search(&project, &SearchRequest::new("bass"));
    assert_eq!(
        hits.iter().map(|hit| hit.name.as_str()).collect::<Vec<_>>(),
        ["bass", "bassline", "subbass"]
    );
}

#[test]
fn kind_filter_limits_the_result_set() {
    let project = fixture();
    let only = [SearchKind::Preset];
    let request = SearchRequest {
        query: "a",
        kinds: Some(&only),
        limit: 20,
    };
    let hits = search(&project, &request);
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|hit| hit.kind == SearchKind::Preset));
    assert_eq!(hits[0].path, "preset:Warm Pad");
}

#[test]
fn limit_bounds_the_result_count() {
    let project = oc_dsl::parse(
        r#"@project "Many"
@track "stem one" | instrument: synth/lead
@track "stem two" | instrument: synth/lead
@track "stem three" | instrument: synth/lead
"#,
    )
    .expect("many fixture parses");
    let request = SearchRequest {
        query: "stem",
        kinds: None,
        limit: 2,
    };
    assert_eq!(search(&project, &request).len(), 2);
    let request = SearchRequest {
        query: "stem",
        kinds: None,
        limit: 500,
    };
    assert!(search(&project, &request).len() <= oc_dsl::search::MAX_LIMIT);
}

#[test]
fn an_empty_or_oversized_query_returns_nothing() {
    let project = fixture();
    assert!(search(&project, &SearchRequest::new("  ")).is_empty());
    let long = "x".repeat(oc_dsl::search::MAX_QUERY_CHARS + 1);
    assert!(search(&project, &SearchRequest::new(&long)).is_empty());
}

#[test]
fn every_returned_path_parses() {
    let project = fixture();
    for query in ["a", "e", "o", "warm", "guitar", "chorus", "drop", "limiter"] {
        for hit in search(&project, &SearchRequest::new(query)) {
            AstPath::parse(&hit.path)
                .unwrap_or_else(|error| panic!("{} does not parse: {error}", hit.path));
        }
    }
}

#[test]
fn labels_types_and_instruments_are_searchable() {
    let project = fixture();
    let chorus = search(&project, &SearchRequest::new("big chorus"));
    assert!(
        chorus
            .iter()
            .any(|hit| hit.kind == SearchKind::Section && hit.path == "sections")
    );
    let guitar = search(&project, &SearchRequest::new("guitar"));
    assert!(
        guitar.iter().any(|hit| hit.kind == SearchKind::Track),
        "{guitar:?}"
    );
    let delay = search(&project, &SearchRequest::new("delay"));
    assert!(
        delay
            .iter()
            .any(|hit| hit.kind == SearchKind::Effect && hit.path.ends_with(".effects")),
        "{delay:?}"
    );
    let patch = search(&project, &SearchRequest::new("pads/warm"));
    assert!(patch.iter().any(|hit| hit.kind == SearchKind::Preset));
}

#[test]
fn sections_and_markers_report_the_sections_path() {
    let project = fixture();
    for query in ["chorus", "drop two"] {
        let hits = search(&project, &SearchRequest::new(query));
        assert!(
            hits.iter()
                .any(|hit| hit.path == "sections" && hit.kind != SearchKind::Track)
        );
    }
}

#[test]
fn unicode_names_match_case_insensitively() {
    let project = oc_dsl::parse(
        r#"@project "Unicode"
@track "Bäss Übergang" | instrument: synth/pad
"#,
    )
    .expect("unicode fixture parses");
    let hits = search(&project, &SearchRequest::new("BÄSS"));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].name, "Bäss Übergang");
    AstPath::parse(&hits[0].path).expect("a Unicode name is a path");
}

#[test]
fn a_name_with_a_dot_falls_back_to_an_index_track_path() {
    let project = oc_dsl::parse(
        r#"@project "Dotted"
@track "Bass.One" | instrument: synth/bass
"#,
    )
    .expect("dotted fixture parses");
    let hits = search(&project, &SearchRequest::new("bass.one"));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].path, "track:0");
    AstPath::parse(&hits[0].path).expect("the fallback path parses");
}

#[test]
fn results_are_stable_for_equal_scores() {
    let project = oc_dsl::parse(
        r#"@project "Stable"
@track same | instrument: synth/lead
@track Same | instrument: synth/lead
"#,
    )
    .expect("stable fixture parses");
    let first = search(&project, &SearchRequest::new("same"));
    assert_eq!(first.len(), 2);
    assert_eq!(
        first.iter().map(|hit| hit.path.clone()).collect::<Vec<_>>(),
        ["track:same", "track:Same"]
    );
}

#[test]
fn numeric_and_child_like_track_names_extract_the_matching_track() {
    let mut project = fixture();
    for name in ["0", "01", "Lead Guitar.mix", "Lead Guitar.effects"] {
        let mut track = project.tracks[0].clone();
        track.name = name.into();
        project.tracks.push(track);
        let hit = search(&project, &SearchRequest::new(name))
            .into_iter()
            .find(|hit| hit.kind == SearchKind::Track && hit.name == name)
            .unwrap();
        let extracted = oc_dsl::ops::extract(&project, &hit.path).unwrap();
        assert_eq!(extracted.affected_tracks, vec![project.tracks.len() - 1]);
        assert!(extracted.dsl_text.contains(name), "{}", extracted.dsl_text);
    }
}

#[test]
fn numeric_and_child_like_item_names_preserve_their_identity() {
    let mut project = fixture();
    for name in ["0", "Main Hook.effects"] {
        let mut phrase = project.tracks[0].phrases[0].clone();
        phrase.name = name.into();
        project.tracks[0].phrases.push(phrase);
        let hit = search(&project, &SearchRequest::new(name))
            .into_iter()
            .find(|hit| hit.kind == SearchKind::Phrase && hit.name == name)
            .unwrap();
        let extracted = oc_dsl::ops::extract(&project, &hit.path).unwrap();
        assert!(extracted.dsl_text.contains(name), "{}", extracted.dsl_text);

        let mut pattern = project.patterns[0].clone();
        pattern.name = name.into();
        project.patterns.push(pattern);
        let hit = search(&project, &SearchRequest::new(name))
            .into_iter()
            .find(|hit| hit.kind == SearchKind::Pattern && hit.name == name)
            .unwrap();
        let extracted = oc_dsl::ops::extract(&project, &hit.path).unwrap();
        assert!(extracted.dsl_text.contains(name), "{}", extracted.dsl_text);
    }
}

#[test]
fn unaddressable_preset_and_bus_names_do_not_alias_other_objects() {
    let mut project = fixture();
    let mut preset = project.presets[0].clone();
    preset.name = "Warm Pad.effects".into();
    project.presets.push(preset);
    let mut bus = project.buses[0].clone();
    bus.name = "reverb.effects".into();
    project.buses.push(bus);
    for name in ["Warm Pad.effects", "reverb.effects"] {
        assert!(search(&project, &SearchRequest::new(name)).is_empty());
    }
}
