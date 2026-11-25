use oc_dsl::{ast::RouteKind, parser::parse, serializer::serialize, validator::validate};

const SCORE: &str = "@project\n  tempo: 120\n@track notes | instrument: midi/out, channel: 2\n  @phrase keys [1:1 - 2:1]\n    C4 q v=100\n@bus vocoder\n  routes: [master]\n  effects:\n    - plugin:\"au.aumf:test:demo\"\n  route: midi notes -> fx1\n";

#[test]
fn midi_routes_roundtrip_without_becoming_audio_routes() {
    let project = parse(SCORE).unwrap();
    validate(&project).unwrap();
    assert!(project.tracks[0].instrument.is_midi_only());
    assert_eq!(project.tracks[0].channel, Some(2));
    assert_eq!(project.buses[0].input_routes[0].kind, RouteKind::Midi);
    let saved = serialize(&project);
    assert!(saved.contains("route: midi notes -> fx1"));
    assert_eq!(parse(&saved).unwrap(), project);
    let json = serde_json::to_string(&project).unwrap();
    assert!(json.contains("\"kind\":\"midi\""));
}
#[test]
fn midi_routes_refuse_missing_sources_wrong_targets_and_duplicate_inputs() {
    for score in [
        SCORE.replace("midi notes ->", "midi absent ->"),
        SCORE.replace("plugin:\"au.aumf:test:demo\"", "gain: factor=1"),
        SCORE.to_string() + "  route: midi notes -> fx1\n",
    ] {
        assert!(validate(&parse(&score).unwrap()).is_err());
    }
}
#[test]
fn midi_only_sources_refuse_hidden_audio_configuration() {
    let mut project = parse(SCORE).unwrap();
    project.tracks[0].preset = Some("hidden".into());
    assert!(oc_dsl::instruments::validate_midi_only_source(&project, &project.tracks[0]).is_err());
    assert!(validate(&project).is_err());
}
