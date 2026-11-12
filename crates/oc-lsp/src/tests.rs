use super::*;

#[test]
fn poisoned_document_read_recovers_for_the_next_completion() {
    let source = "instrument: ";
    let documents: Mutex<HashMap<String, DocumentState>> = Mutex::new(HashMap::from([(
        "file:///test.ocp".into(),
        DocumentState {
            source: source.into(),
            project: None,
            errors: Vec::new(),
        },
    )]));
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let guard = lock_documents(&documents);
            assert_eq!(guard["file:///test.ocp"].source, source);
            panic!("simulate a document reader panic");
        });
        assert!(reader.join().is_err());
    });
    assert!(documents.is_poisoned());
    {
        let guard = lock_documents(&documents);
        let document = &guard["file:///test.ocp"];
        let items = completion::complete(
            &document.source,
            &document.project,
            Position::new(0, source.encode_utf16().count() as u32),
        );
        assert!(
            items
                .iter()
                .any(|item| item.label == "piano/acoustic_grand")
        );
    }
    assert!(!documents.is_poisoned());
    assert_eq!(lock_documents(&documents).len(), 1);
}

#[test]
fn advertises_utf16_and_push_diagnostics_only() {
    let capabilities = server_capabilities();
    assert_eq!(
        capabilities.position_encoding,
        Some(PositionEncodingKind::UTF16)
    );
    assert!(capabilities.diagnostic_provider.is_none());
    assert!(capabilities.completion_provider.is_some());
    assert!(capabilities.hover_provider.is_some());
}

#[test]
fn unicode_completion_does_not_poison_the_document_lock() {
    let source = "中😀 instrument: ";
    let document = Mutex::new(source);
    {
        let source = document.lock().unwrap();
        let end = source.encode_utf16().count() as u32;
        let items = completion::complete(&source, &None, Position::new(0, end));
        assert!(
            items
                .iter()
                .any(|item| item.label == "piano/acoustic_grand")
        );
        for character in 0..=end + 1 {
            completion::complete(&source, &None, Position::new(0, character));
            hover::hover_info(&source, &None, Position::new(0, character));
        }
        assert!(completion::complete(&source, &None, Position::new(0, 2)).is_empty());
        assert!(completion::complete(&source, &None, Position::new(u32::MAX, 0)).is_empty());
        assert!(hover::hover_info(&source, &None, Position::new(u32::MAX, 0)).is_none());
    }
    assert!(document.lock().is_ok());
}

#[test]
fn unicode_hover_resolves_the_token_after_multibyte_text() {
    let source = "// 中😀 tempo";
    let character = source[..source.find("tempo").unwrap()]
        .encode_utf16()
        .count() as u32;
    let hover = hover::hover_info(source, &None, Position::new(0, character)).unwrap();
    let HoverContents::Markup(contents) = hover.contents else {
        panic!("expected markdown");
    };
    assert!(contents.value.starts_with("**tempo:**"));
    assert!(hover::hover_info(source, &None, Position::new(0, u32::MAX)).is_none());
    assert!(!completion::complete("@project\n", &None, Position::new(1, 0)).is_empty());
}

#[test]
fn syntax_diagnostic_columns_are_utf16_and_stay_within_the_line() {
    let source = "@project \"中😀\" !\n";
    let (project, diagnostics) = diagnostics::parse_and_diagnose(source);
    assert!(project.is_none());
    assert_eq!(diagnostics.len(), 1);
    let start = source[..source.find('!').unwrap()].encode_utf16().count() as u32;
    assert_eq!(
        diagnostics[0].range,
        Range::new(Position::new(0, start), Position::new(0, start + 1))
    );
}

fn assert_owner_parity(source: &str, valid: bool) {
    let project = oc_dsl::parse(source).unwrap();
    let expected: Vec<_> = oc_dsl::validate(&project)
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|error| error.to_string())
        .collect();
    let report = validate_project(&project);
    assert_eq!(report.valid, valid, "{expected:?}");
    assert_eq!(
        report
            .errors
            .iter()
            .map(|error| error.message.clone())
            .collect::<Vec<_>>(),
        expected
    );
    let (parsed, diagnostics) = diagnostics::parse_and_diagnose(source);
    assert!(parsed.is_some());
    assert_eq!(diagnostics.len(), expected.len());
    let owner =
        oc_dsl::parse_document(source, oc_dsl::source_map::SourceLimits::default()).unwrap();
    let located = owner.validation_diagnostics();
    for ((diagnostic, message), owning) in diagnostics.iter().zip(expected).zip(located) {
        assert!(diagnostic.message.ends_with(&message));
        assert_eq!(diagnostic.severity, DiagnosticSeverity::ERROR);
        match owning.subject.resolve(&owner) {
            oc_dsl::diagnostics::DiagnosticLocation::Span(span) => {
                let range = owner.utf16_range(span).unwrap();
                assert_eq!(
                    diagnostic.range,
                    Range::new(
                        Position::new(range.start.line, range.start.character),
                        Position::new(range.end.line, range.end.character)
                    )
                );
                assert_eq!(diagnostic.location, diagnostics::LocationStatus::Located);
            }
            oc_dsl::diagnostics::DiagnosticLocation::UnknownSubject => {
                assert_eq!(diagnostic.range, Range::default());
                assert_eq!(
                    diagnostic.location,
                    diagnostics::LocationStatus::UnknownSubject
                );
            }
            other => panic!("unexpected owner location: {other:?}"),
        }
    }
}

