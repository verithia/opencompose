//! One ordered view over the three clip kinds.
//!
//! Every clip lives in one typed pool on its track. This module names each one
//! with a [`ClipKey`] and lists them in document order, so a caller can walk
//! every clip-shaped object as one sequence without owning or moving any of
//! them. Pattern clip and automation lane indices count within their track;
//! an audio index counts across the whole project.

use std::fmt;
use std::str::FromStr;

use crate::ast::{AudioClip, AutomationLane, Clip, Project, TimeRange};

/// The three kinds the timeline draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipKind {
    /// A pattern placed on a track.
    Pattern,
    /// A decoded media file placed on a track.
    Audio,
    /// An automation lane, with or without a span.
    Automation,
}

impl ClipKind {
    /// Stable lowercase name used by the GUI and in [`ClipKey`] text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Audio => "audio",
            Self::Automation => "automation",
        }
    }
}

pub use crate::ast::AutomationOwner;

impl AutomationOwner {
    fn parse(text: &str) -> Option<Self> {
        if text == "master" {
            return Some(Self::Master);
        }
        if let Some(rest) = text.strip_prefix("track:") {
            return rest.parse().ok().map(Self::Track);
        }
        let name = text.strip_prefix("bus:")?;
        if name.is_empty() {
            return None;
        }
        Some(Self::Bus(name.to_string()))
    }
}

/// Stable name of one clip in a project.
///
/// The text form is the contract with the GUI:
///
/// * `track:<i>/clip:<j>` — pattern clip `j` on track `i`;
/// * `audio:<k>` — audio clip `k` in `properties.audio_clips`;
/// * `automation:<owner>/<lane>` — automation lane `lane` of `track:<i>`,
///   `bus:<name>` or `master`.
///
/// Keys are stable across edits that keep the index; a key never encodes a
/// position, so moving a clip keeps its key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClipKey {
    /// A pattern clip inside its track's `clips` list.
    TrackClip { track: usize, clip: usize },
    /// An audio clip by its index in `properties.audio_clips`.
    Audio { index: usize },
    /// An automation lane by owner and index in that owner's `automation` list.
    Automation { owner: AutomationOwner, lane: usize },
}

impl ClipKey {
    /// Key of one pattern clip.
    pub fn track_clip(track: usize, clip: usize) -> Self {
        Self::TrackClip { track, clip }
    }

    /// Key of one audio clip.
    pub fn audio(index: usize) -> Self {
        Self::Audio { index }
    }

    /// Key of one automation lane.
    pub fn automation(owner: AutomationOwner, lane: usize) -> Self {
        Self::Automation { owner, lane }
    }

    /// Kind this key names.
    pub fn kind(&self) -> ClipKind {
        match self {
            Self::TrackClip { .. } => ClipKind::Pattern,
            Self::Audio { .. } => ClipKind::Audio,
            Self::Automation { .. } => ClipKind::Automation,
        }
    }

    /// Stable text form; the inverse of [`ClipKey::parse`].
    pub fn to_text(&self) -> String {
        match self {
            Self::TrackClip { track, clip } => format!("track:{track}/clip:{clip}"),
            Self::Audio { index } => format!("audio:{index}"),
            Self::Automation { owner, lane } => format!("automation:{owner}/{lane}"),
        }
    }

    /// Parse the text form; `None` when the text names no shape.
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(rest) = text.strip_prefix("track:") {
            let (track, clip) = rest.split_once("/clip:")?;
            return Some(Self::TrackClip {
                track: track.parse().ok()?,
                clip: clip.parse().ok()?,
            });
        }
        if let Some(rest) = text.strip_prefix("audio:") {
            return Some(Self::Audio {
                index: rest.parse().ok()?,
            });
        }
        let rest = text.strip_prefix("automation:")?;
        let (owner, lane) = rest.rsplit_once('/')?;
        Some(Self::Automation {
            owner: AutomationOwner::parse(owner)?,
            lane: lane.parse().ok()?,
        })
    }
}

impl fmt::Display for ClipKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

impl FromStr for ClipKey {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text).ok_or(())
    }
}

