//! Import aligned WAV stems and optional MIDI into one document.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use oc_dsl::ast::{BarBeat, Project, TimeRange};
use oc_dsl::timing::ProjectClock;
use oc_engine::midi::{ImportSummary, import_smf};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::state::{AppState, AppStateInner, UndoEdit, UndoScope};

use super::bounce::covering_position;
use super::sample_market::{plain_audio_clip, sample_track, unique_clip_name};
use super::shared::{ProjectSnapshot, resync_with_undo, snapshot, validation_text};

const MAX_STEMS: usize = 64;
const MAX_MIDI_BYTES: u64 = 8 * 1024 * 1024;
const MAX_WAV_BYTES: u64 = 2 * 1024 * 1024 * 1024;
static IMPORT_SERIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
/// Result of importing aligned stems.
pub struct HandoffOutcome {
    snapshot: ProjectSnapshot,
    #[serde(flatten)]
    summary: HandoffSummary,
}

#[derive(Serialize)]
pub(crate) struct HandoffSummary {
    revision: u64,
    stem_count: usize,
    timestamped_stem_count: usize,
    midi_track_count: usize,
    midi: Option<ImportSummary>,
}

pub(crate) struct HandoffVersion {
    path: PathBuf,
    revision: u64,
    epoch: u64,
    transaction_id: Option<u64>,
}

struct Destination {
    version: HandoffVersion,
    project: Project,
    staged_only: bool,
}

fn transaction_id(inner: &AppStateInner, caller: Option<&str>) -> Result<Option<u64>, String> {
    let Some(transaction) = &inner.transaction else {
        return Ok(None);
    };
    if caller == Some(transaction.owner.as_str()) {
        return Ok(Some(transaction.id));
    }
    Err("The active project transaction must finish before this handoff.".into())
}

impl HandoffVersion {
    pub(crate) fn capture(
        inner: &AppStateInner,
        expected_revision: u64,
        caller: Option<&str>,
    ) -> Result<Self, String> {
        inner.refuse_in_preview()?;
        let transaction_id = transaction_id(inner, caller)?;
        inner.check_revision(Some(expected_revision))?;
        let path = inner
            .file_path
            .clone()
            .ok_or("Save the project before using its handoff folder.")?;
        Ok(Self {
            path,
            revision: inner.revision,
            epoch: inner.content_epoch,
            transaction_id,
        })
    }

    pub(crate) fn project_root(&self) -> Result<PathBuf, String> {
        self.path
            .parent()
            .ok_or("The project has no media directory.")?
            .canonicalize()
            .map_err(|error| format!("Cannot open the project directory: {error}"))
    }

    pub(crate) fn verify(&self, inner: &AppStateInner, caller: Option<&str>) -> Result<(), String> {
        inner.refuse_in_preview()?;
        let active_transaction = transaction_id(inner, caller)?;
        inner.check_revision(Some(self.revision))?;
        if inner.file_path.as_ref() != Some(&self.path)
            || inner.content_epoch != self.epoch
            || active_transaction != self.transaction_id
        {
            return Err("The destination project changed during the handoff.".into());
        }
        Ok(())
    }
}

impl Destination {
    fn capture(
        inner: &AppStateInner,
        expected_revision: u64,
        caller: Option<&str>,
    ) -> Result<Self, String> {
        let version = HandoffVersion::capture(inner, expected_revision, caller)?;
        if !inner.project.tracks.is_empty() || !inner.project.sections.is_empty() {
            return Err(
                "Import aligned stems into a saved project with no tracks or sections.".into(),
            );
        }
        Ok(Self {
            version,
            project: inner.project.clone(),
            staged_only: caller.is_some(),
        })
    }
}

/// Prepared copies are removed on refusal and on transaction rollback.
pub(crate) struct HandoffMedia {
    directory: PathBuf,
    keep: bool,
}

impl HandoffMedia {
    pub(crate) fn retain(&mut self) {
        self.keep = true;
    }

    fn create(root: &Path) -> Result<Self, String> {
        let media = root.join("media");
        fs::create_dir_all(&media)
            .map_err(|error| format!("Cannot create the project media directory: {error}"))?;
        if media
            .canonicalize()
            .map_err(|error| error.to_string())?
            .parent()
            != Some(root)
        {
            return Err("The project media directory resolves outside the project folder.".into());
        }
        loop {
            let serial = IMPORT_SERIAL.fetch_add(1, Ordering::Relaxed);
            let name = format!("handoff-{}-{serial}", std::process::id());
            let directory = media.join(name);
            match fs::create_dir(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        directory,
                        keep: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("Cannot stage the project media: {error}")),
            }
        }
    }
}

