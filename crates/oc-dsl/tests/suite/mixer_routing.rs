//! Send taps and channel overrides: parse, format, JSON and v0.2 defaults.

use oc_dsl::ast::Project;

const SOURCE: &str = r##"@ocp "0.2.0"

@project "mixer routing"
  tempo: 120
  time_signature: 4/4

@track keys | instrument: synth/lead_square
  mix: vol=-6dB, pan=0.25, send(space=-6dB, pre), send(delay=25%), polarity=invert, display_name="Keys Wide", color="#ff8800", solo_safe=true
  @phrase line [1:1 - 2:1]
    C4 w mf

@bus space
  routes: [master]
@bus delay
  routes: [master]
"##;

#[test]
fn sends_and_overrides_round_trip_through_fmt_and_validate() {
    let project = oc_dsl::parse(SOURCE).unwrap();
    oc_dsl::validate(&project).unwrap();

    let mix = &project.tracks[0].mix;
    assert_eq!(mix.sends.len(), 2);
    assert_eq!(mix.sends[0].target, "space");
    assert!(mix.sends[0].pre_fader);
    assert!((mix.sends[0].amount - 10.0_f64.powf(-6.0 / 20.0)).abs() < 1e-12);
    assert_eq!(mix.sends[1].target, "delay");
    assert!(!mix.sends[1].pre_fader);
    assert_eq!(mix.sends[1].amount, 0.25);
    assert!(mix.polarity_inverted);
    assert_eq!(mix.overrides.display_name.as_deref(), Some("Keys Wide"));
    assert_eq!(mix.overrides.color.as_deref(), Some("#ff8800"));
    assert!(mix.overrides.solo_safe);

    let text = oc_dsl::serialize(&project);
    for expected in [
        "pre",
        "polarity=invert",
        "display_name=\"Keys Wide\"",
        "color=\"#ff8800\"",
        "solo_safe=true",
    ] {
        assert!(text.contains(expected), "missing {expected} in:\n{text}");
    }

    let reopened = oc_dsl::parse(&text).unwrap();
    oc_dsl::validate(&reopened).unwrap();
    assert_eq!(
        serde_json::to_value(&project).unwrap(),
        serde_json::to_value(&reopened).unwrap()
    );
}

#[test]
fn json_export_carries_sends_polarity_and_overrides() {
    let project = oc_dsl::parse(SOURCE).unwrap();
    let value = serde_json::to_value(&project).unwrap();
    let mix = &value["tracks"][0]["mix"];
    assert_eq!(mix["sends"][0]["pre_fader"], true);
    assert_eq!(mix["sends"][0]["target"], "space");
    assert_eq!(mix["sends"][1]["pre_fader"], false);
    assert_eq!(mix["polarity_inverted"], true);
    assert_eq!(mix["overrides"]["display_name"], "Keys Wide");
    assert_eq!(mix["overrides"]["color"], "#ff8800");
    assert_eq!(mix["overrides"]["solo_safe"], true);
}

#[test]
fn v02_documents_without_new_keys_keep_their_defaults() {
    let project = oc_dsl::parse(
        "@project \"legacy\"\n  tempo: 120\n@track keys | instrument: piano/acoustic_grand\n  mix: send(reverb=25%)\n@bus reverb\n  routes: [master]\n",
    )
    .unwrap();
    let send = &project.tracks[0].mix.sends[0];
    assert!(!send.pre_fader, "post-fader is the v0.2 default");
    assert!(!project.tracks[0].mix.polarity_inverted);
    assert!(project.tracks[0].mix.overrides.is_default());

    let mut value = serde_json::to_value(&project).unwrap();
    let mix = value["tracks"][0]["mix"].as_object_mut().unwrap();
    mix.remove("polarity_inverted");
    mix.remove("overrides");
    mix["sends"][0].as_object_mut().unwrap().remove("pre_fader");
    mix["sends"][0].as_object_mut().unwrap().remove("pan");
    let restored: Project = serde_json::from_value(value).unwrap();
    assert!(!restored.tracks[0].mix.polarity_inverted);
    assert!(restored.tracks[0].mix.overrides.is_default());
    assert!(!restored.tracks[0].mix.sends[0].pre_fader);
    assert_eq!(restored.tracks[0].mix.sends[0].pan, 0.0);
    assert_eq!(restored.buses[0].volume_db, 0.0);
    assert!(!restored.buses[0].mute);
    assert!(!restored.buses[0].solo);
}

#[test]
fn bus_return_fader_mute_and_solo_round_trip_through_dsl_and_json() {
    let source = "@ocp \"0.2.0\"\n@project \"bus strip\"\n  tempo: 120\n\
        @bus glue\n  mix: vol=-4.5dB, mute=true, solo=true\n  routes: [master]\n\
        @bus room\n  routes: [master]\n";
    let project = oc_dsl::parse(source).unwrap();
    oc_dsl::validate(&project).unwrap();
    let glue = &project.buses[0];
    assert!((glue.volume_db - -4.5).abs() < 1e-12);
    assert!(glue.mute);
    assert!(glue.solo);
    assert_eq!(project.buses[1].volume_db, 0.0);
    assert!(!project.buses[1].mute && !project.buses[1].solo);

    let value = serde_json::to_value(&project).unwrap();
    assert_eq!(value["buses"][0]["volume_db"], -4.5);
    assert_eq!(value["buses"][0]["mute"], true);
    assert_eq!(value["buses"][0]["solo"], true);

    let text = oc_dsl::serialize(&project);
    assert!(
        text.contains("mix: vol=-4.5dB, mute=true, solo=true"),
        "{text}"
    );
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&project).unwrap(),
        serde_json::to_value(&reopened).unwrap()
    );
    // The untouched bus keeps its legacy spelling.
    assert!(!text.contains("@bus room\n  mix:"));
}

