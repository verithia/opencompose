//! Resource limits shared by application and dataset MIDI import.
use super::MidiError;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// An import budget that can be exceeded.
pub enum ImportLimit {
    Bytes,
    Tracks,
    Events,
    Notes,
    OutputEvents,
    OutputTracks,
    Work,
    Extent,
    Metadata,
    Payload,
}
impl ImportLimit {
    /// Stable error code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Bytes => "midi_bytes_limit",
            Self::Tracks => "midi_tracks_limit",
            Self::Events => "midi_events_limit",
            Self::Notes => "midi_notes_limit",
            Self::OutputEvents => "midi_output_events_limit",
            Self::OutputTracks => "midi_output_tracks_limit",
            Self::Work => "midi_work_limit",
            Self::Extent => "midi_extent_limit",
            Self::Metadata => "midi_metadata_limit",
            Self::Payload => "midi_payload_limit",
        }
    }
}
impl fmt::Display for ImportLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Debug, Clone, Copy)]
/// Budgets applied while reading an SMF.
pub struct ImportLimits {
    pub max_bytes: usize,
    pub max_tracks: usize,
    pub max_events: usize,
    pub max_notes: usize,
    pub max_output_events: usize,
    pub max_output_tracks: usize,
    pub max_work: u64,
    pub max_quarter_notes: u64,
    pub max_metadata_bytes: usize,
    pub max_event_payload_bytes: usize,
}
impl ImportLimits {
    /// Default import budgets.
    pub const DEFAULT: Self = Self {
        max_bytes: 64 * 1024 * 1024,
        max_tracks: 4096,
        max_events: 2_000_000,
        max_notes: 500_000,
        max_output_events: 2_000_000,
        max_output_tracks: 8192,
        max_work: 200_000_000,
        max_quarter_notes: 1_000_000,
        max_metadata_bytes: 4 * 1024 * 1024,
        max_event_payload_bytes: 64 * 1024,
    };
    pub(crate) fn enforce(
        kind: ImportLimit,
        limit: u64,
        actual: u64,
        offset: usize,
    ) -> Result<(), MidiError> {
        if actual > limit {
            Err(MidiError::LimitExceeded {
                kind,
                limit,
                actual,
                offset,
            })
        } else {
            Ok(())
        }
    }
}
impl Default for ImportLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Read an SMF with default limits.
pub fn read_smf_file(path: &Path) -> Result<Vec<u8>, MidiError> {
    read_smf_file_with_limits(path, &ImportLimits::default())
}
/// Read an SMF with explicit limits.
pub fn read_smf_file_with_limits(path: &Path, limits: &ImportLimits) -> Result<Vec<u8>, MidiError> {
    read_smf_file_cancellable(path, limits, &AtomicBool::new(false))
}
/// Nonblocking open on Unix, followed by handle metadata, prevents a FIFO or
/// path swap from blocking the importer. Symlinks to ordinary files are allowed.
pub fn read_smf_file_cancellable(
    path: &Path,
    limits: &ImportLimits,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, MidiError> {
    let io = |error: std::io::Error| MidiError::InputIo {
        message: format!("{}: {error}", path.display()),
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(MidiError::Cancelled);
    }
    let metadata = std::fs::metadata(path).map_err(io)?;
    if !metadata.is_file() {
        return Err(MidiError::NonRegularInput);
    }
    ImportLimits::enforce(
        ImportLimit::Bytes,
        limits.max_bytes as u64,
        metadata.len(),
        0,
    )?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(io)?;
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() {
        return Err(MidiError::NonRegularInput);
    }
    ImportLimits::enforce(
        ImportLimit::Bytes,
        limits.max_bytes as u64,
        metadata.len(),
        0,
    )?;
    read_bounded(&mut file, limits, cancel).map_err(|error| match error {
        MidiError::InputIo { message } => MidiError::InputIo {
            message: format!("{}: {message}", path.display()),
        },
        other => other,
    })
}

fn read_bounded(
    reader: &mut impl Read,
    limits: &ImportLimits,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, MidiError> {
    let mut bytes = Vec::new();
    let mut block = [0u8; 16 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(MidiError::Cancelled);
        }
        let remaining = limits
            .max_bytes
            .saturating_sub(bytes.len())
            .saturating_add(1)
            .min(block.len());
        let count = reader
            .read(&mut block[..remaining])
            .map_err(|error| MidiError::InputIo {
                message: error.to_string(),
            })?;
        if count == 0 {
            break;
        }
        ImportLimits::enforce(
            ImportLimit::Bytes,
            limits.max_bytes as u64,
            bytes.len().saturating_add(count) as u64,
            0,
        )?;
        bytes.extend_from_slice(&block[..count]);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::{ImportOptions, import_smf_dataset_with_limits, import_smf_with_limits};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };

    fn fixture(delta: u32, metadata: bool) -> Vec<u8> {
        let mut events = vec![];
        if metadata {
            events.push(TrackEvent {
                delta: 0.into(),
                kind: TrackEventKind::Meta(MetaMessage::TrackName(b"budget")),
            });
        }
        events.extend([
            TrackEvent {
                delta: delta.into(),
                kind: TrackEventKind::Midi {
                    channel: 0.into(),
                    message: MidiMessage::NoteOn {
                        key: 60.into(),
                        vel: 100.into(),
                    },
                },
            },
            TrackEvent {
                delta: 1.into(),
                kind: TrackEventKind::Midi {
                    channel: 0.into(),
                    message: MidiMessage::NoteOff {
                        key: 60.into(),
                        vel: 0.into(),
                    },
                },
            },
        ]);
        let smf = Smf {
            header: Header::new(Format::SingleTrack, Timing::Metrical(1.into())),
            tracks: vec![events],
        };
        let mut bytes = vec![];
        smf.write_std(&mut bytes).unwrap();
        bytes
    }
    fn app(data: &[u8], limits: &ImportLimits) -> Result<crate::midi::ImportResult, MidiError> {
        import_smf_with_limits(data, None, &ImportOptions::default(), None, limits)
    }

    #[test]
    fn small_shared_limits_refuse_before_unbounded_event_materialization() {
        let data = fixture(1, false);
        let cases = [
            (
                ImportLimits {
                    max_bytes: data.len() - 1,
                    ..Default::default()
                },
                ImportLimit::Bytes,
            ),
            (
                ImportLimits {
                    max_tracks: 0,
                    ..Default::default()
                },
                ImportLimit::Tracks,
            ),
            (
                ImportLimits {
                    max_events: 1,
                    ..Default::default()
                },
                ImportLimit::Events,
            ),
            (
                ImportLimits {
                    max_notes: 0,
                    ..Default::default()
                },
                ImportLimit::Notes,
            ),
            (
                ImportLimits {
                    max_output_events: 0,
                    ..Default::default()
                },
                ImportLimit::OutputEvents,
            ),
            (
                ImportLimits {
                    max_output_tracks: 0,
                    ..Default::default()
                },
                ImportLimit::OutputTracks,
            ),
            (
                ImportLimits {
                    max_work: 0,
                    ..Default::default()
                },
                ImportLimit::Work,
            ),
            (
                ImportLimits {
                    max_quarter_notes: 0,
                    ..Default::default()
                },
                ImportLimit::Extent,
            ),
        ];
        for (limits, expected) in cases {
            for result in [
                app(&data, &limits),
                import_smf_dataset_with_limits(&data, None, &limits),
            ] {
                let error = result.unwrap_err();
                assert_eq!(error.code(), expected.code(), "{error}");
                assert!(matches!(error,MidiError::LimitExceeded {kind,..} if kind==expected));
            }
        }
        assert_eq!(
            app(&data, &ImportLimits::default())
                .unwrap()
                .summary
                .target_ppq,
            480
        );
        assert_eq!(
            import_smf_dataset_with_limits(&data, None, &ImportLimits::default())
                .unwrap()
                .summary
                .target_ppq,
            1
        );
    }

    #[test]
    fn event_expansion_is_refused_in_parse_stage_before_track_materialization() {
        let data = fixture(0, false);
        let limits = ImportLimits {
            max_events: 1,
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        let mut stages = Vec::new();
        let error = crate::midi::import_smf_cancellable_with_limits(
            &data,
            None,
            &ImportOptions::default(),
            None,
            &cancel,
            &mut |stage, _| stages.push(stage),
            &limits,
        )
        .unwrap_err();
        assert_eq!(error.code(), ImportLimit::Events.code());
        assert!(!stages.is_empty());
        assert!(
            stages
                .iter()
                .all(|stage| *stage == crate::midi::ImportStage::Parse),
            "{stages:?}"
        );
    }

    #[test]
    fn metadata_and_unrepresentable_time_are_checked_without_giant_fixtures() {
        let data = fixture(0, true);
        for (limits, kind) in [
            (
                ImportLimits {
                    max_metadata_bytes: 1,
                    ..Default::default()
                },
                ImportLimit::Metadata,
            ),
            (
                ImportLimits {
                    max_event_payload_bytes: 1,
                    ..Default::default()
                },
                ImportLimit::Payload,
            ),
        ] {
            assert_eq!(app(&data, &limits).unwrap_err().code(), kind.code());
        }
        let data = fixture(0x0fff_ffff, false);
        assert_eq!(
            app(
                &data,
                &ImportLimits {
                    max_quarter_notes: 16,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .code(),
            ImportLimit::Extent.code()
        );
    }

    #[test]
    fn ignored_sysex_and_escape_enforce_exact_encoded_event_payload_boundary_in_preflight() {
        for kind in [
            TrackEventKind::SysEx(b"\x01\x02\x03\xf7"),
            TrackEventKind::Escape(b"\x01\x02\x03"),
        ] {
            let smf = Smf {
                header: Header::new(Format::SingleTrack, Timing::Metrical(480.into())),
                tracks: vec![vec![TrackEvent {
                    delta: 0.into(),
                    kind,
                }]],
            };
            let mut bytes = vec![];
            smf.write_std(&mut bytes).unwrap();
            assert_eq!(&bytes[14..18], b"MTrk");
            let (body, _) = midly::EventBytemapIter::new(&bytes[22..])
                .next()
                .unwrap()
                .unwrap();
            let exact = ImportLimits {
                max_event_payload_bytes: body.len(),
                ..Default::default()
            };
            assert!(app(&bytes, &exact).is_ok());
            assert!(import_smf_dataset_with_limits(&bytes, None, &exact).is_ok());
            let smaller = ImportLimits {
                max_event_payload_bytes: body.len() - 1,
                ..exact
            };
            let mut stages = vec![];
            let error = crate::midi::import_smf_cancellable_with_limits(
                &bytes,
                None,
                &ImportOptions::default(),
                None,
                &AtomicBool::new(false),
                &mut |stage, _| stages.push(stage),
                &smaller,
            )
            .unwrap_err();
            assert_eq!(error.code(), ImportLimit::Payload.code());
            assert!(
                stages
                    .iter()
                    .all(|stage| *stage == crate::midi::ImportStage::Parse)
            );
            assert_eq!(
                import_smf_dataset_with_limits(&bytes, None, &smaller)
                    .unwrap_err()
                    .code(),
                ImportLimit::Payload.code()
            );
        }
    }

    #[test]
    fn regular_files_cancellation_and_directory_refusals_share_typed_errors() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.mid");
        let bytes = fixture(0, false);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        assert_eq!(read_smf_file(&path).unwrap(), bytes);
        assert_eq!(
            read_smf_file_with_limits(
                &path,
                &ImportLimits {
                    max_bytes: 8,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .code(),
            ImportLimit::Bytes.code()
        );
        assert_eq!(
            read_smf_file(directory.path()).unwrap_err().code(),
            "midi_nonregular_input"
        );
        assert!(matches!(
            read_smf_file_cancellable(&path, &Default::default(), &AtomicBool::new(true)),
            Err(MidiError::Cancelled)
        ));
    }

    #[test]
    fn file_growth_during_read_cannot_bypass_the_byte_limit() {
        use std::io::Write;
        struct Growing {
            reader: std::fs::File,
            writer: std::fs::File,
            grew: bool,
        }
        impl Read for Growing {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let count = self.reader.read(output)?;
                if !self.grew {
                    self.writer.write_all(b"extra data")?;
                    self.writer.flush()?;
                    self.grew = true;
                }
                Ok(count)
            }
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"ok").unwrap();
        let mut growing = Growing {
            reader: std::fs::File::open(file.path()).unwrap(),
            writer: std::fs::OpenOptions::new()
                .append(true)
                .open(file.path())
                .unwrap(),
            grew: false,
        };
        let error = read_bounded(
            &mut growing,
            &ImportLimits {
                max_bytes: 4,
                ..Default::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.code(), ImportLimit::Bytes.code());
    }

    #[cfg(unix)]
    #[test]
    fn unix_symlink_to_regular_input_is_allowed_and_fifo_is_refused_without_waiting() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("input.mid");
        let bytes = fixture(0, false);
        std::fs::write(&target, &bytes).unwrap();
        let link = directory.path().join("linked.mid");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(read_smf_file(&link).unwrap(), bytes);
        let fifo = directory.path().join("pipe.mid");
        let cpath = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: cpath is a live NUL-terminated local fixture path.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_smf_file(&fifo).unwrap_err().code(),
            "midi_nonregular_input"
        );
    }
}