#[test]
fn diagnostics_share_the_owners_chord_partial_bar_and_meter_semantics() {
    for (signature, range, notes) in [
        ("4/4", "1:2 - 1:4", "[C4 E4 G4] h"),
        ("6/8", "1:1 - 2:1", "[C4 E4 G4] h\n    D4 q"),
        ("4/4", "1:1 - 2:1", "C4 q"),
    ] {
        let source = format!(
            "@project\n  time_signature: {signature}\n@track keys | instrument: piano/acoustic_grand\n  @phrase p [{range}]\n    {notes}\n"
        );
        assert_owner_parity(&source, true);
    }
    assert_owner_parity(
        "@project\n  time_signature: 6/8\n@track keys | instrument: piano/acoustic_grand\n  @phrase p [1:2 - 1:4]\n    C4 h\n",
        false,
    );
}

#[test]
fn diagnostics_include_owner_checks_beyond_the_old_reference_subset() {
    assert_owner_parity("@project\n  tempo: 0\n", false);
    assert_owner_parity("@project\n@track keys | preset: missing\n", false);
}

#[test]
fn semantic_lsp_ranges_use_offending_unicode_occurrences_for_all_separators() {
    use diagnostics::LocationStatus;
    for separator in ["\n", "\r", "\r\n"] {
        let source = [
            "@project",
            "@track \"😀中\" | preset: absent",
            "@track \"😀中\" | preset: absent",
        ]
        .join(separator);
        let (project, errors) = diagnostics::parse_and_diagnose(&source);
        assert!(project.is_some());
        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors
                .iter()
                .map(|error| error.location)
                .collect::<Vec<_>>(),
            vec![LocationStatus::Located; 3]
        );
        assert_eq!(
            errors.iter().map(|error| error.range).collect::<Vec<_>>(),
            vec![
                Range::new(Position::new(2, 7), Position::new(2, 12)),
                Range::new(Position::new(1, 23), Position::new(1, 29)),
                Range::new(Position::new(2, 23), Position::new(2, 29)),
            ]
        );
        let published = protocol_diagnostic(&errors[0]);
        assert_eq!(
            published.data,
            Some(serde_json::json!({"source_location": "located"}))
        );
    }
}

#[test]
fn lsp_keeps_legacy_unknown_subjects_explicit_and_preserves_parent_diagnostics() {
    use diagnostics::LocationStatus;
    let source = "@project\n  tempo: 0\n@preset p\n  extends: absent\n";
    let (_, errors) = diagnostics::parse_and_diagnose(source);
    assert_eq!(
        errors.last().unwrap().range,
        Range::new(Position::new(3, 11), Position::new(3, 17))
    );
    assert_eq!(errors.last().unwrap().location, LocationStatus::Located);
    assert!(
        errors[..errors.len() - 2]
            .iter()
            .all(|error| error.location == LocationStatus::UnknownSubject)
    );
    assert_eq!(errors[errors.len() - 2].location, LocationStatus::Located);
    assert_eq!(errors[errors.len() - 2].range, errors.last().unwrap().range);
    assert_eq!(
        protocol_diagnostic(&errors[0]).data,
        Some(serde_json::json!({"source_location": "unknown_subject"}))
    );
    assert_owner_parity(source, false);
}

#[test]
fn lsp_admission_failure_is_a_document_constraint_and_publishes_no_ast() {
    let limits = oc_dsl::source_map::SourceLimits {
        max_map_entries: 0,
        ..Default::default()
    };
    let (project, errors) = diagnostics::parse_with_limits("@project\n", limits);
    assert!(project.is_none());
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].location, diagnostics::LocationStatus::Document);
    assert_eq!(
        protocol_diagnostic(&errors[0]).data,
        Some(serde_json::json!({"source_location": "document"}))
    );
}

#[tokio::test]
async fn document_update_never_caches_byte_rejected_text_or_keeps_its_old_ast() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    // An opaque cache key skips publication to an unconnected protocol client.
    let uri = "test-document";
    let small = "@project\n";
    let limits = oc_dsl::source_map::SourceLimits {
        document: oc_dsl::diff::compressed::CodecLimits {
            max_encoded_bytes: 32,
            max_decoded_bytes: 32,
        },
        ..Default::default()
    };
    backend
        .update_document_with_limits(uri, small, limits)
        .await;
    assert!(
        lock_documents(&backend.documents).cache[uri]
            .project
            .is_some()
    );
    let too_large = format!("@project \"{}\"\n", "x".repeat(40));
    let analysis = diagnostics::analyze_with_limits(&too_large, limits);
    assert!(matches!(
        analysis.cache,
        diagnostics::CacheAdmission::Rejected
    ));
    assert_eq!(analysis.errors.len(), 1);
    assert_eq!(
        analysis.errors[0].location,
        diagnostics::LocationStatus::Document
    );
    backend
        .update_document_with_limits(uri, &too_large, limits)
        .await;
    assert!(!lock_documents(&backend.documents).cache.contains_key(uri));
    backend
        .update_document_with_limits("new-document", &too_large, limits)
        .await;
    assert!(lock_documents(&backend.documents).cache.is_empty());
    backend
        .update_document_with_limits(uri, small, limits)
        .await;
    assert_eq!(lock_documents(&backend.documents).cache[uri].source, small);
}