/// Borrowed payload behind a [`ClipRef`].
#[derive(Debug, Clone, Copy)]
pub enum ClipPayload<'a> {
    Pattern(&'a Clip),
    Audio(&'a AudioClip),
    Automation(&'a AutomationLane),
}

/// One clip-shaped object, borrowed from the project that owns it.
#[derive(Debug, Clone)]
pub struct ClipRef<'a> {
    /// Stable name, see [`ClipKey`].
    pub key: ClipKey,
    /// Kind the payload carries.
    pub kind: ClipKind,
    /// Span the timeline draws; `None` only for a whole-timeline automation lane.
    pub time_range: Option<TimeRange>,
    /// The stored object itself.
    pub payload: ClipPayload<'a>,
}

impl<'a> ClipRef<'a> {
    fn new(key: ClipKey, payload: ClipPayload<'a>) -> Self {
        let (kind, time_range) = match payload {
            ClipPayload::Pattern(clip) => (ClipKind::Pattern, Some(clip.time_range)),
            ClipPayload::Audio(clip) => (ClipKind::Audio, Some(clip.time_range)),
            ClipPayload::Automation(lane) => (ClipKind::Automation, lane.time_range),
        };
        Self {
            key,
            kind,
            time_range,
            payload,
        }
    }

    /// Human label: the pattern name, the audio clip name or the automation
    /// target (`<owner>/<parameter>`).
    pub fn label(&self) -> String {
        match self.payload {
            ClipPayload::Pattern(clip) => clip.pattern.clone(),
            ClipPayload::Audio(clip) => clip.name.clone(),
            ClipPayload::Automation(lane) => match &self.key {
                ClipKey::Automation { owner, .. } => format!("{owner}/{}", lane.param),
                _ => lane.param.clone(),
            },
        }
    }
}

impl Project {
    /// Every clip-shaped object in document order.
    ///
    /// Order is: for each track in order its pattern clips; then every audio
    /// clip in `properties.audio_clips` order; then every automation lane in
    /// track order, then bus order, then master. A whole-timeline automation
    /// lane is listed with `time_range: None`.
    pub fn clips(&self) -> Vec<ClipRef<'_>> {
        let mut clips = Vec::with_capacity(self.clip_count());
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (clip_index, clip) in track.pattern_clips().enumerate() {
                clips.push(ClipRef::new(
                    ClipKey::track_clip(track_index, clip_index),
                    ClipPayload::Pattern(clip),
                ));
            }
        }
        for (index, clip) in self.audio_clips().into_iter().enumerate() {
            clips.push(ClipRef::new(
                ClipKey::audio(index),
                ClipPayload::Audio(clip),
            ));
        }
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (lane_index, lane) in track.automation().enumerate() {
                clips.push(ClipRef::new(
                    ClipKey::automation(AutomationOwner::Track(track_index), lane_index),
                    ClipPayload::Automation(lane),
                ));
            }
        }
        for bus in &self.buses {
            for (lane_index, lane) in bus.automation.iter().enumerate() {
                clips.push(ClipRef::new(
                    ClipKey::automation(AutomationOwner::Bus(bus.name.clone()), lane_index),
                    ClipPayload::Automation(lane),
                ));
            }
        }
        if let Some(master) = &self.master {
            for (lane_index, lane) in master.automation.iter().enumerate() {
                clips.push(ClipRef::new(
                    ClipKey::automation(AutomationOwner::Master, lane_index),
                    ClipPayload::Automation(lane),
                ));
            }
        }
        clips
    }

    /// Look up one clip by key; a key that resolves to nothing returns `None`.
    pub fn clip(&self, key: &ClipKey) -> Option<ClipRef<'_>> {
        match key {
            ClipKey::TrackClip { track, clip } => {
                let clip = self.tracks.get(*track)?.pattern_clip(*clip)?;
                Some(ClipRef::new(key.clone(), ClipPayload::Pattern(clip)))
            }
            ClipKey::Audio { index } => {
                let clip = self.audio_clip(*index)?;
                Some(ClipRef::new(key.clone(), ClipPayload::Audio(clip)))
            }
            ClipKey::Automation { owner, lane } => {
                if let AutomationOwner::Track(index) = owner {
                    let lane = self.tracks.get(*index)?.automation_lane(*lane)?;
                    return Some(ClipRef::new(key.clone(), ClipPayload::Automation(lane)));
                }
                let lanes = match owner {
                    AutomationOwner::Track(_) => unreachable!("handled above"),
                    AutomationOwner::Bus(name) => {
                        &self.buses.iter().find(|bus| &bus.name == name)?.automation
                    }
                    AutomationOwner::Master => &self.master.as_ref()?.automation,
                };
                let lane = lanes.get(*lane)?;
                Some(ClipRef::new(key.clone(), ClipPayload::Automation(lane)))
            }
        }
    }

    /// Number of clips [`Project::clips`] lists.
    pub fn clip_count(&self) -> usize {
        let pattern_clips = self
            .tracks
            .iter()
            .map(|track| track.pattern_clip_count())
            .sum::<usize>();
        let audio_clips = self.audio_clip_count();
        let track_lanes = self
            .tracks
            .iter()
            .map(|track| track.automation_count())
            .sum::<usize>();
        let bus_lanes = self
            .buses
            .iter()
            .map(|bus| bus.automation.len())
            .sum::<usize>();
        let master_lanes = self
            .master
            .as_ref()
            .map(|master| master.automation.len())
            .unwrap_or(0);
        pattern_clips + audio_clips + track_lanes + bus_lanes + master_lanes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    const FIXTURE: &str = r#"@project "Pool"
  tempo: 120
  time_signature: 4/4

@pattern hook | instrument: synth/lead_saw
  events:
    C4 q mf

@track lead | instrument: synth/lead_saw
  clip: hook [1:1 - 3:1]
  automation: volume [1:1 - 3:1] {1:1=0%, 3:1=100%}
  automation: pan {1:1=50%, 5:1=50%}

@audio_clip take [1:1 - 3:1] | track: lead
  file: "media/take.wav"

@bus drums
  automation: fx1.mix {1:1=0%, 3:1=100%}

@master
  automation: fx2.mix {1:1=0%, 3:1=100%}
"#;

    fn project() -> Project {
        parse(FIXTURE).expect("the fixture parses")
    }

    #[test]
    fn one_of_each_kind_lists_in_document_order() {
        let project = project();
        let keys: Vec<String> = project
            .clips()
            .iter()
            .map(|clip| clip.key.to_text())
            .collect();
        assert_eq!(
            keys,
            vec![
                "track:0/clip:0",
                "audio:0",
                "automation:track:0/0",
                "automation:track:0/1",
                "automation:bus:drums/0",
                "automation:master/0",
            ]
        );
        assert_eq!(project.clip_count(), keys.len());
    }

    #[test]
    fn labels_name_the_pattern_the_media_or_the_automation_target() {
        let project = project();
        let labels: Vec<String> = project.clips().iter().map(ClipRef::label).collect();
        assert_eq!(
            labels,
            vec![
                "hook",
                "take",
                "track:0/volume",
                "track:0/pan",
                "bus:drums/fx1.mix",
                "master/fx2.mix",
            ]
        );
    }

    #[test]
    fn a_key_round_trips_through_its_text_form() {
        for clip in project().clips() {
            let text = clip.key.to_text();
            let parsed = ClipKey::parse(&text).expect("the text parses");
            assert_eq!(parsed, clip.key, "{text}");
            assert_eq!(parsed.to_text(), text);
            assert_eq!(parsed.kind(), clip.kind);
        }
        assert!(ClipKey::parse("track:0").is_none());
        assert!(ClipKey::parse("audio:x").is_none());
        assert!(ClipKey::parse("automation:track:0").is_none());
        assert!(ClipKey::parse("automation:track:0/1").is_some());
    }

    #[test]
    fn a_whole_timeline_lane_is_listed_without_a_span() {
        let project = project();
        let lane = project
            .clip(&ClipKey::automation(AutomationOwner::Track(0), 1))
            .expect("the second lane resolves");
        assert_eq!(lane.kind, ClipKind::Automation);
        assert_eq!(lane.time_range, None);
        assert_eq!(lane.label(), "track:0/pan");
        // The lane before it carries the authored span.
        let clip = project
            .clip(&ClipKey::automation(AutomationOwner::Track(0), 0))
            .expect("the first lane resolves");
        assert!(clip.time_range.is_some());
    }

    #[test]
    fn a_lookup_reports_the_stored_object_or_nothing() {
        let project = project();
        let pattern = project
            .clip(&ClipKey::track_clip(0, 0))
            .expect("the clip resolves");
        assert_eq!(pattern.kind, ClipKind::Pattern);
        assert!(matches!(pattern.payload, ClipPayload::Pattern(_)));
        assert!(project.clip(&ClipKey::track_clip(0, 9)).is_none());
        assert!(project.clip(&ClipKey::audio(9)).is_none());
        assert!(
            project
                .clip(&ClipKey::automation(
                    AutomationOwner::Bus("missing".into()),
                    0
                ))
                .is_none()
        );
    }

    #[test]
    fn an_empty_project_lists_nothing() {
        let project = parse("@project \"Empty\"\n  tempo: 120\n").expect("it parses");
        assert!(project.clips().is_empty());
        assert_eq!(project.clip_count(), 0);
    }
}