impl Drop for HandoffMedia {
    fn drop(&mut self) {
        if !self.keep
            && let Err(error) = fs::remove_dir_all(&self.directory)
        {
            log::warn!("Could not remove unfinished import media: {error}");
        }
    }
}

struct Prepared {
    version: HandoffVersion,
    project: Project,
    media: HandoffMedia,
    stem_count: usize,
    timestamped_stem_count: usize,
    midi_track_count: usize,
    midi: Option<ImportSummary>,
}

/// Create the one bounded exchange directory used by agent handoffs.
pub(crate) fn ensure_handoff_folder(root: &Path) -> Result<PathBuf, String> {
    let folder = root.join("handoff");
    match fs::create_dir(&folder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("Cannot create the project handoff folder: {error}")),
    }
    if !folder.is_dir() || folder.canonicalize().ok().as_deref() != Some(folder.as_path()) {
        return Err("The project handoff folder must be a directory inside the project.".into());
    }
    Ok(folder)
}

/// An agent can only read explicitly named files from the project's handoff
/// folder. Check both the input components and the resolved location.
pub(crate) fn staged_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let folder = root.join("handoff");
    let refusal = "Use project-relative paths inside the saved project's handoff/ folder.";
    if !matches!(path.components().next(), Some(Component::Normal(name)) if name == "handoff")
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(refusal.into());
    }
    if folder.canonicalize().ok().as_deref() != Some(folder.as_path()) {
        return Err(refusal.into());
    }
    let canonical = root
        .join(path)
        .canonicalize()
        .map_err(|_| format!("Cannot open staged handoff file {}.", path.display()))?;
    if !canonical.starts_with(&folder) {
        return Err(refusal.into());
    }
    Ok(canonical)
}

fn prepare(destination: Destination, paths: Vec<PathBuf>) -> Result<Prepared, String> {
    if paths.is_empty() || paths.len() > MAX_STEMS + 1 {
        return Err("Select up to 64 WAV stems and one optional MIDI file.".into());
    }
    let Destination {
        version,
        mut project,
        staged_only,
    } = destination;
    let root = version.project_root()?;
    let mut wav = Vec::new();
    let mut midi = None;
    let mut seen = HashSet::new();
    for path in paths {
        let path = if staged_only {
            staged_path(&root, &path)?
        } else {
            if !path.is_absolute() {
                return Err("Select files with the file picker.".into());
            }
            path.canonicalize()
                .map_err(|error| format!("Cannot open {}: {error}", path.display()))?
        };
        let metadata = path.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || !seen.insert(path.clone()) {
            return Err("Select each regular media file only once.".into());
        }
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("wav") => wav.push((path, metadata.len())),
            Some("mid" | "midi") if midi.is_none() => midi = Some(path),
            _ => return Err("Select WAV stems and at most one MIDI file.".into()),
        }
    }
    if wav.is_empty() || wav.len() > MAX_STEMS {
        return Err("Select between 1 and 64 WAV stems.".into());
    }
    if wav.iter().map(|(_, size)| u128::from(*size)).sum::<u128>() > u128::from(MAX_WAV_BYTES) {
        return Err("The selected WAV stems exceed the 2 GiB import limit.".into());
    }

    let midi = midi
        .as_deref()
        .map(|path| import_midi(path, &mut project))
        .transpose()?;
    let midi_track_count = project.tracks.len();
    let clock = ProjectClock::new(&project.properties)?;
    let media = HandoffMedia::create(&root)?;
    let stem_count = wav.len();
    let mut origins: Vec<(PathBuf, BroadcastOrigin)> = Vec::new();
    for (index, (source, bytes)) in wav.into_iter().enumerate() {
        let target = media.directory.join(format!("stem-{:02}.wav", index + 1));
        copy_wav(&source, &target, bytes)?;
        let (seconds, stereo, origin) =
            wav_details(&target).map_err(|error| format!("{}: {error}", source.display()))?;
        if let Some(origin) = origin {
            if let Some((other, _)) = origins
                .iter()
                .find(|(_, earlier)| !origin.matches(*earlier))
            {
                return Err(format!(
                    "The Broadcast WAV start timestamps differ for {} and {}. Re-export both from the same origin with leading silence.",
                    other.display(),
                    source.display()
                ));
            }
            origins.push((source.clone(), origin));
        }
        let end = covering_position(&clock, clock.quarter_at_seconds(seconds)?)?;
        let name = source
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Stem");
        let name = track_name(&project, name);
        let clip_name = unique_clip_name(&project, &name);
        let relative = target
            .strip_prefix(&root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let mut track = sample_track(&name);
        track.mix.stereo_balance = stereo;
        project.tracks.push(track);
        project.add_audio_clip(plain_audio_clip(
            clip_name,
            name,
            relative,
            TimeRange {
                start: BarBeat::new(1, 1, 0),
                end,
            },
        ))?;
    }
    oc_dsl::validate(&project).map_err(|errors| validation_text(&errors))?;
    Ok(Prepared {
        version,
        project,
        media,
        stem_count,
        timestamped_stem_count: origins.len(),
        midi_track_count,
        midi,
    })
}

