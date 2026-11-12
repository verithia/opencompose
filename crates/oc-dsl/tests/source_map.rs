//! Source identity follows authored occurrences rather than display names.
use oc_dsl::{
    diagnostics::{DiagnosticLocation, DiagnosticSubject},
    parse_document,
    source_map::{
        ByteSpan, DocumentParseError, ParsedDocument, ProjectField, ReferenceKind, SourceLimits,
        SourceMapError, SourceOwner, SourcePart, SourceSubject, Utf16Position, Utf16Range,
    },
};

fn subject(owner: SourceOwner, part: SourcePart) -> SourceSubject {
    SourceSubject { owner, part }
}
fn text(document: &ParsedDocument, owner: SourceOwner, part: SourcePart) -> &str {
    document
        .span(subject(owner, part))
        .unwrap()
        .text(document.source())
        .unwrap()
}
fn reference(kind: ReferenceKind, occurrence: usize) -> SourcePart {
    SourcePart::Reference { kind, occurrence }
}

#[test]
fn duplicate_and_quoted_names_keep_distinct_authored_occurrences() {
    let source = "// @track ghost | preset: wrong\n@project \"😀\"\n@preset \"p.a:😀\"\n  extends: absent\n@preset \"p.a:😀\"\n@track \"中😀\" | preset: \"p.a:😀\"\n@track \"中😀\" | preset: absent\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    assert_eq!(document.project(), &oc_dsl::parse(source).unwrap());
    assert_eq!(
        text(&document, SourceOwner::Preset(0), SourcePart::Name),
        "\"p.a:😀\""
    );
    assert_eq!(
        text(
            &document,
            SourceOwner::Track(1),
            reference(ReferenceKind::Preset, 0)
        ),
        "absent"
    );
    assert_eq!(
        text(
            &document,
            SourceOwner::Preset(0),
            reference(ReferenceKind::ParentPreset, 0)
        ),
        "absent"
    );
    for (owner, line) in [
        (SourceOwner::Preset(0), 2),
        (SourceOwner::Preset(1), 4),
        (SourceOwner::Track(0), 5),
        (SourceOwner::Track(1), 6),
    ] {
        let span = document.span(subject(owner, SourcePart::Name)).unwrap();
        assert_eq!(document.utf16_range(span).unwrap().start.line, line);
    }
}

#[test]
fn unicode_utf16_positions_respect_crlf_combining_marks_and_eof() {
    let source = "@project \"中😀e\u{301}\"\r\n@track \"🎹\" | preset: absent\r\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    let span = document
        .span(subject(
            SourceOwner::Track(0),
            reference(ReferenceKind::Preset, 0),
        ))
        .unwrap();
    assert_eq!(
        document.utf16_range(span).unwrap(),
        Utf16Range {
            start: Utf16Position {
                line: 1,
                character: 22
            },
            end: Utf16Position {
                line: 1,
                character: 28
            }
        }
    );
    let eof = ByteSpan::checked(source, source.len(), source.len()).unwrap();
    assert_eq!(
        document.utf16_range(eof).unwrap().start,
        Utf16Position {
            line: 2,
            character: 0
        }
    );
    assert!(matches!(
        ByteSpan::checked(source, 11, 12),
        Err(SourceMapError::InvalidSpan)
    ));
    assert!(ByteSpan::checked(source, 5, 2).is_err());
    assert!(ByteSpan::checked(source, 0, source.len() + 1).is_err());
}

#[test]
fn long_unicode_lines_use_exact_positions_across_sparse_checkpoints() {
    let prefix = "😀中".repeat(1000);
    let source = format!("@project \"{prefix}\"\n@track t | preset: missing");
    let document = parse_document(&source, SourceLimits::default()).unwrap();
    let span = document
        .span(subject(SourceOwner::Project, SourcePart::Name))
        .unwrap();
    assert_eq!(document.utf16_range(span).unwrap().end.character, 3011);
    let span = document
        .span(subject(
            SourceOwner::Track(0),
            reference(ReferenceKind::Preset, 0),
        ))
        .unwrap();
    for _ in 0..1000 {
        assert_eq!(
            document.utf16_range(span).unwrap().start,
            Utf16Position {
                line: 1,
                character: 19
            }
        );
    }
}

