//! Production examples stay executable at the typed tool boundary.

use super::{curriculum, read};
use crate::agent_tools::{audio_analysis::CompareArgs, audio_clip::AudioClipSetPlaybackArgs};
use crate::tool_registry::arguments::ToolArguments;

fn example(id: &str) -> serde_json::Value {
    let skill = read(id).unwrap();
    let body = skill.body.split("```json\n").nth(1).unwrap();
    serde_json::from_str(body.split("```").next().unwrap()).unwrap()
}

#[test]
fn the_loop_fit_example_uses_a_duration_ratio_without_changing_the_trim() {
    let args = AudioClipSetPlaybackArgs::decode(&example("fit-loops-with-pitch-and-time"))
        .expect("the taught request matches the production argument schema");
    assert_eq!(args.clip_index, 0);
    let update: crate::commands::AudioClipPlayback = args.playback.into();
    assert_eq!(update.tempo_follow, Some(false));
    assert_eq!(
        update.stretch_mode,
        Some(oc_dsl::ast::StretchMode::Percussive)
    );
    // Four source beats must occupy four destination beats, not the inverse.
    let source_seconds = 4.0 * 60.0 / 100.0;
    let target_seconds = 4.0 * 60.0 / 128.0;
    assert!((source_seconds * update.stretch.unwrap() - target_seconds).abs() < 1e-9);
    assert!(update.stretch_to.is_none());
    assert!(update.warp_markers.is_none());
    assert!(update.pitch.is_none());
}

#[test]
fn the_diagnosis_example_requests_full_rate_stereo_listening_without_alignment() {
    let args = CompareArgs::decode(&example("diagnose-a-sound"))
        .expect("the taught request matches the production argument schema");
    assert_eq!(args.duration, Some(4.0));
    assert_eq!(args.listen, Some(true));
    assert_eq!(args.listening_only, Some(true));
    assert_eq!(args.channel.as_deref(), Some("stereo"));
    assert_eq!(args.clip_sample_rate, Some(48_000));
    assert_ne!(args.reference, args.candidate);
}

#[test]
fn production_tasks_resolve_to_their_specific_workflows() {
    for (topic, workflow) in [
        ("Sample selection by role", "select-samples-by-role"),
        ("Loop pitch and time", "fit-loops-with-pitch-and-time"),
        ("Sampled drum-kit mapping", "assemble-a-sampled-drum-kit"),
        ("Vocal chop phrasing", "make-vocal-chop-phrases"),
        ("Playable resampling", "resample-a-playable-instrument"),
        (
            "Sample transition arrivals",
            "build-sample-based-transitions",
        ),
        (
            "Printed and live layer mixing",
            "mix-printed-and-live-layers",
        ),
        (
            "Compression recovery listening",
            "hear-compression-and-recovery",
        ),
        (
            "Production control translation",
            "translate-a-production-technique",
        ),
        ("Techno kick rumble", "build-a-techno-rumble"),
        ("Audible sound diagnosis", "diagnose-a-sound"),
        (
            "Complete arrangement review",
            "review-a-complete-arrangement",
        ),
        (
            "Pad-map intersection before drum processing",
            "verify-sample-trigger-and-source-state",
        ),
        ("Silent parent path diagnosis", "route-groups-and-returns"),
        (
            "Accumulated repeat drift and recovery",
            "fit-loops-with-pitch-and-time",
        ),
    ] {
        let exercise = curriculum::read_topic(topic).unwrap();
        assert_eq!(exercise.title, topic);
        assert!(exercise.body.contains(&format!("id `{workflow}`")));
        let skill = read(workflow).expect("the recommended workflow is bundled");
        assert_eq!(skill.id, workflow);
    }
}

#[test]
fn every_skill_a_body_tells_the_model_to_read_is_bundled() {
    let ids: Vec<&str> = super::BUNDLED.iter().map(|(id, _)| *id).collect();
    let mut checked = 0;
    for (owner, _) in super::BUNDLED {
        let body = read(owner).unwrap().body;
        let words: Vec<&str> = body
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'))
            .filter(|word| !word.is_empty())
            .collect();
        for pair in words.windows(2) {
            let [verb, target] = pair else { continue };
            // A hyphenated word after "read" is a skill id; plain words are prose.
            if verb.eq_ignore_ascii_case("read") && super::valid_id(target) && target.contains('-')
            {
                assert!(
                    ids.contains(target),
                    "{owner} refers to unknown skill {target}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 40, "only {checked} references were found");
}

#[test]
fn taught_detector_routes_parse_into_distinct_silent_device_inputs() {
    use oc_device_schema::{PortKind, find_device};
    use oc_dsl::ast::RouteKind;

    let skill = read("route-groups-and-returns").unwrap();
    let routes: Vec<_> = skill
        .body
        .split('`')
        .filter(|part| part.starts_with("route: "))
        .collect();
    assert_eq!(routes.len(), 2);
    for route in routes {
        let source = format!(
            "@project\n  tempo: 120\n  time_signature: 4/4\n\n\
             @track vocal | instrument: synth/lead\n\n\
             @control_source env | from: track(vocal), attack: 5ms, release: 120ms\n\n\
             @bus wet\n  routes: [master]\n  effects:\n    - compression: threshold=-24dB\n  {route}\n"
        );
        let project = oc_dsl::parse(&source).expect("the taught route parses");
        oc_dsl::validate(&project).expect("the declared source and effect resolve");
        let edge = &project.buses[0].input_routes[0];
        let (source_name, port_kind) = match edge.kind {
            RouteKind::Audio => ("vocal", PortKind::Audio),
            RouteKind::Control => ("env", PortKind::Control),
        };
        assert_eq!(edge.source, source_name);
        assert_eq!(edge.target, "compression");
        assert!(
            find_device("compression")
                .unwrap()
                .ports
                .iter()
                .any(|port| port.kind == port_kind && !port.sums)
        );

        let unsupported = source
            .replace("- compression:", "- gate:")
            .replace("-> compression", "-> gate");
        let gate_project = oc_dsl::parse(&unsupported).unwrap();
        assert!(oc_dsl::validate(&gate_project).is_err());
    }
}