#[tokio::test]
async fn document_update_retains_bounded_syntax_failure_text_without_a_stale_ast() {
    let (service, _socket) = LspService::new(Backend::new);
    let backend = service.inner();
    let uri = "test-document";
    let limits = oc_dsl::source_map::SourceLimits {
        document: oc_dsl::diff::compressed::CodecLimits {
            max_encoded_bytes: 32,
            max_decoded_bytes: 32,
        },
        ..Default::default()
    };
    backend
        .update_document_with_limits(uri, "@project\n", limits)
        .await;
    let malformed = "@project !\n";
    backend
        .update_document_with_limits(uri, malformed, limits)
        .await;
    let documents = lock_documents(&backend.documents);
    let cached = &documents.cache[uri];
    assert_eq!(cached.source, malformed);
    assert!(cached.project.is_none());
    assert_eq!(cached.errors.len(), 1);
    assert_eq!(
        cached.errors[0].location,
        diagnostics::LocationStatus::Located
    );
}

#[test]
fn root_reference_lsp_locations_use_actual_pattern_and_effective_control_tokens() {
    use diagnostics::LocationStatus;
    let lines = [
        "@project",
        "@pattern \"p😀\" | preset: absent, instrument: synth/lead",
        "  events:",
        "    @0 C4 q",
        "@control_source \"env😀\" | from: track(ignored), from: track(absent), attack: 5ms, release: 100ms",
    ];
    for separator in ["\n", "\r", "\r\n"] {
        let source = lines.join(separator);
        let (project, errors) = diagnostics::parse_and_diagnose(&source);
        assert!(project.is_some());
        assert_eq!(errors.len(), 2);
        for (error, line) in errors.iter().zip([4usize, 1usize]) {
            let column = lines[line][..lines[line].rfind("absent").unwrap()]
                .encode_utf16()
                .count() as u32;
            assert_eq!(
                error.range,
                Range::new(
                    Position::new(line as u32, column),
                    Position::new(line as u32, column + 6)
                )
            );
            assert_eq!(error.location, LocationStatus::Located);
            assert_eq!(
                protocol_diagnostic(error).data,
                Some(serde_json::json!({"source_location":"located"}))
            );
        }
        assert_owner_parity(&source, false);
    }
}

#[test]
fn root_reference_audio_missing_track_retains_parser_owned_syntax_location() {
    let source = "@project\n@audio_clip a [1:1 - 2:1] | track: absent\n  file: \"a.wav\"\n";
    assert!(matches!(
        oc_dsl::parse(source),
        Err(oc_dsl::error::ParseError::Syntax {
            line: 2,
            col: 1,
            ..
        })
    ));
    let (project, errors) = diagnostics::parse_and_diagnose(source);
    assert!(project.is_none());
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].range,
        Range::new(Position::new(1, 0), Position::new(1, 1))
    );
    assert_eq!(errors[0].location, diagnostics::LocationStatus::Located);
}

#[test]
fn preset_resolution_lsp_cycle_closure_and_dangling_edges_have_exact_utf16_ranges() {
    use diagnostics::LocationStatus;
    let cases: &[(&[&str], usize)] = &[
        (
            &[
                "@project",
                "@preset start",
                "  extends: \"a😀\"",
                "@preset \"a😀\"",
                "  extends: \"b😀\"",
                "@preset \"b😀\"",
                "  extends: \"a😀\"",
            ],
            6,
        ),
        (
            &[
                "@project",
                "@preset start",
                "  extends: \"p😀\"",
                "@preset \"p😀\"",
                "  extends: \"absent😀\"",
            ],
            4,
        ),
    ];
    for separator in ["\n", "\r", "\r\n"] {
        for &(lines, line) in cases {
            let source = lines.join(separator);
            let (_, errors) = diagnostics::parse_and_diagnose(&source);
            let token = lines[line].strip_prefix("  extends: ").unwrap();
            assert_eq!(errors[0].location, LocationStatus::Located);
            assert_eq!(
                errors[0].range,
                Range::new(
                    Position::new(line as u32, 11),
                    Position::new(line as u32, 11 + token.encode_utf16().count() as u32)
                )
            );
            assert_eq!(
                protocol_diagnostic(&errors[0]).data,
                Some(serde_json::json!({"source_location":"located"}))
            );
            assert_owner_parity(&source, false);
        }
    }
}
