//! MPE expression must survive offline and streaming dispatch identically.

use super::*;
use crate::block_source::BlockSource;
use oc_synth::synth::PLUGIN_ID;

const PROJECT: &str = r#"@project
  tempo: 120
  mpe: {lower: 2-16, bend_range: 48}
@preset mpe_lead
  source: builtin
  plugin: "builtin.open_synth"
  params: { mod.1.source: 27, mod.1.destination: 1, mod.1.amount: 0.25, filter.wet: 0 }
@track lead | instrument: synth/lead_square, preset: mpe_lead
  @phrase notes [1:1 - 1:3]
    C4 q mf bend=100ct expression={pressure: 20%->90%, timbre: 10%->80%}
    @1 D4 q mf bend=50ct expression={pressure: 80%->30%, timbre: 90%->20%}
"#;

fn stream(
    source: &mut BlockSource,
    events: &[MidiEvent],
    total_samples: usize,
    sample_rate: u32,
    tempo: f64,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(total_samples * 2);
    let mut cursor = 0;
    let mut pos = 0;
    while pos < total_samples {
        while cursor < events.len() && events[cursor].time_samples as usize <= pos {
            source.send(&events[cursor].event).unwrap();
            cursor += 1;
        }
        let next = events
            .get(cursor)
            .map(|event| event.time_samples as usize)
            .unwrap_or(total_samples);
        let frames = 64.min(total_samples - pos).min((next - pos).max(1));
        let transport = oc_host::PluginTransport::new(pos as u64, sample_rate, tempo);
        out.extend(source.render(frames, sample_rate, transport));
        pos += frames;
    }
    out
}

#[test]
fn project_expression_matches_between_offline_and_streaming() {
    let sample_rate = 48_000;
    let project = oc_dsl::parse(PROJECT).unwrap_or_else(|error| panic!("{error:?}"));
    let track = &project.tracks[0];
    let preset = project
        .presets
        .iter()
        .find(|p| p.name == "mpe_lead")
        .unwrap();
    let params = crate::preset::builtin_track_params(&project, Some(preset), track);
    assert!(params.iter().any(|(name, value)| {
        name == "mpe.bend_range"
            && matches!(value, oc_dsl::ast::ParamValue::Number(range) if (*range - 48.0).abs() < 1e-9)
    }));
    let clock = oc_dsl::timing::ProjectClock::new(&project.properties).unwrap();
    let events = interpret::try_interpret(
        &project,
        &InterpretSettings {
            sample_rate,
            ..Default::default()
        },
    )
    .unwrap();
    let total =
        events.iter().map(|event| event.time_samples).max().unwrap() + sample_rate as u64 * 2;
    let total = total as usize;
    let refs: Vec<&MidiEvent> = events.iter().collect();

    let offline = render_builtin_project(
        PLUGIN_ID,
        Some(&params),
        &refs,
        total,
        0,
        sample_rate,
        project.properties.tempo,
        None,
        Some(&clock),
        None,
    )
    .unwrap();

    let mut source = BlockSource::new(
        Some(preset),
        &params,
        std::path::Path::new(""),
        sample_rate,
        project.properties.tempo,
        None,
    )
    .unwrap();
    let streaming = stream(
        &mut source,
        &events,
        total,
        sample_rate,
        project.properties.tempo,
    );

    assert_eq!(offline.len(), streaming.len());
    assert!(offline.iter().any(|sample| sample.abs() > 1e-4));
    let mut max_delta = 0.0f32;
    for (left, right) in offline.iter().zip(&streaming) {
        max_delta = max_delta.max((left - right).abs());
    }
    assert_eq!(
        max_delta, 0.0,
        "offline and streaming differ by {max_delta}"
    );
}
