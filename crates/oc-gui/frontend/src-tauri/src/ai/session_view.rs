//! Reading the current view of a conversation without parsing its whole file,
//! and cutting that view into pages of whole turns for the panel.
//!
//! A `compaction` or `trim` record replaces the view, so the view is the
//! newest such record plus the `turn` records after it. `view.json` remembers
//! where that record starts; a read seeks there and parses only what follows.
//! The index is a hint: a read checks that the line it names is still that
//! record, and falls back to a full scan otherwise. Starting from an older
//! replacing record, or from the first line, builds the same view, so an index
//! one append behind is slower, never wrong. A rewrite of the file removes the
//! index before it replaces the file.
//!
//! A page boundary is a message the person sent. After pairing repair only the
//! round open at the end can lack its results, so no page splits a tool call
//! from its result. Appends only add messages after the newest page; a
//! replacing record changes the view's `revision`, and a page asked for under
//! an older revision is refused as stale.

use super::{MESSAGES_FILE, MessageKind, Record, RecordKind, message_protocol};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::Path;

/// The index of the newest replacing record, beside `messages.jsonl`.
pub const VIEW_INDEX: &str = "view.json";
const INDEX_VERSION: u32 = 1;
/// The most turns one page carries.
pub const MAX_PAGE_TURNS: usize = 64;
/// A page stops adding older turns past this many serialized bytes; it always
/// carries at least one turn.
pub const PAGE_BYTES: usize = 4 * 1024 * 1024;
/// The longest line the index may name; a longer one is not trusted.
const MAX_INDEXED_LINE: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ViewIndex {
    v: u32,
    /// Byte offset of the newest replacing record's line.
    offset: u64,
    /// Length of that line without its newline.
    len: u64,
    /// The record's own timestamp, checked against the line.
    at: String,
    /// Whether a compaction record sits before `offset`.
    compacted: bool,
    /// Lines before `offset` this build could not read.
    skipped: u64,
}

/// The raw view and what the read learned about the file.
#[derive(Debug, Default)]
pub(super) struct Tail {
    pub messages: Vec<Value>,
    pub skipped: u64,
    /// Whether any compaction record is in the file.
    pub compacted: bool,
    /// Names the newest replacing record; it changes whenever positions in
    /// the view can change.
    pub revision: String,
}

/// The messages of the view, before media, protocol and pairing repair.
pub(super) fn read_tail(dir: &Path) -> Result<Tail, String> {
    let path = dir.join(MESSAGES_FILE);
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Tail {
                revision: revision_of(None),
                ..Tail::default()
            });
        }
        Err(error) => return Err(format!("sessions: {error}")),
    };
    let index = read_index(dir).filter(|index| index_holds(&mut file, index));
    let start = index.as_ref().map_or(0, |index| index.offset);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("sessions: {error}"))?;
    let mut tail = Tail {
        compacted: index.as_ref().is_some_and(|index| index.compacted),
        skipped: index.as_ref().map_or(0, |index| index.skipped),
        ..Tail::default()
    };
    // What the scan saw before the newest replacing record, for a new index.
    let mut newest: Option<ViewIndex> = index.clone();
    let mut compacted_before = tail.compacted;
    let mut skipped_before = tail.skipped;
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    let mut offset = start;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("sessions: {error}"))?;
        if read == 0 {
            break;
        }
        let at_offset = offset;
        offset += read as u64;
        let body = line.strip_suffix(b"\n").unwrap_or(&line);
        if body.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Record>(body) else {
            tail.skipped += 1;
            continue;
        };
        match record.kind {
            RecordKind::Turn => tail.messages.extend(record.messages),
            RecordKind::Compaction | RecordKind::Trim => {
                newest = Some(ViewIndex {
                    v: INDEX_VERSION,
                    offset: at_offset,
                    len: body.len() as u64,
                    at: record.at.clone(),
                    compacted: compacted_before,
                    skipped: skipped_before,
                });
                if record.kind == RecordKind::Compaction {
                    tail.compacted = true;
                }
                tail.messages = record.messages;
            }
        }
        compacted_before = tail.compacted;
        skipped_before = tail.skipped;
    }
    if let Some(found) = newest.as_ref()
        && newest != index
    {
        // A failed write only costs the next read a longer scan.
        let _ = write_index(dir, found);
    }
    tail.revision = revision_of(newest.as_ref());
    Ok(tail)
}

/// Remove the index before a rewrite replaces the file it points into.
pub(super) fn forget(dir: &Path) -> Result<(), String> {
    match std::fs::remove_file(dir.join(VIEW_INDEX)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("sessions: {error}")),
    }
}

fn revision_of(index: Option<&ViewIndex>) -> String {
    match index {
        Some(index) => format!("{}:{}:{}", index.offset, index.len, index.at),
        None => "0".into(),
    }
}