#[test]
fn audio_relocation_keeps_document_identity_and_actual_first_matching_track() {
    let source = "@project\n@audio_clip \"a😀\" [1:1 - 2:1] | track: b\n  file: \"a.wav\"\n@audio_clip second [1:1 - 2:1] | track: a\n  file: \"b.wav\"\n@track a\n@track b\n@track b\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    assert_eq!(document.project(), &oc_dsl::parse(source).unwrap());
    assert_eq!(document.source_map().audio_clip_track(0), Some(1));
    assert_eq!(document.source_map().audio_clip_track(1), Some(0));
    assert_eq!(document.source_map().audio_clip_track(2), None);
    assert_eq!(
        text(&document, SourceOwner::AudioClip(0), SourcePart::Name),
        "\"a😀\""
    );
    assert_eq!(
        text(
            &document,
            SourceOwner::AudioClip(1),
            reference(ReferenceKind::Track, 0)
        ),
        "a"
    );
    assert_eq!(document.project().tracks[1].clips.audio_count(), 1);
    assert_eq!(document.project().tracks[2].clips.audio_count(), 0);
}

#[test]
fn kit_lowering_shares_the_actual_preset_index_without_name_search() {
    let source = "@project\n@preset same\n@kit same\n  pad kick | note: C2\n@preset final\n@pattern riff | preset: same\n  events:\n    C4 q mf\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    assert_eq!(
        document.project().presets[1].plugin.as_deref(),
        Some(oc_dsl::DRUMKIT_PLUGIN_ID)
    );
    assert!(text(&document, SourceOwner::Preset(1), SourcePart::Whole).starts_with("@kit"));
    assert!(
        text(&document, SourceOwner::Preset(2), SourcePart::Whole).starts_with("@preset final")
    );
    assert_eq!(
        text(
            &document,
            SourceOwner::Pattern(0),
            reference(ReferenceKind::Preset, 0)
        ),
        "same"
    );
}

#[test]
fn effective_property_spans_follow_parser_assignment_and_preserve_versions() {
    let source = "@ocp \"0.2.0\"\n@project\n  tempo: 90\n  tempo: 130\n@master\n  effects:\n    - reverb: mix=20%\n@master\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    assert_eq!(document.project().properties.tempo, 130.0);
    assert_eq!(
        text(
            &document,
            SourceOwner::Project,
            SourcePart::ProjectField(ProjectField::Tempo)
        ),
        "tempo: 130"
    );
    assert_eq!(
        text(&document, SourceOwner::Project, SourcePart::Version),
        "\"0.2.0\""
    );
    assert_eq!(document.project().ocp_version.as_deref(), Some("0.2.0"));
    let master = document
        .span(subject(SourceOwner::Master, SourcePart::Whole))
        .unwrap();
    assert_eq!(master.start(), source.rfind("@master").unwrap());
}

#[test]
fn immutable_snapshot_owns_source_after_the_caller_changes_its_input() {
    let mut input = String::from("@project \"original\"\n@track t\n");
    let document = parse_document(&input, SourceLimits::default()).unwrap();
    input.clear();
    input.push_str("unrelated");
    assert_eq!(
        text(&document, SourceOwner::Project, SourcePart::Name),
        "\"original\""
    );
    let mut project = document.into_project();
    project.name = Some("edited".into());
    assert_eq!(project.name.as_deref(), Some("edited"));
}