#[test]
fn per_send_pan_round_trips_and_validates() {
    let source = "@ocp \"0.2.0\"\n@project \"send pan\"\n  tempo: 120\n\
        @track keys | instrument: synth/lead_square\n  mix: send(space=50%, pan=-0.35), send(delay=+6dB, pre, pan=0.75)\n\
        @bus space\n  routes: [master]\n@bus delay\n  routes: [master]\n";
    let project = oc_dsl::parse(source).unwrap();
    oc_dsl::validate(&project).unwrap();
    let sends = &project.tracks[0].mix.sends;
    assert!((sends[0].pan - -0.35).abs() < 1e-12);
    assert!(!sends[0].pre_fader);
    assert!((sends[1].pan - 0.75).abs() < 1e-12);
    assert!(sends[1].pre_fader);
    // +6 dB is allowed: the send ceiling is +12 dB.
    assert!((sends[1].amount - 10.0_f64.powf(6.0 / 20.0)).abs() < 1e-12);

    let value = serde_json::to_value(&project).unwrap();
    assert_eq!(value["tracks"][0]["mix"]["sends"][0]["pan"], -0.35);
    assert_eq!(value["tracks"][0]["mix"]["sends"][1]["pan"], 0.75);

    let text = oc_dsl::serialize(&project);
    assert!(text.contains("send(space=50%, pan=-0.35)"), "{text}");
    assert!(text.contains("pre, pan=0.75)"), "{text}");
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&project).unwrap(),
        serde_json::to_value(&reopened).unwrap()
    );

    let outside = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: synth/lead_square\n  mix: send(space=50%, pan=1.5)\n@bus space\n",
    )
    .unwrap();
    assert!(
        oc_dsl::validate(&outside).is_err(),
        "pan outside -1..1 must fail"
    );
}

#[test]
fn send_amount_ceiling_is_twelve_db() {
    let at_ceiling = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: synth/lead_square\n  mix: send(space=+12dB)\n@bus space\n",
    )
    .unwrap();
    assert!(oc_dsl::validate(&at_ceiling).is_ok());
    assert!((at_ceiling.tracks[0].mix.sends[0].amount - 10.0_f64.powf(12.0 / 20.0)).abs() < 1e-12);

    let above = oc_dsl::parse(
        "@project\n  tempo: 120\n@track t | instrument: synth/lead_square\n  mix: send(space=+13dB)\n@bus space\n",
    )
    .unwrap();
    assert!(
        oc_dsl::validate(&above).is_err(),
        "sends above +12 dB must fail"
    );
}

#[test]
fn latency_compensation_policy_round_trips_and_defaults_to_on() {
    use oc_dsl::ast::LatencyCompensation;

    let project = oc_dsl::parse(
        "@ocp \"0.2.0\"\n@project \"pdc\"\n  tempo: 120\n\
         @track keys | instrument: keys/piano\n  mix: pdc=when_recording\n\
         @track pad | instrument: keys/piano\n  mix: pdc=off\n\
         @track bass | instrument: keys/piano\n",
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    assert_eq!(
        project.tracks[0].mix.latency_compensation,
        LatencyCompensation::WhenRecording
    );
    assert_eq!(
        project.tracks[1].mix.latency_compensation,
        LatencyCompensation::Off
    );
    assert_eq!(
        project.tracks[2].mix.latency_compensation,
        LatencyCompensation::On,
        "a document without the key keeps today's behaviour"
    );
    assert!(!LatencyCompensation::WhenRecording.bypasses(false));
    assert!(LatencyCompensation::WhenRecording.bypasses(true));
    assert!(LatencyCompensation::Off.bypasses(false));

    let text = oc_dsl::serialize(&project);
    assert!(text.contains("pdc=when_recording"), "{text}");
    assert!(text.contains("pdc=off"), "{text}");
    assert_eq!(text.matches("pdc=").count(), 2, "{text}");
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&project).unwrap(),
        serde_json::to_value(&reopened).unwrap()
    );

    // The default is omitted from JSON too, so existing exports stay stable.
    let value = serde_json::to_value(&project).unwrap();
    assert!(value["tracks"][0]["mix"]["latency_compensation"].is_string());
    assert!(
        value["tracks"][2]["mix"]
            .get("latency_compensation")
            .is_none()
    );
}

#[test]
fn manual_latency_offset_round_trips_and_defaults_to_zero() {
    let project = oc_dsl::parse(
        "@ocp \"0.2.0\"\n@project \"offset\"\n  tempo: 120\n\
         @track early | instrument: keys/piano\n  mix: latency=-128\n\
         @track late | instrument: keys/piano\n  mix: latency=+512\n\
         @track plain | instrument: keys/piano\n",
    )
    .unwrap();
    oc_dsl::validate(&project).unwrap();
    assert_eq!(project.tracks[0].mix.latency_offset, -128);
    assert_eq!(project.tracks[1].mix.latency_offset, 512);
    assert_eq!(
        project.tracks[2].mix.latency_offset, 0,
        "a document without the key keeps today's behaviour"
    );

    let text = oc_dsl::serialize(&project);
    assert!(text.contains("latency=-128"), "{text}");
    assert!(text.contains("latency=512"), "{text}");
    assert_eq!(text.matches("latency=").count(), 2, "{text}");
    let reopened = oc_dsl::parse(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&project).unwrap(),
        serde_json::to_value(&reopened).unwrap()
    );

    // A zero offset is omitted from JSON, so existing exports stay stable.
    let value = serde_json::to_value(&project).unwrap();
    assert_eq!(value["tracks"][0]["mix"]["latency_offset"], -128);
    assert!(value["tracks"][2]["mix"].get("latency_offset").is_none());
}
