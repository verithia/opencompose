//! MIDI export must keep percussion on channel 10 and melodic tracks off it.

use midly::{MidiMessage, TrackEventKind};
use oc_engine::midi::{ExportOptions, export_smf};

fn project_with(melodic: usize) -> String {
    let mut source = String::from(
        r#"@ocp "0.2.0"

@project "channels"
  tempo: 120
  time_signature: 4/4

@track drums | instrument: drums/electronic
  @pattern beat [1:1 - 2:1]
    kick 1:0 ff
    snare 2:0 mf

"#,
    );
    for index in 1..=melodic {
        source.push_str(&format!(
            "@track t{index} | instrument: synth/lead_saw\n  @phrase p [1:1 - 2:1]\n    C4 q ff\n\n"
        ));
    }
    source
}

fn note_channels(project: &str) -> Vec<(usize, u8)> {
    let parsed = oc_dsl::parse(project).expect("project parses");
    let result = export_smf(&parsed, &ExportOptions::default()).expect("export succeeds");
    let smf = midly::Smf::parse(&result.bytes).expect("bytes are a valid SMF");
    let mut channels = Vec::new();
    for (index, track) in smf.tracks.iter().enumerate() {
        for event in track {
            if let TrackEventKind::Midi { channel, message } = event.kind
                && matches!(message, MidiMessage::NoteOn { .. })
            {
                let channel = u8::from(channel) + 1;
                if !channels.contains(&(index, channel)) {
                    channels.push((index, channel));
                }
            }
        }
    }
    channels
}

#[test]
fn percussion_keeps_channel_ten_and_melodic_tracks_avoid_it() {
    let channels = note_channels(&project_with(12));
    let used: Vec<u8> = channels.iter().map(|(_, channel)| *channel).collect();
    assert!(used.contains(&10), "the drum track should be on channel 10");
    let melodic_on_ten = channels
        .iter()
        .filter(|(_, channel)| *channel == 10)
        .count();
    assert_eq!(melodic_on_ten, 1, "only the drum track may use channel 10");
}

#[test]
fn fifteen_melodic_tracks_skip_channel_ten_and_fit_in_the_port() {
    let channels = note_channels(&project_with(15));
    let melodic: Vec<u8> = channels
        .iter()
        .filter(|(_, channel)| *channel != 10)
        .map(|(_, channel)| *channel)
        .collect();
    assert_eq!(
        melodic.len(),
        15,
        "every melodic track gets its own channel"
    );
    assert!(
        !melodic.contains(&10),
        "no melodic track may be assigned channel 10"
    );
    assert!(
        melodic.iter().all(|channel| (1..=16).contains(channel)),
        "channels stay inside the 16-channel port"
    );
}