#[test]
fn absent_mapping_is_distinct_from_a_document_level_diagnostic() {
    let document = parse_document("@project\n", SourceLimits::default()).unwrap();
    let absent = subject(SourceOwner::Track(9), SourcePart::Name);
    assert_eq!(
        DiagnosticSubject::Located(absent).resolve(&document),
        DiagnosticLocation::Unmapped(absent)
    );
    assert_eq!(
        DiagnosticSubject::Unmapped(absent).resolve(&document),
        DiagnosticLocation::Unmapped(absent)
    );
    assert_eq!(
        DiagnosticSubject::Document.resolve(&document),
        DiagnosticLocation::Document
    );
    assert_eq!(
        DiagnosticSubject::NoSource.resolve(&document),
        DiagnosticLocation::NoSource
    );
}

#[test]
fn source_and_each_sidecar_budget_refuse_without_publishing_a_snapshot() {
    let source = "@project\n@track t\n";
    let limits = SourceLimits {
        document: oc_dsl::diff::compressed::CodecLimits {
            max_decoded_bytes: source.len() - 1,
            ..SourceLimits::default().document
        },
        ..SourceLimits::default()
    };
    assert!(matches!(
        parse_document(source, limits),
        Err(DocumentParseError::Source(SourceMapError::SourceLimit))
    ));
    for limits in [
        SourceLimits {
            max_map_entries: 1,
            ..SourceLimits::default()
        },
        SourceLimits {
            max_parse_nodes: 1,
            ..SourceLimits::default()
        },
        SourceLimits {
            max_parse_depth: 0,
            ..SourceLimits::default()
        },
        SourceLimits {
            max_index_entries: 1,
            ..SourceLimits::default()
        },
    ] {
        assert!(matches!(
            parse_document(source, limits),
            Err(DocumentParseError::Source(SourceMapError::Budget(_)))
        ));
    }
    assert!(parse_document(source, SourceLimits::default()).is_ok());
    assert!(oc_dsl::parse(source).is_ok());
}

#[test]
fn root_references_keep_appended_routes_and_vocal_selector_identity() {
    let source = "@project\n@track t\n@bus b\n  routes: [first, second]\n  routes: [third]\n@group g { members: one, two }\n@vocal_tune vt | track: t\n@vocal_harmony vh | clip: absent\n@control_source envelope | from: track(t), attack: 5ms, release: 100ms\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    assert_eq!(
        document.project().buses[0].routes,
        ["first", "second", "third"]
    );
    for (index, name) in ["first", "second", "third"].into_iter().enumerate() {
        assert_eq!(
            text(
                &document,
                SourceOwner::Bus(0),
                reference(ReferenceKind::BusRoute, index)
            ),
            name
        );
    }
    assert_eq!(
        text(
            &document,
            SourceOwner::Group(0),
            reference(ReferenceKind::GroupMember, 1)
        ),
        "two"
    );
    assert_eq!(
        text(
            &document,
            SourceOwner::VocalTune(0),
            reference(ReferenceKind::Track, 0)
        ),
        "t"
    );
    assert_eq!(
        text(
            &document,
            SourceOwner::VocalHarmony(0),
            reference(ReferenceKind::Clip, 0)
        ),
        "absent"
    );
    assert_eq!(
        text(
            &document,
            SourceOwner::ControlSource(0),
            reference(ReferenceKind::ControlTap, 0)
        ),
        "t"
    );
}

#[test]
fn all_grammar_line_separators_share_utf16_and_eof_positions() {
    for separator in ["\n", "\r", "\r\n"] {
        let source = format!("@project{separator}@track t{separator}");
        let document = parse_document(&source, SourceLimits::default()).unwrap();
        let name = document
            .span(subject(SourceOwner::Track(0), SourcePart::Name))
            .unwrap();
        assert_eq!(
            document.utf16_range(name).unwrap(),
            Utf16Range {
                start: Utf16Position {
                    line: 1,
                    character: 7
                },
                end: Utf16Position {
                    line: 1,
                    character: 8
                },
            },
            "{separator:?}"
        );
        let eof = ByteSpan::checked(&source, source.len(), source.len()).unwrap();
        assert_eq!(
            document.utf16_range(eof).unwrap().start,
            Utf16Position {
                line: 2,
                character: 0
            }
        );
        let no_final_separator = source.strip_suffix(separator).unwrap();
        let document = parse_document(no_final_separator, SourceLimits::default()).unwrap();
        let eof = ByteSpan::checked(
            no_final_separator,
            no_final_separator.len(),
            no_final_separator.len(),
        )
        .unwrap();
        assert_eq!(
            document.utf16_range(eof).unwrap().start,
            Utf16Position {
                line: 1,
                character: 8
            }
        );
    }
}

