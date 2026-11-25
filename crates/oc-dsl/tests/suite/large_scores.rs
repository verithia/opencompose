use oc_dsl::error::ParseError;
use oc_dsl::source_map::SourceLimits;

fn score(notes: usize, named_pattern: bool) -> String {
    let mut source = String::from("@project \"Score 🎵\"\r\n");
    if named_pattern {
        source.push_str("@pattern melody\r\n  events:\r\n");
    } else {
        source.push_str("@track melody\r\n  @phrase theme [1:1 - 2049:1]\r\n");
    }
    for index in 0..notes {
        source.push_str(&format!("    @{index} C4 q v=100\r\n"));
    }
    source
}

#[test]
fn large_phrase_and_pattern_scores_preserve_every_note() {
    for named_pattern in [false, true] {
        let source = score(4096, named_pattern);
        let document = oc_dsl::parse_document(&source, SourceLimits::default()).unwrap();
        let project = document.project();
        let events = if named_pattern {
            &project.patterns[0].events
        } else {
            &project.tracks[0].phrases[0].events
        };
        assert_eq!(events.len(), 4096);
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.start_beat, index as f64);
            assert_eq!(event.duration.to_beats(), 1.0);
            assert_eq!(event.velocity, Some(100));
        }
    }
}

#[test]
fn late_note_refusals_keep_unicode_columns_and_crlf_lines() {
    for named_pattern in [false, true] {
        let mut source = score(256, named_pattern);
        let line = source.lines().count() + 1;
        let bad_note = "    @256 C4 q text=\"🎵\", C4 q";
        let col = bad_note[..bad_note.rfind("C4").unwrap()].chars().count() + 1;
        source.push_str(bad_note);
        let ParseError::Syntax {
            line: actual_line,
            col: actual_col,
            message,
        } = oc_dsl::parse(&source).unwrap_err()
        else {
            panic!("a repeated comma-list pitch must carry its source position");
        };
        assert_eq!((actual_line, actual_col), (line, col));
        assert!(message.contains("comma list repeats pitch"), "{message}");
    }
}

#[test]
fn late_trailing_commas_still_name_the_previous_note_line() {
    for named_pattern in [false, true] {
        let mut source = score(256, named_pattern);
        let line = source.lines().count() + 1;
        let note = "    @256 C4 q,";
        source.push_str(note);
        let ParseError::Syntax {
            line: actual_line,
            col: actual_col,
            message,
        } = oc_dsl::parse(&source).unwrap_err()
        else {
            panic!("a trailing comma must carry its source position");
        };
        assert_eq!((actual_line, actual_col), (line, note.len()));
        assert!(message.contains("trailing comma"), "{message}");
    }
}

#[cfg(feature = "perf-tests")]
#[test]
fn growing_scores_do_not_rescan_the_document_for_each_note_location() {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    fn median_parse(source: &str) -> Duration {
        let mut times = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            black_box(oc_dsl::parse_document(black_box(source), SourceLimits::default()).unwrap());
            times.push(start.elapsed());
        }
        times.sort_unstable();
        times[2]
    }

    for named_pattern in [false, true] {
        let small = score(1024, named_pattern);
        let large = score(4096, named_pattern);
        // Warm code and allocator paths for both sizes; the budget compares
        // work growth on one machine rather than imposing a CPU speed.
        black_box(oc_dsl::parse_document(&small, SourceLimits::default()).unwrap());
        black_box(oc_dsl::parse_document(&large, SourceLimits::default()).unwrap());
        let small_time = median_parse(&small);
        let large_time = median_parse(&large);
        assert!(
            large_time <= small_time * 10,
            "4x notes took {:.2}x the parse time ({small_time:?} -> {large_time:?}, named_pattern={named_pattern})",
            large_time.as_secs_f64() / small_time.as_secs_f64(),
        );
    }
}
