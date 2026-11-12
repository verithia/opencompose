//! Source locations belong to one immutable parse snapshot, never an edited AST.

use crate::{ast::Project, diff::compressed::CodecLimits, error::ParseError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// AST owner a source span belongs to.
pub enum SourceOwner {
    Project,
    Preset(usize),
    Track(usize),
    Bus(usize),
    Master,
    Pattern(usize),
    AudioClip(usize),
    Section(usize),
    Marker(usize),
    Group(usize),
    ControlSource(usize),
    ControlLink(usize),
    VocalTune(usize),
    VocalHarmony(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Project property that carries source locations.
pub enum ProjectField {
    Tempo,
    TimeSignature,
    Key,
    Swing,
    SoloMode,
    Mpe,
    Export,
    LiveResample,
    AutomationNormalization,
    PictureClock,
    TempoMap,
    TimeSignatureMap,
    KeyMap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Kind of cross-reference a source part names.
pub enum ReferenceKind {
    Preset,
    ParentPreset,
    Track,
    Clip,
    VocalReference,
    BusRoute,
    GroupMember,
    ControlTap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Specific part of an owner, such as its name or a reference.
pub enum SourcePart {
    Whole,
    Name,
    Version,
    Instrument,
    TimeRange,
    ProjectField(ProjectField),
    Reference {
        kind: ReferenceKind,
        occurrence: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Owner and part addressed by the source map.
pub struct SourceSubject {
    pub owner: SourceOwner,
    pub part: SourcePart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Half-open byte range in the parsed source.
pub struct ByteSpan {
    start: usize,
    end: usize,
}

impl ByteSpan {
    /// Validate a range against the source's UTF-8 boundaries.
    pub fn checked(source: &str, start: usize, end: usize) -> Result<Self, SourceMapError> {
        if start > end
            || end > source.len()
            || !source.is_char_boundary(start)
            || !source.is_char_boundary(end)
        {
            return Err(SourceMapError::InvalidSpan);
        }
        Ok(Self { start, end })
    }
    /// Start byte offset.
    pub fn start(self) -> usize {
        self.start
    }
    /// End byte offset.
    pub fn end(self) -> usize {
        self.end
    }
    /// The source slice when the range is valid.
    pub fn text(self, source: &str) -> Option<&str> {
        source.get(self.start..self.end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Zero-based line and UTF-16 column.
pub struct Utf16Position {
    pub line: u32,
    pub character: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// UTF-16 range for one byte span.
pub struct Utf16Range {
    pub start: Utf16Position,
    pub end: Utf16Position,
}

#[derive(Clone, Copy, Debug)]
/// Budgets applied while building a source map.
pub struct SourceLimits {
    pub document: CodecLimits,
    pub max_parse_nodes: usize,
    pub max_parse_depth: usize,
    pub max_map_entries: usize,
    pub max_index_entries: usize,
}
impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            document: CodecLimits::default(),
            max_parse_nodes: 1_000_000,
            max_parse_depth: 128,
            max_map_entries: 262_144,
            max_index_entries: 1_000_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
/// Why a source map could not be built.
pub enum SourceMapError {
    #[error("Source exceeds the document byte limit")]
    SourceLimit,
    #[error("Source location budget exceeded: {0}")]
    Budget(&'static str),
    #[error("Source location allocation failed")]
    Allocation,
    #[error("Source span is outside UTF-8 boundaries")]
    InvalidSpan,
}
#[derive(Debug, thiserror::Error)]
/// A parse failure with its source-location error.
pub enum DocumentParseError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Source(#[from] SourceMapError),
}

#[derive(Debug)]
struct Checkpoint {
    byte: usize,
    position: Utf16Position,
}

/// Sparse checkpoints bound each conversion to at most 64 Unicode scalars.
#[derive(Debug)]
pub struct SourceMap {
    entries: Vec<(SourceSubject, ByteSpan)>,
    checkpoints: Vec<Checkpoint>,
    audio_tracks: Vec<usize>,
}

pub(crate) fn bounded_push<T>(
    items: &mut Vec<T>,
    value: T,
    limit: usize,
    name: &'static str,
) -> Result<(), SourceMapError> {
    if items.len() >= limit {
        return Err(SourceMapError::Budget(name));
    }
    if items.len() == items.capacity() {
        items
            .try_reserve_exact((limit - items.len()).min(256))
            .map_err(|_| SourceMapError::Allocation)?;
    }
    items.push(value);
    Ok(())
}

/// CRLF is one separator; its interior byte boundary stays at the old line end.
fn advance_position(
    position: &mut Utf16Position,
    source: &str,
    byte: usize,
    ch: char,
) -> Result<bool, SourceMapError> {
    let crlf_head = ch == '\r' && source.as_bytes().get(byte + 1) == Some(&b'\n');
    let newline = ch == '\n' || (ch == '\r' && !crlf_head);
    if newline {
        position.line = position
            .line
            .checked_add(1)
            .ok_or(SourceMapError::Budget("lines"))?;
        position.character = 0;
    } else if !crlf_head {
        position.character = position
            .character
            .checked_add(ch.len_utf16() as u32)
            .ok_or(SourceMapError::Budget("columns"))?;
    }
    Ok(newline)
}

impl SourceMap {
    /// Build a map and its sparse position checkpoints.
    pub(crate) fn new(source: &str, limits: SourceLimits) -> Result<Self, SourceMapError> {
        if source.len() > limits.document.max_encoded_bytes
            || source.len() > limits.document.max_decoded_bytes
        {
            return Err(SourceMapError::SourceLimit);
        }
        let mut checkpoints = Vec::new();
        let mut position = Utf16Position {
            line: 0,
            character: 0,
        };
        bounded_push(
            &mut checkpoints,
            Checkpoint { byte: 0, position },
            limits.max_index_entries,
            "UTF-16 index",
        )?;
        let mut distance = 0;
        for (byte, ch) in source.char_indices() {
            let newline = advance_position(&mut position, source, byte, ch)?;
            distance += 1;
            if distance == 64 || newline {
                bounded_push(
                    &mut checkpoints,
                    Checkpoint {
                        byte: byte + ch.len_utf8(),
                        position,
                    },
                    limits.max_index_entries,
                    "UTF-16 index",
                )?;
                distance = 0;
            }
        }
        Ok(Self {
            entries: Vec::new(),
            checkpoints,
            audio_tracks: Vec::new(),
        })
    }
    /// The span recorded for a subject.
    pub fn span(&self, subject: SourceSubject) -> Option<ByteSpan> {
        self.entries
            .binary_search_by_key(&subject, |(key, _)| *key)
            .ok()
            .map(|index| self.entries[index].1)
    }
    /// Audio declaration ordinal to the track actually selected by the parser.
    pub fn audio_clip_track(&self, ordinal: usize) -> Option<usize> {
        self.audio_tracks.get(ordinal).copied()
    }
    pub(crate) fn insert(
        &mut self,
        subject: SourceSubject,
        span: ByteSpan,
        limit: usize,
    ) -> Result<(), SourceMapError> {
        bounded_push(&mut self.entries, (subject, span), limit, "source subjects")
    }
    pub(crate) fn relocate_audio(
        &mut self,
        track: usize,
        limit: usize,
    ) -> Result<(), SourceMapError> {
        bounded_push(&mut self.audio_tracks, track, limit, "audio ownership")
    }
    pub(crate) fn finish(&mut self) {
        // Stable sorting and reverse dedup preserve the last effective assignment.
        self.entries.sort_by_key(|(subject, _)| *subject);
        self.entries.reverse();
        self.entries.dedup_by_key(|(subject, _)| *subject);
        self.entries.reverse();
    }
    fn position(&self, source: &str, byte: usize) -> Result<Utf16Position, SourceMapError> {
        ByteSpan::checked(source, byte, byte)?;
        let index = self
            .checkpoints
            .partition_point(|checkpoint| checkpoint.byte <= byte)
            .saturating_sub(1);
        let checkpoint = &self.checkpoints[index];
        let mut position = checkpoint.position;
        for (offset, ch) in source[checkpoint.byte..byte].char_indices() {
            advance_position(&mut position, source, checkpoint.byte + offset, ch)?;
        }
        Ok(position)
    }
}

/// Neither the AST nor its map can be mutated independently of the authored text.
#[derive(Debug)]
pub struct ParsedDocument {
    project: Project,
    source: String,
    map: SourceMap,
}
impl ParsedDocument {
    pub(crate) fn new(
        project: Project,
        source: &str,
        map: SourceMap,
    ) -> Result<Self, SourceMapError> {
        let mut owned = String::new();
        owned
            .try_reserve_exact(source.len())
            .map_err(|_| SourceMapError::Allocation)?;
        owned.push_str(source);
        Ok(Self {
            project,
            source: owned,
            map,
        })
    }
    /// Validate this exact AST while retaining check-emitted location subjects.
    pub fn validation_diagnostics(&self) -> Vec<crate::diagnostics::ValidationDiagnostic> {
        crate::validator::validate_diagnostics(&self.project)
    }
    /// The parsed project.
    pub fn project(&self) -> &Project {
        &self.project
    }
    /// The source text this map describes.
    pub fn source(&self) -> &str {
        &self.source
    }
    /// The location table.
    pub fn source_map(&self) -> &SourceMap {
        &self.map
    }
    /// Consume the document and return its project.
    pub fn into_project(self) -> Project {
        self.project
    }
    /// The byte span for a subject in this document.
    pub fn span(&self, subject: SourceSubject) -> Option<ByteSpan> {
        self.map.span(subject)
    }
    /// Convert a byte span to a UTF-16 range for the language server.
    pub fn utf16_range(&self, span: ByteSpan) -> Result<Utf16Range, SourceMapError> {
        ByteSpan::checked(&self.source, span.start, span.end)?;
        Ok(Utf16Range {
            start: self.map.position(&self.source, span.start)?,
            end: self.map.position(&self.source, span.end)?,
        })
    }
}