fn track_name(project: &Project, filename: &str) -> String {
    let base: String = filename
        .chars()
        .filter(|c| !c.is_control())
        .take(90)
        .collect();
    let base = if base.trim().is_empty() {
        "Stem"
    } else {
        base.trim()
    };
    let mut name = base.to_owned();
    for suffix in 2.. {
        if !project.tracks.iter().any(|track| track.name == name) {
            return name;
        }
        name = format!("{base} {suffix}");
    }
    unreachable!("a finite set of tracks leaves a free name")
}

pub(crate) fn read_handoff_midi(path: &Path) -> Result<Vec<u8>, String> {
    if !path.is_file() {
        return Err("Select a staged MIDI file.".into());
    }
    let mut data = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_MIDI_BYTES + 1).read_to_end(&mut data))
        .map_err(|error| format!("Cannot read the MIDI file: {error}"))?;
    if data.len() as u64 > MAX_MIDI_BYTES {
        return Err("The MIDI file exceeds the 8 MiB import limit.".into());
    }
    Ok(data)
}

fn import_midi(path: &Path, project: &mut Project) -> Result<ImportSummary, String> {
    let data = read_handoff_midi(path)?;
    let options = crate::midi_bridge::handoff_import_options(true);
    let result = import_smf(&data, None, &options, Some(project))
        .map_err(|error| format!("Cannot import MIDI: {error}"))?;
    if result.summary.format == 2 {
        return Err(
            "MIDI format 2 contains independent sequences and has no shared origin.".into(),
        );
    }
    let mut imported = result.project;
    crate::midi_bridge::preserve_non_midi_properties(&mut imported, project);
    for track in &mut imported.tracks {
        track.mix.mute = true;
    }
    *project = imported;
    Ok(result.summary)
}

fn copy_wav(source: &Path, target: &Path, expected_bytes: u64) -> Result<(), String> {
    let mut input =
        File::open(source).map_err(|error| format!("Cannot read {}: {error}", source.display()))?;
    let mut output = File::create(target)
        .map_err(|error| format!("Cannot copy {}: {error}", source.display()))?;
    let copied = std::io::copy(&mut (&mut input).take(expected_bytes + 1), &mut output)
        .map_err(|error| format!("Cannot copy {}: {error}", source.display()))?;
    if copied != expected_bytes {
        return Err(format!(
            "{} changed while it was being imported.",
            source.display()
        ));
    }
    Ok(())
}

fn wav_details(path: &Path) -> Result<(f64, bool, Option<BroadcastOrigin>), String> {
    let reader = oc_engine::wav::open_reader(path).map_err(|error| error.to_string())?;
    let spec = reader.spec();
    if spec.sample_rate == 0 || !matches!(spec.channels, 1 | 2) || reader.duration() == 0 {
        return Err("Use a nonempty mono or stereo WAV.".into());
    }
    let seconds = f64::from(reader.duration()) / f64::from(spec.sample_rate);
    if seconds > 30.0 * 60.0 {
        return Err("The WAV exceeds the 30-minute source limit.".into());
    }
    // Check the last declared frame so a truncated copy cannot create a clip
    // whose media ends before its declared source length.
    let mut reader = reader;
    reader
        .seek(reader.duration() - 1)
        .map_err(|error| error.to_string())?;
    let valid_tail = match spec.sample_format {
        hound::SampleFormat::Int => reader.samples::<i32>().all(|sample| sample.is_ok()),
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .all(|sample| sample.is_ok_and(f32::is_finite)),
    };
    if !valid_tail {
        return Err("The WAV ends with incomplete or invalid sample data.".into());
    }
    let origin = oc_engine::wav::broadcast_time_reference(path)
        .map_err(|error| format!("Cannot read Broadcast WAV timing: {error}"))?
        .map(|samples| BroadcastOrigin {
            samples,
            rate: spec.sample_rate,
        });
    Ok((seconds, spec.channels == 2, origin))
}