#[test]
fn separator_boundaries_are_consistent_at_and_across_sparse_checkpoints() {
    // The closing quote is scalar 63 or 64; CR then lands on either side of
    // the checkpoint, exercising both stored positions and sparse replay.
    for count in [52, 53, 54] {
        for separator in ["\n", "\r", "\r\n"] {
            let first_line = format!("@project \"{}\"", "😀".repeat(count));
            let source = format!("{first_line}{separator}@track t");
            let document = parse_document(&source, SourceLimits::default()).unwrap();
            let end = Utf16Position {
                line: 0,
                character: 11 + u32::try_from(count).unwrap() * 2,
            };
            let boundary = |byte| {
                document
                    .utf16_range(ByteSpan::checked(&source, byte, byte).unwrap())
                    .unwrap()
                    .start
            };
            assert_eq!(boundary(first_line.len()), end);
            if separator == "\r\n" {
                assert_eq!(boundary(first_line.len() + 1), end);
            }
            assert_eq!(
                boundary(first_line.len() + separator.len()),
                Utf16Position {
                    line: 1,
                    character: 0
                }
            );
            let name = document
                .span(subject(SourceOwner::Track(0), SourcePart::Name))
                .unwrap();
            assert_eq!(
                document.utf16_range(name).unwrap().start,
                Utf16Position {
                    line: 1,
                    character: 7
                }
            );
        }
    }
}

#[test]
fn validation_duplicates_locate_each_offending_occurrence_in_all_root_classes() {
    use oc_dsl::{diagnostics::DiagnosticSubject, error::ValidationError};
    type Case = (&'static str, fn(usize) -> SourceOwner, usize, usize);
    let cases: [Case; 6] = [
        ("@preset dup\n", SourceOwner::Preset, 0, 1),
        ("@track dup\n", SourceOwner::Track, 1, 1),
        ("@bus dup\n", SourceOwner::Bus, 0, 1),
        (
            "@pattern dup\n  events:\n    C4 q mf\n",
            SourceOwner::Pattern,
            0,
            3,
        ),
        (
            "@audio_clip dup [1:1 - 2:1] | track: owner\n  file: \"sample.wav\"\n",
            SourceOwner::AudioClip,
            0,
            2,
        ),
        (
            "@control_source dup | from: track(owner), attack: 1ms, release: 100ms\n",
            SourceOwner::ControlSource,
            0,
            1,
        ),
    ];
    for (block, owner, base, block_lines) in cases {
        let source = format!(
            "// dup in a comment\n@project \"dup\"\n@track owner\n{}",
            block.repeat(3)
        );
        let document = parse_document(&source, SourceLimits::default()).unwrap();
        let diagnostics = document.validation_diagnostics();
        let legacy = oc_dsl::validate(document.project()).unwrap_err();
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| format!("{:?}", diagnostic.error))
                .collect::<Vec<_>>(),
            legacy
                .iter()
                .map(|error| format!("{error:?}"))
                .collect::<Vec<_>>()
        );
        let duplicates: Vec<_> = diagnostics
            .iter()
            .filter(|diagnostic| {
                matches!(
                    diagnostic.error,
                    ValidationError::DuplicatePreset { .. }
                        | ValidationError::DuplicateTrack { .. }
                        | ValidationError::DuplicateBus { .. }
                        | ValidationError::DuplicatePattern { .. }
                        | ValidationError::DuplicateAudioClip { .. }
                        | ValidationError::DuplicateControlSource { .. }
                )
            })
            .collect();
        assert_eq!(duplicates.len(), 2, "{block}");
        for (duplicate, occurrence) in duplicates.into_iter().zip([1, 2]) {
            assert_eq!(
                duplicate.subject,
                DiagnosticSubject::Located(subject(owner(base + occurrence), SourcePart::Name))
            );
            let span = document
                .span(subject(owner(base + occurrence), SourcePart::Name))
                .unwrap();
            assert_eq!(span.text(&source), Some("dup"));
            assert_eq!(
                document.utf16_range(span).unwrap().start.line as usize,
                3 + occurrence * block_lines
            );
        }
    }
}