fn read_index(dir: &Path) -> Option<ViewIndex> {
    let bytes = std::fs::read(dir.join(VIEW_INDEX)).ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    let index: ViewIndex = serde_json::from_slice(&bytes).ok()?;
    (index.v == INDEX_VERSION && index.len <= MAX_INDEXED_LINE).then_some(index)
}

fn write_index(dir: &Path, index: &ViewIndex) -> Result<(), String> {
    let bytes = serde_json::to_vec(index).map_err(|error| format!("sessions: {error}"))?;
    crate::private_fs::write_atomic(&dir.join(VIEW_INDEX), &bytes)
        .map_err(|error| format!("sessions: {error}"))
}

/// Whether the line at the index's offset is still the record it names.
fn index_holds(file: &mut std::fs::File, index: &ViewIndex) -> bool {
    let Ok(length) = file.metadata().map(|meta| meta.len()) else {
        return false;
    };
    if index.offset.saturating_add(index.len) > length {
        return false;
    }
    if index.offset > 0 {
        // The line must start a line: the byte before it ends the previous one.
        let mut before = [0u8; 1];
        if file.seek(SeekFrom::Start(index.offset - 1)).is_err()
            || file.read_exact(&mut before).is_err()
            || before[0] != b'\n'
        {
            return false;
        }
    }
    if file.seek(SeekFrom::Start(index.offset)).is_err() {
        return false;
    }
    let mut body = vec![0u8; index.len as usize];
    if file.read_exact(&mut body).is_err() {
        return false;
    }
    let mut end = [0u8; 1];
    let ends = match file.read(&mut end) {
        Ok(0) => true,
        Ok(_) => end[0] == b'\n',
        Err(_) => false,
    };
    ends && serde_json::from_slice::<Record>(&body).is_ok_and(|record| {
        record.at == index.at && matches!(record.kind, RecordKind::Compaction | RecordKind::Trim)
    })
}

/// Where a page may start: a message the person sent.
pub(super) fn starts_turn(message: &Value) -> bool {
    message["role"] == "user" && message_protocol::kind(message) == Some(MessageKind::User)
}

/// The start of the page that ends before `before`: back to the `turns`-th
/// turn, or fewer once the page passes [`PAGE_BYTES`], or the beginning.
pub(super) fn page_start(view: &[Value], before: usize, turns: usize) -> usize {
    let turns = turns.clamp(1, MAX_PAGE_TURNS);
    let mut counted = 0;
    let mut bytes = 0usize;
    for index in (0..before.min(view.len())).rev() {
        bytes += serde_json::to_vec(&view[index]).map_or(0, |bytes| bytes.len());
        if starts_turn(&view[index]) {
            counted += 1;
            if counted >= turns || bytes >= PAGE_BYTES {
                return index;
            }
        }
    }
    0
}

/// The panel's own estimate of `messages`: their text joined by newlines, a
/// token per four narrow characters and one per wide one. It matches the
/// composer's count, so rows not loaded yet still count toward it.
pub(super) fn panel_tokens(messages: &[Value]) -> u64 {
    let mut narrow = 0u64;
    let mut wide = 0u64;
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            narrow += 1;
        }
        for character in message["content"].as_str().unwrap_or_default().chars() {
            if is_wide(character) {
                wide += 1;
            } else {
                narrow += 1;
            }
        }
    }
    narrow.div_ceil(4) + wide
}

fn is_wide(character: char) -> bool {
    matches!(
        character,
        '\u{3040}'..='\u{30ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{ac00}'..='\u{d7af}'
    )
}