#[derive(Clone, Copy)]
struct BroadcastOrigin {
    samples: u64,
    rate: u32,
}

impl BroadcastOrigin {
    fn matches(self, other: Self) -> bool {
        if self.rate == other.rate {
            return self.samples == other.samples;
        }
        // Independent rounding to each source rate can differ by up to half
        // a sample at each rate. Compare exactly, including long timecodes.
        let left = u128::from(self.samples) * u128::from(other.rate);
        let right = u128::from(other.samples) * u128::from(self.rate);
        2 * left.abs_diff(right) <= u128::from(self.rate) + u128::from(other.rate)
    }
}

fn commit(
    inner: &mut AppStateInner,
    mut prepared: Prepared,
    caller: Option<&str>,
) -> Result<HandoffSummary, String> {
    prepared.version.verify(inner, caller)?;
    let old = std::mem::replace(&mut inner.project, prepared.project);
    resync_with_undo(
        inner,
        old,
        UndoEdit::new("Import aligned stems", UndoScope::Project),
    )?;
    if let Some(transaction) = &mut inner.transaction {
        transaction.handoff_media.push(prepared.media);
    } else {
        prepared.media.retain();
    }
    Ok(HandoffSummary {
        revision: inner.revision,
        stem_count: prepared.stem_count,
        timestamped_stem_count: prepared.timestamped_stem_count,
        midi_track_count: prepared.midi_track_count,
        midi: prepared.midi,
    })
}

/// Import aligned stems into the project.
#[tauri::command]
pub async fn import_aligned_stems(
    paths: Vec<PathBuf>,
    expected_revision: u64,
    app: AppHandle,
) -> Result<HandoffOutcome, String> {
    let state = app.state::<AppState>();
    let destination = Destination::capture(&state.lock_inner(), expected_revision, None)?;
    let prepared = super::shared::run_blocking(move || prepare(destination, paths)).await?;
    let mut inner = state.lock_inner();
    let summary = commit(&mut inner, prepared, None)?;
    Ok(HandoffOutcome {
        snapshot: snapshot(&inner),
        summary,
    })
}

/// The agent dispatcher already runs on a worker. Copy and validate without
/// holding the state lock, then publish against the captured document version.
pub(crate) fn import_aligned_stems_for_agent(
    state: &AppState,
    paths: Vec<PathBuf>,
    expected_revision: u64,
    caller: &str,
) -> Result<HandoffSummary, String> {
    let caller = Some(caller);
    let destination = Destination::capture(&state.lock_inner(), expected_revision, caller)?;
    let prepared = prepare(destination, paths)?;
    commit(&mut state.lock_inner(), prepared, caller)
}