#[test]
fn typed_parent_and_track_reference_errors_preserve_unknown_legacy_checks_and_order() {
    use oc_dsl::{diagnostics::DiagnosticSubject, error::ValidationError};
    let source = "@project\n  tempo: 0\n@preset \"p:😀\"\n  extends: absent\n@preset \"p:😀\"\n  extends: absent\n@track \"t.😀\" | preset: absent\n@track \"t.😀\" | preset: absent\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    let diagnostics = document.validation_diagnostics();
    let legacy = oc_dsl::validate(document.project()).unwrap_err();
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| format!("{:?}", diagnostic.error))
            .collect::<Vec<_>>(),
        legacy
            .iter()
            .map(|error| format!("{error:?}"))
            .collect::<Vec<_>>()
    );
    let classes: Vec<_> = diagnostics
        .iter()
        .map(|diagnostic| match &diagnostic.error {
            ValidationError::OutOfRange { field, .. } if field == "tempo" => "tempo",
            ValidationError::InvalidValue { field, .. } if field == "project.clock" => "clock",
            ValidationError::InvalidValue { field, .. } if field == "preset.extends" => "resolver",
            ValidationError::DuplicatePreset { .. } => "duplicate_preset",
            ValidationError::DuplicateTrack { .. } => "duplicate_track",
            ValidationError::UnknownPreset { .. } => "reference",
            error => panic!("unexpected semantic error: {error:?}"),
        })
        .collect();
    assert_eq!(
        classes,
        [
            "tempo",
            "clock",
            "resolver",
            "duplicate_preset",
            "duplicate_track",
            "reference",
            "reference",
            "reference",
            "reference"
        ]
    );
    let references: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.error, ValidationError::UnknownPreset { .. }))
        .collect();
    assert_eq!(references.len(), 4);
    let expected = [
        subject(
            SourceOwner::Preset(0),
            reference(ReferenceKind::ParentPreset, 0),
        ),
        subject(
            SourceOwner::Preset(1),
            reference(ReferenceKind::ParentPreset, 0),
        ),
        subject(SourceOwner::Track(0), reference(ReferenceKind::Preset, 0)),
        subject(SourceOwner::Track(1), reference(ReferenceKind::Preset, 0)),
    ];
    for (diagnostic, expected) in references.into_iter().zip(expected) {
        assert_eq!(diagnostic.subject, DiagnosticSubject::Located(expected));
        assert_eq!(
            document.span(expected).unwrap().text(source),
            Some("absent")
        );
    }
    let resolver = diagnostics.iter().find(|diagnostic| matches!(&diagnostic.error, ValidationError::InvalidValue { field, .. } if field == "preset.extends")).unwrap();
    let resolver_subject = subject(
        SourceOwner::Preset(0),
        reference(ReferenceKind::ParentPreset, 0),
    );
    assert_eq!(
        resolver.subject,
        DiagnosticSubject::Located(resolver_subject)
    );
    assert_eq!(
        resolver.subject.resolve(&document),
        DiagnosticLocation::Span(document.span(resolver_subject).unwrap())
    );
    assert_eq!(diagnostics[0].subject, DiagnosticSubject::UnknownSubject);
}