#[cfg(test)]
mod tests {
    use super::super::{
        Dropped, PageRead, Record, RecordKind, Store, VERSION, append_records, read_current,
        write_records,
    };
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> Value {
        json!({"role":"user","content":text,"message_meta":{"version":1,"kind":"user"}})
    }
    fn call(id: &str) -> Value {
        json!({"role":"assistant","content":null,"tool_calls":[{
            "id": id, "type":"function", "function":{"name":"project_get","arguments":"{}"}
        }],"message_meta":{"version":1,"kind":"assistant"}})
    }
    fn result(id: &str) -> Value {
        json!({"role":"tool","tool_call_id":id,"content":"ok","message_meta":{"version":1,"kind":"tool"}})
    }
    fn answer(text: &str) -> Value {
        json!({"role":"assistant","content":text,"message_meta":{"version":1,"kind":"assistant"}})
    }
    fn record(kind: RecordKind, at: &str, messages: Vec<Value>) -> Record {
        Record {
            v: VERSION,
            kind,
            at: at.into(),
            dropped: (kind == RecordKind::Trim).then_some(Dropped { from: 0, to: 1 }),
            tokens_before: None,
            tokens_after: None,
            truncated: None,
            messages,
        }
    }
    fn turn(index: usize) -> Vec<Value> {
        let id = format!("c{index}");
        vec![
            user(&format!("ask {index}")),
            call(&id),
            result(&id),
            answer(&format!("done {index}")),
        ]
    }
    fn fixture() -> (tempfile::TempDir, Store, String) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("sessions"));
        let id = store.create(None, None, Some("Pages"), None).unwrap().id;
        (root, store, id)
    }
    fn dir_of(store: &Store, id: &str) -> std::path::PathBuf {
        store.find(id).unwrap().0
    }
    fn index_of(dir: &Path) -> Option<ViewIndex> {
        read_index(dir)
    }
    /// Overwrite the first line with junk of the same length, keeping offsets.
    fn spoil_first_line(dir: &Path) {
        let path = dir.join(MESSAGES_FILE);
        let mut bytes = std::fs::read(&path).unwrap();
        let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        bytes[..end].fill(b'x');
        std::fs::write(&path, bytes).unwrap();
    }

    #[test]
    fn a_read_starts_at_the_newest_replacing_record_the_index_names() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        append_records(dir, &[record(RecordKind::Turn, "t1", turn(0))]).unwrap();
        append_records(dir, &[record(RecordKind::Trim, "t2", turn(1))]).unwrap();
        append_records(dir, &[record(RecordKind::Turn, "t3", turn(2))]).unwrap();
        let first = read_current(dir).unwrap();
        assert_eq!(first.messages.len(), 8);
        let index = index_of(dir).expect("the read indexes the trim");
        let first_line = std::fs::read(dir.join(MESSAGES_FILE)).unwrap();
        let first_end = first_line.iter().position(|byte| *byte == b'\n').unwrap();
        assert_eq!(index.offset, first_end as u64 + 1);
        assert_eq!(index.at, "t2");

        // The line before the index is never parsed again.
        spoil_first_line(dir);
        let seeked = read_current(dir).unwrap();
        assert_eq!(seeked.messages, first.messages);
        assert_eq!(seeked.skipped, 0);
        assert_eq!(seeked.revision, first.revision);

        // Without the index the scan starts over and meets the spoiled line.
        forget(dir).unwrap();
        let scanned = read_current(dir).unwrap();
        assert_eq!(scanned.messages, first.messages);
        assert_eq!(scanned.skipped, 1);
    }

    #[test]
    fn an_index_that_names_another_line_is_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        append_records(dir, &[record(RecordKind::Trim, "t1", turn(0))]).unwrap();
        append_records(dir, &[record(RecordKind::Turn, "t2", turn(1))]).unwrap();
        let expected = read_current(dir).unwrap().messages;
        let good = index_of(dir).unwrap();
        for wrong in [
            ViewIndex {
                at: "elsewhere".into(),
                ..good.clone()
            },
            ViewIndex {
                offset: good.offset + 1,
                ..good.clone()
            },
            ViewIndex {
                len: good.len + 1,
                ..good.clone()
            },
            ViewIndex {
                offset: u64::MAX - 1,
                ..good.clone()
            },
        ] {
            write_index(dir, &wrong).unwrap();
            assert_eq!(read_current(dir).unwrap().messages, expected, "{wrong:?}");
            assert_eq!(index_of(dir).as_ref(), Some(&good), "the read repairs it");
        }
        // An index naming a turn record is not a starting point either.
        let turn_line = good.offset + good.len + 1;
        let body = std::fs::read(dir.join(MESSAGES_FILE)).unwrap();
        let turn_len = body[turn_line as usize..]
            .iter()
            .position(|b| *b == b'\n')
            .unwrap();
        write_index(
            dir,
            &ViewIndex {
                offset: turn_line,
                len: turn_len as u64,
                at: "t2".into(),
                ..good.clone()
            },
        )
        .unwrap();
        assert_eq!(read_current(dir).unwrap().messages, expected);
    }

    #[test]
    fn an_index_one_replacement_behind_still_reads_the_newest_view() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        append_records(dir, &[record(RecordKind::Compaction, "t1", turn(0))]).unwrap();
        read_current(dir).unwrap();
        let behind = index_of(dir).unwrap();
        append_records(dir, &[record(RecordKind::Trim, "t2", turn(1))]).unwrap();
        append_records(dir, &[record(RecordKind::Turn, "t3", turn(2))]).unwrap();
        let current = read_current(dir).unwrap();
        assert_eq!(current.messages, [turn(1), turn(2)].concat());
        assert!(
            current.compacted,
            "the compaction before the trim still counts"
        );
        let ahead = index_of(dir).unwrap();
        assert_eq!(ahead.at, "t2");
        assert!(ahead.compacted);
        assert_ne!(ahead, behind);
        // Read from the new index: the compaction is behind it, and still known.
        let again = read_current(dir).unwrap();
        assert!(again.compacted);
        assert_eq!(again.revision, current.revision);
    }

    #[test]
    fn a_rewrite_removes_the_index_before_it_replaces_the_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        append_records(dir, &[record(RecordKind::Turn, "t1", turn(0))]).unwrap();
        append_records(dir, &[record(RecordKind::Trim, "t2", turn(1))]).unwrap();
        let before = read_current(dir).unwrap();
        assert!(index_of(dir).is_some());
        write_records(dir, &[record(RecordKind::Turn, "t3", turn(5))]).unwrap();
        assert!(index_of(dir).is_none());
        let after = read_current(dir).unwrap();
        assert_eq!(after.messages, turn(5));
        assert_ne!(after.revision, before.revision);
        assert!(
            index_of(dir).is_none(),
            "a file with no replacing record needs none"
        );
    }

    #[test]
    fn pages_cover_the_view_in_whole_turns_from_the_newest() {
        let (_root, store, id) = fixture();
        for index in 0..70 {
            store.append_turn(&id, &turn(index), None).unwrap();
        }
        let whole = store.load(&id).unwrap().messages;
        let opened = store.open_page(&id, MAX_PAGE_TURNS).unwrap();
        assert_eq!(opened.total, whole.len());
        assert_eq!(opened.start, 6 * 4, "the newest 64 turns");
        assert_eq!(opened.loaded.messages, whole[opened.start..]);
        assert_eq!(opened.older_tokens, panel_tokens(&whole[..opened.start]));
        let PageRead::Page(older) = store.page(&id, opened.start, 4, &opened.revision).unwrap()
        else {
            panic!("the view did not change");
        };
        assert_eq!((older.start, older.total), (2 * 4, whole.len()));
        assert_eq!(older.messages, whole[older.start..opened.start]);
        let PageRead::Page(first) = store.page(&id, older.start, 64, &older.revision).unwrap()
        else {
            panic!("the view did not change");
        };
        assert_eq!(first.start, 0);
        assert_eq!(first.older_tokens, 0);
        assert_eq!(
            [first.messages, older.messages, opened.loaded.messages].concat(),
            whole
        );
        // Every page starts on a message the person sent.
        assert!(starts_turn(&whole[older.start]));
        // A turn count past the bound is held to it; zero still yields a turn.
        assert_eq!(store.open_page(&id, 1000).unwrap().start, opened.start);
        assert_eq!(store.open_page(&id, 0).unwrap().start, whole.len() - 4);
    }

    #[test]
    fn a_page_asked_for_after_the_view_changed_shape_is_stale() {
        let (_root, store, id) = fixture();
        for index in 0..5 {
            store.append_turn(&id, &turn(index), None).unwrap();
        }
        let opened = store.open_page(&id, 2).unwrap();
        // An append keeps the revision: older positions did not move.
        store.append_turn(&id, &turn(5), None).unwrap();
        assert!(matches!(
            store.page(&id, opened.start, 2, &opened.revision).unwrap(),
            PageRead::Page(_)
        ));
        assert!(matches!(
            store.page(&id, usize::MAX, 2, &opened.revision).unwrap(),
            PageRead::Stale
        ));
        append_records(
            &dir_of(&store, &id),
            // Longer than before, so only the revision tells the shape changed.
            &[record(
                RecordKind::Trim,
                "later",
                (10..20).flat_map(turn).collect(),
            )],
        )
        .unwrap();
        assert_eq!(
            store.page(&id, opened.start, 2, &opened.revision).unwrap(),
            PageRead::Stale
        );
    }

    #[test]
    fn a_page_stops_at_its_byte_bound_but_keeps_one_turn() {
        let big = "x".repeat(PAGE_BYTES);
        let view = [
            vec![user("a"), answer(&big)],
            vec![user("b"), answer(&big)],
            vec![user("c"), answer("small")],
        ]
        .concat();
        assert_eq!(
            page_start(&view, view.len(), 64),
            2,
            "c, then b passes the bound"
        );
        assert_eq!(page_start(&view, 2, 64), 0);
        assert_eq!(
            page_start(&view, 4, 64),
            2,
            "one huge turn still comes whole"
        );
    }

    #[test]
    fn the_panel_estimate_matches_the_composer_count() {
        let messages = [
            json!({"content":"abcd"}),
            json!({"content":"\u{4e2d}\u{6587}"}),
            json!({}),
        ];
        // "abcd\n" + "\n" -> 6 narrow -> 2 tokens; two wide -> 2 tokens.
        assert_eq!(panel_tokens(&messages), 4);
        assert_eq!(panel_tokens(&[]), 0);
    }
}