#[cfg(test)]
mod tests {
    use super::*;
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };

    fn state_fixture() -> (PathBuf, AppState) {
        let root = oc_audio::private_fs::test_root().join(format!(
            "stem-handoff-{}",
            IMPORT_SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let state = AppState::new();
        {
            let mut inner = state.lock_inner();
            inner.project = super::super::project::new_project_document("Handoff".into());
            let path = root.join("destination.ocp");
            inner.dsl_source = oc_dsl::serialize(&inner.project);
            fs::write(&path, &inner.dsl_source).unwrap();
            inner.file_path = Some(path);
        }
        (root, state)
    }

    fn destination_fixture() -> (PathBuf, AppStateInner) {
        let (root, state) = state_fixture();
        (root, state.inner.into_inner().unwrap())
    }

    fn write_stem(path: &Path, frames: usize, sign: f32) {
        let mut samples = vec![0.0; frames];
        samples[2_000..].fill(sign * 0.25);
        fs::write(
            path,
            oc_engine::wav::mono_wav_bytes(&samples, 8_000).unwrap(),
        )
        .unwrap();
    }

    fn write_midi(path: &Path) {
        let meta = |delta, message| TrackEvent {
            delta: midly::num::u28::new(delta),
            kind: TrackEventKind::Meta(message),
        };
        let note = |delta, message| TrackEvent {
            delta: midly::num::u28::new(delta),
            kind: TrackEventKind::Midi {
                channel: 0.into(),
                message,
            },
        };
        let second_channel_pedal = |value| TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Midi {
                channel: 1.into(),
                message: MidiMessage::Controller {
                    controller: 64.into(),
                    value: midly::num::u7::new(value),
                },
            },
        };
        let midi = Smf {
            header: Header::new(Format::SingleTrack, Timing::Metrical(480.into())),
            tracks: vec![vec![
                meta(0, MetaMessage::Tempo(500_000.into())),
                meta(0, MetaMessage::TimeSignature(3, 2, 24, 8)),
                note(
                    240,
                    MidiMessage::NoteOn {
                        key: 60.into(),
                        vel: 90.into(),
                    },
                ),
                note(
                    0,
                    MidiMessage::Controller {
                        controller: 64.into(),
                        value: 127.into(),
                    },
                ),
                second_channel_pedal(0),
                note(
                    480,
                    MidiMessage::NoteOff {
                        key: 60.into(),
                        vel: 0.into(),
                    },
                ),
                note(
                    240,
                    MidiMessage::Controller {
                        controller: 64.into(),
                        value: 0.into(),
                    },
                ),
                second_channel_pedal(127),
                meta(480, MetaMessage::Tempo(750_000.into())),
                meta(0, MetaMessage::TimeSignature(4, 2, 24, 8)),
                meta(0, MetaMessage::EndOfTrack),
            ]],
        };
        midi.save(path).unwrap();
    }

    fn write_broadcast_stem(path: &Path, rate: u32, time_reference: u64, after_data: bool) {
        let wav = oc_engine::wav::mono_wav_bytes(&vec![0.2; rate as usize], rate).unwrap();
        let mut bytes = wav[..12].to_vec();
        if after_data {
            bytes.extend_from_slice(&wav[12..]);
        }
        bytes.extend_from_slice(b"JUNK");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]); // odd payload and RIFF padding
        bytes.extend_from_slice(b"bext");
        bytes.extend_from_slice(&602_u32.to_le_bytes());
        let mut bext = [0; 602];
        bext[338..346].copy_from_slice(&time_reference.to_le_bytes());
        bytes.extend_from_slice(&bext);
        if !after_data {
            bytes.extend_from_slice(&wav[12..]);
        }
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn aligned_stems_checks_broadcast_origins_and_cleans_a_refusal() {
        let (root, inner) = destination_fixture();
        let drums = root.join("Drums.wav");
        let vocal = root.join("Vocal.wav");
        let plain = root.join("Plain.wav");
        // Beyond the low 32 bits at 96 kHz. The second rate rounds to its grid.
        let reference = 48_000_u64 * 96_000 + 1;
        write_broadcast_stem(&drums, 96_000, reference, false);
        write_broadcast_stem(&vocal, 48_000, 48_000 * 48_000 + 1, true);
        write_stem(&plain, 4_000, 1.0);
        let capture = || Destination::capture(&inner, inner.revision, None).unwrap();
        let prepared = prepare(capture(), vec![drums.clone(), vocal.clone(), plain]).unwrap();
        assert_eq!(prepared.timestamped_stem_count, 2);
        assert_eq!(prepared.stem_count, 3);
        drop(prepared);

        write_broadcast_stem(&vocal, 96_000, reference + 1, true);
        let error = prepare(capture(), vec![drums, vocal]).err().unwrap();
        assert!(error.contains("start timestamps differ"), "{error}");
        assert!(error.contains("Drums.wav") && error.contains("Vocal.wav"));
        assert_eq!(fs::read_dir(root.join("media")).unwrap().count(), 0);
        assert!(inner.project.tracks.is_empty());
    }

    #[test]
    fn aligned_stems_import_keeps_origin_midi_clock_and_portable_media_in_one_edit() {
        let (root, mut inner) = destination_fixture();
        let drums = root.join("Drums.wav");
        let vocal = root.join("Vocal.wav");
        let midi = root.join("Notes.mid");
        write_stem(&drums, 17_600, 1.0);
        write_stem(&vocal, 24_800, -1.0);
        write_midi(&midi);
        let drum_bytes = fs::read(&drums).unwrap();
        let vocal_bytes = fs::read(&vocal).unwrap();
        let before = inner.project.clone();

        let destination = Destination::capture(&inner, inner.revision, None).unwrap();
        let prepared = prepare(destination, vec![drums.clone(), midi, vocal.clone()]).unwrap();
        let outcome = commit(&mut inner, prepared, None).unwrap();
        assert_eq!((outcome.stem_count, outcome.midi_track_count), (2, 2));
        assert_eq!(inner.undo.undo.len(), 1);
        assert_eq!(inner.undo.undo[0].label, "Import aligned stems");
        assert!(inner.project.tracks[0].mix.mute);
        assert!(inner.project.tracks[1].mix.mute);
        assert!(!inner.project.tracks[2].mix.mute);
        assert!(!inner.project.tracks[2].mix.stereo_balance);
        let clock = ProjectClock::new(&inner.project.properties).unwrap();
        assert_eq!(inner.project.properties.tempo, 120.0);
        assert_eq!(clock.meter_at_bar(1).unwrap().numerator, 3);
        assert_eq!(clock.meter_at_bar(2).unwrap().numerator, 4);
        assert_eq!(inner.project.properties.tempo_map.len(), 1);
        assert_eq!(inner.project.properties.tempo_map[0].bpm, 80.0);
        assert_eq!(outcome.midi.unwrap().sustain_expanded_notes, 0);
        let midi_track = &inner.project.tracks[0];
        let hit = &midi_track.phrases[0].events[0];
        assert_eq!(hit.duration.to_beats(), 1.0);
        let pedal_values = |track: &oc_dsl::ast::Track| {
            track
                .automation()
                .find(|lane| lane.param == "sustain")
                .unwrap()
                .points
                .iter()
                .map(|point| point.value.clone())
                .collect::<Vec<_>>()
        };
        use oc_dsl::ast::ParamValue::Percentage;
        assert_eq!(midi_track.channel, Some(0));
        assert_eq!(
            pedal_values(midi_track),
            [Percentage(100.0), Percentage(0.0)]
        );
        let controller_only = &inner.project.tracks[1];
        assert_eq!(controller_only.channel, Some(1));
        assert!(controller_only.phrases.is_empty());
        assert_eq!(
            pedal_values(controller_only),
            [Percentage(0.0), Percentage(100.0)]
        );

        fs::remove_file(drums).unwrap();
        fs::remove_file(vocal).unwrap();
        let clips = inner.project.audio_clips();
        for (clip, expected, frames, sign) in [
            (clips[0], drum_bytes, 17_600, 1.0),
            (clips[1], vocal_bytes, 24_800, -1.0),
        ] {
            assert_eq!(clip.time_range.start, BarBeat::new(1, 1, 0));
            assert_eq!(clip.offset_seconds, 0.0);
            assert!(!clip.tempo_follow);
            assert!(clip.file.starts_with("media/handoff-"));
            assert_eq!(fs::read(root.join(&clip.file)).unwrap(), expected);
            let (start, length) =
                oc_engine::audio_clip::range_frames(&inner.project, clip, 8_000).unwrap();
            assert_eq!(start, 0);
            assert!(length >= frames && length - frames <= 1_500);
            let mut rendered = vec![0.0; length as usize * 2];
            oc_engine::audio_clip::mix_into_track(
                &mut rendered,
                &inner.project,
                clip,
                8_000,
                Some(&root),
            )
            .unwrap();
            assert!(rendered[..4_000].iter().all(|sample| *sample == 0.0));
            assert!(
                rendered[4_000..4_002]
                    .iter()
                    .all(|sample| sign * sample > 0.2)
            );
        }
        assert!(oc_dsl::parse(&inner.dsl_source).is_ok());
        let after = inner.project.clone();
        inner.undo.undo(&mut inner.project).unwrap();
        assert_eq!(inner.project, before);
        inner.undo.redo(&mut inner.project).unwrap();
        assert_eq!(inner.project, after);
        assert!(
            inner
                .project
                .audio_clips()
                .iter()
                .all(|clip| root.join(&clip.file).is_file())
        );
    }

    #[test]
    fn aligned_stems_agent_transaction_keeps_only_committed_media_for_undo_redo() {
        let (root, state) = state_fixture();
        let staged = root.join("handoff");
        fs::create_dir(&staged).unwrap();
        write_stem(&staged.join("Drums.wav"), 8_000, 1.0);
        write_stem(&staged.join("Vocal.wav"), 10_000, -1.0);
        write_midi(&staged.join("Notes.mid"));
        let paths = vec![
            "handoff/Drums.wav".into(),
            "handoff/Vocal.wav".into(),
            "handoff/Notes.mid".into(),
        ];
        let owner = "panel:stem-test";
        let revision = state
            .begin_transaction("discard stems".into(), owner.into())
            .unwrap();
        assert!(import_aligned_stems_for_agent(&state, paths.clone(), revision, "http").is_err());
        assert!(Destination::capture(&state.lock_inner(), revision, None).is_err());
        let summary =
            import_aligned_stems_for_agent(&state, paths.clone(), revision, owner).unwrap();
        assert_eq!((summary.stem_count, summary.midi_track_count), (2, 2));
        assert_eq!(summary.revision, revision);
        assert_eq!(fs::read_dir(root.join("media")).unwrap().count(), 1);
        assert!(state.lock_inner().project.tracks[0].mix.mute);

        state.rollback_transaction().unwrap();
        assert!(state.lock_inner().project.tracks.is_empty());
        assert!(state.lock_inner().undo.undo.is_empty());
        assert_eq!(fs::read_dir(root.join("media")).unwrap().count(), 0);
        assert!(staged.join("Vocal.wav").is_file());

        let revision = state
            .begin_transaction("import stems".into(), owner.into())
            .unwrap();
        import_aligned_stems_for_agent(&state, paths, revision, owner).unwrap();
        let (_, published) = state.commit_transaction().unwrap();
        assert_eq!(published, revision + 1);
        crate::project_persist::flush_state(&state).unwrap();
        fs::remove_dir_all(&staged).unwrap();
        let mut inner = state.lock_inner();
        assert_eq!(inner.undo.undo.len(), 1);
        let saved =
            oc_dsl::parse(&fs::read_to_string(root.join("destination.ocp")).unwrap()).unwrap();
        let files = saved
            .audio_clips()
            .iter()
            .map(|clip| root.join(&clip.file))
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 2);
        assert_eq!(saved.properties.tempo_map[0].bpm, 80.0);
        for (file, sign) in files.iter().zip([1.0, -1.0]) {
            let (passage, _) = oc_engine::analysis::read_wav_window(file, 0.25, 0.01).unwrap();
            assert!(sign * passage.channels[0][0] > 0.2);
        }
        inner.undo_step().unwrap();
        assert!(inner.project.tracks.is_empty());
        let mut redo_project = inner.project.clone();
        inner.undo.redo(&mut redo_project).unwrap();
        assert_eq!(redo_project.audio_clips().len(), 2);
        assert!(files.iter().all(|path| path.is_file()));
    }

    #[test]
    fn aligned_stems_agent_refuses_replaced_or_edited_transactions_even_at_same_revision() {
        let (root, state) = state_fixture();
        fs::create_dir(root.join("handoff")).unwrap();
        write_stem(&root.join("handoff/Drums.wav"), 4_000, 1.0);
        let owner = "panel:stem-test";
        let mut inner = state.lock_inner();
        let revision = inner
            .begin_transaction("first".into(), owner.into())
            .unwrap();
        let prepare_import = |inner: &AppStateInner| {
            let destination = Destination::capture(inner, revision, Some(owner)).unwrap();
            prepare(destination, vec!["handoff/Drums.wav".into()]).unwrap()
        };
        let prepared = prepare_import(&inner);
        inner.rollback_transaction().unwrap();
        assert_eq!(
            inner
                .begin_transaction("second".into(), owner.into())
                .unwrap(),
            revision
        );
        assert!(commit(&mut inner, prepared, Some(owner)).is_err());

        let prepared = prepare_import(&inner);
        inner.bump_revision();
        assert_eq!(inner.revision, revision);
        assert!(commit(&mut inner, prepared, Some(owner)).is_err());
        assert_eq!(fs::read_dir(root.join("media")).unwrap().count(), 0);
        assert!(inner.project.tracks.is_empty());
    }

    #[test]
    fn aligned_stems_preserve_dual_mono_stereo_level_after_reopen() {
        let (root, state) = state_fixture();
        fs::create_dir(root.join("handoff")).unwrap();
        let samples = vec![0.25; 8_000];
        let source = root.join("handoff/Print.wav");
        fs::write(
            &source,
            oc_engine::wav::pcm_wav_bytes(&samples, 8_000, 2).unwrap(),
        )
        .unwrap();
        let revision = state.lock_inner().revision;
        import_aligned_stems_for_agent(&state, vec!["handoff/Print.wav".into()], revision, "http")
            .unwrap();

        let reopened = AppState::new();
        {
            let mut inner = reopened.lock_inner();
            inner.project = oc_dsl::parse(&state.lock_inner().dsl_source).unwrap();
            inner.file_path = Some(root.join("destination.ocp"));
            inner.audio.sample_rate = 8_000;
        }
        let tracks = crate::agent_api::tracks_list(&reopened).unwrap();
        assert_eq!(tracks["tracks"][0]["pan_law"], "balance");
        assert_eq!(tracks["tracks"][0]["volume_db"], 0.0);
        let output = root.join("reopened.wav");
        super::super::render_range(
            output.display().to_string(),
            super::super::RangeRequest {
                start_s: Some(0.0),
                end_s: Some(0.5),
                float_wav: true,
                ..Default::default()
            },
            &reopened,
        )
        .unwrap();
        let printed = hound::WavReader::open(source)
            .unwrap()
            .samples::<i16>()
            .map(|sample| f32::from(sample.unwrap()) / 32_768.0)
            .collect::<Vec<_>>();
        let rerendered = hound::WavReader::open(output)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert_eq!(rerendered, printed);
    }

    #[test]
    fn aligned_stems_agent_only_reads_staged_relative_paths() {
        let (root, state) = state_fixture();
        fs::create_dir(root.join("handoff")).unwrap();
        write_stem(&root.join("outside.wav"), 4_000, 1.0);
        let revision = state.lock_inner().revision;
        for path in [
            root.join("outside.wav"),
            "outside.wav".into(),
            "handoff/../outside.wav".into(),
        ] {
            assert!(import_aligned_stems_for_agent(&state, vec![path], revision, "http").is_err());
        }
        #[cfg(unix)]
        {
            let link = root.join("handoff/linked.wav");
            std::os::unix::fs::symlink(root.join("outside.wav"), &link).unwrap();
            assert!(
                import_aligned_stems_for_agent(
                    &state,
                    vec!["handoff/linked.wav".into()],
                    revision,
                    "http"
                )
                .is_err()
            );
            fs::remove_file(&link).unwrap();
            fs::remove_dir(root.join("handoff")).unwrap();
            std::os::unix::fs::symlink(&root, root.join("handoff")).unwrap();
            assert!(
                import_aligned_stems_for_agent(
                    &state,
                    vec!["handoff/outside.wav".into()],
                    revision,
                    "http"
                )
                .is_err()
            );
            fs::remove_file(root.join("handoff")).unwrap();
            fs::create_dir(root.join("handoff")).unwrap();
        }
        assert!(!root.join("media").exists());
        assert!(state.lock_inner().project.tracks.is_empty());

        write_stem(&root.join("handoff/Drums.wav"), 4_000, 1.0);
        let summary = import_aligned_stems_for_agent(
            &state,
            vec!["handoff/Drums.wav".into()],
            revision,
            "http",
        )
        .unwrap();
        assert_eq!((summary.revision, summary.stem_count), (revision + 1, 1));
        let mut wire = serde_json::to_value(summary).unwrap();
        crate::project_persist::after_tool(&state, "import_aligned_stems", &mut wire);
        assert!(wire.get("project_file_error").is_none(), "{wire}");
        let saved =
            oc_dsl::parse(&fs::read_to_string(root.join("destination.ocp")).unwrap()).unwrap();
        assert_eq!(saved.audio_clips().len(), 1);
        assert!(root.join(&saved.audio_clips()[0].file).is_file());
        assert_eq!(state.lock_inner().undo.undo.len(), 1);
    }

    #[test]
    fn aligned_stems_import_cleans_failed_copy_and_refuses_a_stale_destination() {
        let (root, mut inner) = destination_fixture();
        let wav = root.join("valid.wav");
        let invalid = root.join("invalid.wav");
        write_stem(&wav, 4_000, 1.0);
        fs::write(&invalid, b"not a WAV").unwrap();
        let dest = || Destination::capture(&inner, inner.revision, None).unwrap();
        assert!(prepare(dest(), vec![wav.clone(), invalid]).is_err());
        assert_eq!(fs::read_dir(root.join("media")).unwrap().count(), 0);
        let prepared = prepare(dest(), vec![wav]).unwrap();
        inner.bump_revision();
        assert!(commit(&mut inner, prepared, None).is_err());
        assert!(inner.project.tracks.is_empty());
        assert!(inner.undo.undo.is_empty());
        assert_eq!(fs::read_dir(root.join("media")).unwrap().count(), 0);
    }
}