#[test]
fn lowered_kit_and_relocated_audio_validation_keep_actual_origin_identity() {
    use oc_dsl::{diagnostics::DiagnosticSubject, error::ValidationError};
    let source = "@project\n@preset same\n@kit same\n  pad kick | note: C2\n@audio_clip a [1:1 - 2:1] | track: b\n  file: \"a.wav\"\n@audio_clip a [1:1 - 2:1] | track: t\n  file: \"a.wav\"\n@track t\n@track b\n";
    let document = parse_document(source, SourceLimits::default()).unwrap();
    let diagnostics = document.validation_diagnostics();
    let preset = diagnostics
        .iter()
        .find(|diagnostic| matches!(diagnostic.error, ValidationError::DuplicatePreset { .. }))
        .unwrap();
    assert_eq!(
        preset.subject,
        DiagnosticSubject::Located(subject(SourceOwner::Preset(1), SourcePart::Name))
    );
    let audio = diagnostics
        .iter()
        .find(|diagnostic| matches!(diagnostic.error, ValidationError::DuplicateAudioClip { .. }))
        .unwrap();
    assert_eq!(
        audio.subject,
        DiagnosticSubject::Located(subject(SourceOwner::AudioClip(1), SourcePart::Name))
    );
    let span = document
        .span(subject(SourceOwner::AudioClip(1), SourcePart::Name))
        .unwrap();
    assert_eq!(document.utf16_range(span).unwrap().start.line, 6);
}

#[test]
fn root_reference_diagnostics_follow_pattern_control_and_relocated_audio_occurrences() {
    use oc_dsl::error::ValidationError;
    let lines = [
        "@project",
        "  tempo: 0",
        "@pattern \"p😀\" | preset: absent, instrument: synth/lead",
        "  events:",
        "    @0 C4 q",
        "@pattern \"p😀\" | preset: absent, instrument: synth/lead",
        "  events:",
        "    @0 D4 q",
        "@control_source \"env😀\" | from: track(ignored), from: track(absent), attack: 5ms, release: 100ms",
        "@control_source \"env😀\" | from: track(absent), attack: 5ms, release: 100ms",
        "@audio_clip same [1:1 - 2:1] | track: b",
        "  file: \"a.wav\"",
        "@audio_clip same [1:1 - 2:1] | track: a",
        "  file: \"b.wav\"",
        "@track a",
        "@track b",
    ];
    for separator in ["\n", "\r", "\r\n"] {
        let source = lines.join(separator);
        let document = parse_document(&source, SourceLimits::default()).unwrap();
        let diagnostics = document.validation_diagnostics();
        let legacy = oc_dsl::validate(document.project()).unwrap_err();
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| format!("{:?}", diagnostic.error))
                .collect::<Vec<_>>(),
            legacy
                .iter()
                .map(|error| format!("{error:?}"))
                .collect::<Vec<_>>()
        );
        for (class, expected) in [
            (
                0,
                vec![
                    (SourceOwner::Pattern(0), ReferenceKind::Preset, 2usize),
                    (SourceOwner::Pattern(1), ReferenceKind::Preset, 5),
                ],
            ),
            (
                1,
                vec![
                    (SourceOwner::ControlSource(0), ReferenceKind::ControlTap, 8),
                    (SourceOwner::ControlSource(1), ReferenceKind::ControlTap, 9),
                ],
            ),
        ] {
            let selected: Vec<_> = diagnostics
                .iter()
                .filter(|diagnostic| match diagnostic.error {
                    ValidationError::UnknownPreset { .. } => class == 0,
                    ValidationError::UnknownControlSourceTrack { .. } => class == 1,
                    _ => false,
                })
                .collect();
            assert_eq!(selected.len(), expected.len());
            for (diagnostic, (owner, kind, line)) in selected.into_iter().zip(expected) {
                let target = subject(owner, reference(kind, 0));
                assert_eq!(diagnostic.subject, DiagnosticSubject::Located(target));
                let span = document.span(target).unwrap();
                assert_eq!(span.text(&source), Some("absent"));
                let column = lines[line][..lines[line].rfind("absent").unwrap()]
                    .encode_utf16()
                    .count() as u32;
                assert_eq!(
                    document.utf16_range(span).unwrap(),
                    Utf16Range {
                        start: Utf16Position {
                            line: line as u32,
                            character: column
                        },
                        end: Utf16Position {
                            line: line as u32,
                            character: column + 6
                        }
                    }
                );
            }
        }
        // The parser refuses absent audio owners before a text snapshot exists;
        // valid relocated references still establish canonical clip identity.
        assert_eq!(document.source_map().audio_clip_track(0), Some(1));
        assert_eq!(document.source_map().audio_clip_track(1), Some(0));
        assert_eq!(
            text(
                &document,
                SourceOwner::AudioClip(0),
                reference(ReferenceKind::Track, 0)
            ),
            "b"
        );
        assert_eq!(
            text(
                &document,
                SourceOwner::AudioClip(1),
                reference(ReferenceKind::Track, 0)
            ),
            "a"
        );
        assert!(diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.error,
            ValidationError::OutOfRange { .. }
        ) && diagnostic.subject
            == DiagnosticSubject::UnknownSubject));
    }
}

#[test]
fn preset_resolution_diagnostics_locate_actual_failed_edges_without_changing_legacy_errors() {
    use oc_dsl::error::ValidationError;
    let cases: &[(&[&str], usize, usize)] = &[
        (&["@project", "@preset \"a😀\"", "  extends: \"a😀\""], 0, 2),
        (
            &[
                "@project",
                "@preset \"a😀\"",
                "  extends: \"b😀\"",
                "@preset \"b😀\"",
                "  extends: \"a😀\"",
            ],
            1,
            4,
        ),
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
            2,
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
            1,
            4,
        ),
        (
            &[
                "@project",
                "@preset same",
                "  extends: absent",
                "@preset same",
                "  extends: different",
            ],
            0,
            2,
        ),
        (&["@project", "@preset p", "  extends: \"\""], 0, 2),
    ];
    for separator in ["\n", "\r", "\r\n"] {
        for &(lines, index, line) in cases {
            let source = lines.join(separator);
            let document = parse_document(&source, SourceLimits::default()).unwrap();
            let diagnostics = document.validation_diagnostics();
            let legacy = oc_dsl::validate(document.project()).unwrap_err();
            assert_eq!(
                diagnostics
                    .iter()
                    .map(|item| format!("{:?}", item.error))
                    .collect::<Vec<_>>(),
                legacy
                    .iter()
                    .map(|error| format!("{error:?}"))
                    .collect::<Vec<_>>()
            );
            let diagnostic=diagnostics.iter().find(|item|matches!(&item.error,ValidationError::InvalidValue {field,..} if field=="preset.extends")).unwrap();
            let expected = subject(
                SourceOwner::Preset(index),
                reference(ReferenceKind::ParentPreset, 0),
            );
            assert_eq!(diagnostic.subject, DiagnosticSubject::Located(expected));
            let span = document.span(expected).unwrap();
            let token = lines[line].strip_prefix("  extends: ").unwrap();
            assert_eq!(span.text(&source), Some(token));
            assert_eq!(
                document.utf16_range(span).unwrap(),
                Utf16Range {
                    start: Utf16Position {
                        line: line as u32,
                        character: 11
                    },
                    end: Utf16Position {
                        line: line as u32,
                        character: 11 + token.encode_utf16().count() as u32
                    }
                }
            );
            assert_eq!(
                oc_dsl::preset::resolve(document.project()).unwrap_err(),
                match &diagnostic.error {
                    ValidationError::InvalidValue { value, .. } => value.clone(),
                    _ => unreachable!(),
                }
            );
        }
    }
}
