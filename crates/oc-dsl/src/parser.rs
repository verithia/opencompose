//! Parser: OpenCompose DSL text → AST
//!
//! Uses pest PEG grammar. Each grammar rule has a corresponding `parse_xxx` function.
//!
//! ## Adding a new grammar construct
//! 1. Add rule to grammar.pest
//! 2. Add AST type in ast.rs
//! 3. Add `parse_xxx` function here
//! 4. Wire it into the parent parse function

use pest::Parser;
use pest_derive::Parser;

use crate::ast::*;
use crate::error::ParseError;

#[derive(Parser)]
#[grammar = "grammar.pest"]
struct OcpParser;

mod source_map;

type Pair<'a> = pest::iterators::Pair<'a, Rule>;

/// Parse an OpenCompose DSL source string into a Project AST.
pub fn parse(source: &str) -> Result<Project, ParseError> {
    let pairs = OcpParser::parse(Rule::project, source).map_err(|error| refuse(source, &error))?;

    let project_pair = pairs.into_iter().next().unwrap();
    parse_project(project_pair)
}

/// Parse an immutable source snapshot with bounded, occurrence-specific locations.
pub fn parse_document(
    source: &str,
    limits: crate::source_map::SourceLimits,
) -> Result<crate::source_map::ParsedDocument, crate::source_map::DocumentParseError> {
    let mut recorder = source_map::Recorder::new(source, limits)?;
    let pairs = OcpParser::parse(Rule::project, source).map_err(|error| refuse(source, &error))?;
    let pair = pairs.into_iter().next().unwrap();
    source_map::check_tree(pair.clone(), limits)?;
    let project = parse_project_with_map(pair, Some(&mut recorder))?;
    Ok(crate::source_map::ParsedDocument::new(
        project,
        source,
        recorder.finish()?,
    )?)
}

/// Rewrite native state paths without touching comments or other strings.
pub fn rewrite_synth_state_paths(
    source: &str,
    paths: &std::collections::BTreeMap<String, String>,
) -> Result<String, ParseError> {
    let pairs = OcpParser::parse(Rule::project, source).map_err(|error| refuse(source, &error))?;
    let mut changes = Vec::new();
    let parameters = pairs
        .flatten()
        .filter(|pair| {
            matches!(
                pair.as_rule(),
                Rule::preset_params_line | Rule::engine_params_line
            )
        })
        .flat_map(|pair| pair.into_inner().flatten())
        .filter(|pair| pair.as_rule() == Rule::key_value_pair);
    for pair in parameters {
        let mut parts = pair.into_inner();
        let key = parts.next().unwrap();
        let value = parts.next().unwrap();
        if key.as_str() == "open_synth_state_file"
            && let ParamValue::String(path) = parse_param_value(value.clone())
            && let Some(new) = paths.get(&path)
        {
            let span = value.as_span();
            changes.push((span.start()..span.end(), new));
        }
    }
    let mut result = source.to_string();
    for (range, path) in changes.into_iter().rev() {
        result.replace_range(range, &format!("\"{path}\""));
    }
    Ok(result)
}

/// Update only the authored picture clock field, retaining the rest of the source.
pub fn rewrite_picture_clock(
    source: &str,
    clock: Option<&crate::timecode::PictureClock>,
) -> Result<String, ParseError> {
    let mut parsed =
        OcpParser::parse(Rule::project, source).map_err(|error| refuse(source, &error))?;
    let mut blocks = parsed.next().unwrap().into_inner();
    let header = blocks
        .find(|pair| pair.as_rule() == Rule::project_header)
        .unwrap();
    let existing = header
        .into_inner()
        .flatten()
        .find(|pair| pair.as_rule() == Rule::picture_clock_prop);
    let mut next = source.to_owned();
    if let Some(property) = existing {
        let span = property.as_span();
        let text = clock
            .map(crate::serializer::picture_clock_property)
            .unwrap_or_default();
        next.replace_range(span.start()..span.end(), &text);
    } else if let Some(clock) = clock {
        // Insert after the header's trailing trivia so an inline comment stays
        // next to the property it describes.
        let at = blocks
            .next()
            .map_or(source.len(), |pair| pair.as_span().start());
        let newline = if source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let separator = if source[..at].ends_with(['\r', '\n']) {
            ""
        } else {
            newline
        };
        let property = crate::serializer::picture_clock_property(clock);
        next.insert_str(at, &format!("{separator}  {property}{newline}"));
    }
    Ok(next)
}

/// Turn a pest failure into a parse error. A line of block content cannot start
/// a top-level block, so pest reports the keyword it wanted instead; the refusal
/// names the block that has to hold the line.
fn refuse(source: &str, error: &pest::error::Error<Rule>) -> ParseError {
    let (line, col) = match error.line_col {
        pest::error::LineColLocation::Pos((line, col)) => (line, col),
        pest::error::LineColLocation::Span((line, col), _) => (line, col),
    };
    let message = misplaced_block_content(source, line, col)
        .unwrap_or_else(|| error.variant.message().to_string());
    ParseError::Syntax { line, col, message }
}

/// Rules that only appear inside a block, so a line writing one at the project
/// level cannot start a block.
const BLOCK_CONTENT_TOKENS: [&str; 9] = [
    "@phrase",
    "mix:",
    "effects:",
    "automation:",
    "gain_automation:",
    "clip:",
    "performance:",
    "engine_params:",
    "route:",
];

/// Sentence for block content written at the project level, or `None` when the
/// failure is a different one and pest's own message is the accurate one.
fn misplaced_block_content(source: &str, line: usize, col: usize) -> Option<String> {
    let text = source.lines().nth(line.checked_sub(1)?)?;
    let trimmed = text.trim_start();
    let token = trimmed.split_whitespace().next()?;
    let subject = if token == "@pattern" {
        // A definition carries neither a time range nor a repeat count, so a
        // failure on `[` or `xN` is the inline drum form, which needs a track.
        if !is_inline_drum_pattern(text, col) {
            return None;
        }
        "an inline drum pattern".to_string()
    } else if BLOCK_CONTENT_TOKENS.contains(&token) {
        // The failure has to sit on the first token of the line, where `@`
        // counts as one character.
        if col.saturating_sub(1) > text.len() - trimmed.len() + 1 {
            return None;
        }
        format!("`{token}`")
    } else {
        return None;
    };

    // A block header is `@` plus a keyword; a note position is `@` plus a digit.
    let is_header = |text: &str| {
        text.trim_start()
            .chars()
            .nth(1)
            .is_some_and(|c| c.is_alphanumeric())
    };
    let last_header = source
        .lines()
        .take(line - 1)
        .filter(|text| text.trim_start().starts_with('@') && is_header(text))
        .last();
    if last_header.is_some_and(|text| text.trim_start().starts_with("@track")) {
        // The track is still open, so pest already reported the real problem.
        return None;
    }
    Some(format!(
        "{subject} is track content, not a block: write it inside its `@track` \
         block, above the next `@` keyword; line {line} is outside every open block"
    ))
}

/// `@pattern name [range]` or `@pattern name xN` at the failure position is the
/// inline drum form, which only a track accepts.
fn is_inline_drum_pattern(text: &str, col: usize) -> bool {
    text.trim_start().starts_with("@pattern")
        && matches!(
            text.chars().nth(col.saturating_sub(1)),
            Some('[') | Some('x')
        )
}

// ============================================================================
// Top-level
// ============================================================================

fn parse_project(pair: Pair) -> Result<Project, ParseError> {
    parse_project_with_map(pair, None)
}

fn parse_project_with_map(
    pair: Pair,
    mut recorder: Option<&mut source_map::Recorder<'_>>,
) -> Result<Project, ParseError> {
    let mut name = None;
    let mut ocp_version = None;
    let mut properties = ProjectProperties::default();
    let mut tracks = Vec::new();
    let mut buses = Vec::new();
    let mut master = None;
    let mut sections = Vec::new();
    let mut markers = Vec::new();
    let mut groups = Vec::new();

    let mut presets = Vec::new();
    let mut named_patterns = Vec::new();
    let mut audio_clips = Vec::new();
    let mut vocal_tunes = Vec::new();
    let mut vocal_harmonies = Vec::new();
    let mut control_sources = Vec::new();
    let mut control_links = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::version_line => {
                if let Some(map) = recorder.as_deref_mut() {
                    map.version(&inner);
                }
                if let Some(sl) = inner.into_inner().next() {
                    ocp_version = Some(parse_string_literal(sl));
                }
            }
            Rule::project_header => {
                if let Some(map) = recorder.as_deref_mut() {
                    map.project_header(&inner);
                }
                for h in inner.into_inner() {
                    match h.as_rule() {
                        Rule::string_literal => {
                            name = Some(parse_string_literal(h));
                        }
                        Rule::project_property => {
                            parse_project_property(h, &mut properties)?;
                        }
                        _ => {}
                    }
                }
            }
            Rule::top_level_block => {
                let block = inner.into_inner().next().unwrap();
                if let Some(map) = recorder.as_deref_mut() {
                    use crate::source_map::SourceOwner;
                    let owner = match block.as_rule() {
                        Rule::control_link_block => {
                            Some(SourceOwner::ControlLink(control_links.len()))
                        }
                        Rule::control_source_block => {
                            Some(SourceOwner::ControlSource(control_sources.len()))
                        }
                        Rule::audio_clip_block => Some(SourceOwner::AudioClip(audio_clips.len())),
                        Rule::vocal_tune_block => Some(SourceOwner::VocalTune(vocal_tunes.len())),
                        Rule::vocal_harmony_block => {
                            Some(SourceOwner::VocalHarmony(vocal_harmonies.len()))
                        }
                        Rule::kit_block | Rule::preset_block => {
                            Some(SourceOwner::Preset(presets.len()))
                        }
                        Rule::pattern_def_block => Some(SourceOwner::Pattern(named_patterns.len())),
                        Rule::track_block => Some(SourceOwner::Track(tracks.len())),
                        Rule::bus_block => Some(SourceOwner::Bus(buses.len())),
                        Rule::master_block => Some(SourceOwner::Master),
                        Rule::section_block => Some(SourceOwner::Section(sections.len())),
                        Rule::marker_block => Some(SourceOwner::Marker(markers.len())),
                        Rule::group_block => Some(SourceOwner::Group(groups.len())),
                        _ => None,
                    };
                    if let Some(owner) = owner {
                        map.block(owner, &block);
                    }
                }
                match block.as_rule() {
                    Rule::control_link_block => control_links.push(parse_control_link(block)?),
                    Rule::control_source_block => {
                        control_sources.push(parse_control_source(block)?)
                    }
                    Rule::audio_clip_block => {
                        let (line, col) = block.as_span().start_pos().line_col();
                        audio_clips.push((parse_audio_clip(block)?, line, col));
                    }
                    Rule::vocal_tune_block => vocal_tunes.push(parse_vocal_tune(block)?),
                    Rule::vocal_harmony_block => vocal_harmonies.push(parse_vocal_harmony(block)?),
                    Rule::kit_block => presets.push(parse_kit_def(block)?),
                    Rule::preset_block => presets.push(parse_preset_def(block)?),
                    Rule::pattern_def_block => named_patterns.push(parse_pattern_def(block)?),
                    Rule::track_block => tracks.push(parse_track(block)?),
                    Rule::bus_block => buses.push(parse_bus(block)?),
                    Rule::master_block => master = Some(parse_master(block)?),
                    Rule::section_block => sections.push(parse_section(block)?),
                    Rule::marker_block => markers.push(parse_marker(block)),
                    Rule::group_block => groups.push(parse_group(block)),
                    Rule::stray_pedal_line => return Err(pedal_outside_phrase(block)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    properties.vocal_tunes = vocal_tunes;
    properties.vocal_harmonies = vocal_harmonies;
    properties.control_sources = control_sources;
    properties.control_links = control_links;

    let mut project = Project {
        name,
        ocp_version,
        properties,
        presets,
        tracks,
        patterns: named_patterns,
        buses,
        master,
        sections,
        markers,
        groups,
    };
    // Audio clips name a track that may be declared after them, so they move
    // into their track pools once the whole document is known.
    for (order, (clip, line, col)) in audio_clips.into_iter().enumerate() {
        let Some((track_index, track)) = project
            .tracks
            .iter_mut()
            .enumerate()
            .find(|(_, track)| track.name == clip.track)
        else {
            return Err(ParseError::Syntax {
                line,
                col,
                message: format!(
                    "Unknown audio clip track reference: '{}' (in audio clip '{}')",
                    clip.track, clip.name
                ),
            });
        };
        track.clips.push_audio_at(order, clip);
        if let Some(map) = recorder.as_deref_mut() {
            map.relocate_audio(track_index);
        }
    }
    Ok(project)
}

fn parse_project_property(pair: Pair, props: &mut ProjectProperties) -> Result<(), ParseError> {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::picture_clock_prop => {
            if props.picture_clock.is_some() {
                return Err(unsupported(
                    inner,
                    "the project already has a picture clock".into(),
                ));
            }
            let mut values = inner.clone().into_inner();
            let rate = parse_string_literal(values.next().unwrap());
            let origin = parse_string_literal(values.next().unwrap());
            let rate = serde_json::from_value(serde_json::Value::String(rate))
                .map_err(|_| unsupported(inner.clone(), "unsupported picture frame rate".into()))?;
            let settings = crate::timecode::PictureClock { rate, origin };
            crate::timecode::TimecodeClock::from_settings(&settings)
                .map_err(|error| unsupported(inner, error))?;
            props.picture_clock = Some(settings);
        }
        Rule::tempo_prop => {
            let num = inner.into_inner().next().unwrap();
            props.tempo = parse_number(num);
        }
        Rule::time_sig_prop => {
            let ts = inner.into_inner().next().unwrap();
            props.time_signature = parse_time_signature(ts);
        }
        Rule::key_prop => {
            let ks = inner.into_inner().next().unwrap();
            props.key = Some(parse_key_signature(ks));
        }
        Rule::swing_subdivision_prop => {
            let value = inner
                .into_inner()
                .next()
                .map(|value| value.as_str().to_owned());
            props.swing_subdivision = if value.as_deref() == Some("16th") {
                crate::ast::SwingSubdivision::Sixteenth
            } else {
                crate::ast::SwingSubdivision::Eighth
            };
        }
        Rule::swing_prop => {
            let pct = inner.into_inner().next().unwrap();
            props.swing = Some(parse_percentage(pct));
        }
        Rule::solo_mode_prop => {
            let value = inner.into_inner().next().unwrap();
            props.solo_mode = value
                .as_str()
                .parse()
                .expect("grammar restricts the solo mode");
        }
        Rule::mpe_prop => {
            let mut config = MpeConfig::default();
            for setting in inner.into_inner() {
                match setting.as_rule() {
                    Rule::mpe_zone_prop => {
                        let mut parts = setting.into_inner();
                        let zone = parts.next().unwrap().as_str();
                        let range = parse_channel_range(parts.next().unwrap());
                        if zone == "lower" {
                            config.lower = Some(range);
                        } else {
                            config.upper = Some(range);
                        }
                    }
                    Rule::mpe_bend_range_prop => {
                        let value = setting.into_inner().next().unwrap();
                        config.bend_range = Some(parse_number(value));
                    }
                    _ => {}
                }
            }
            props.mpe = Some(config);
        }
        Rule::export_prop => {
            let mut settings = ExportSettings::default();
            for setting in inner.into_inner() {
                match setting.as_rule() {
                    Rule::export_container_prop => {
                        let value = setting.into_inner().next().unwrap();
                        settings.container = value
                            .as_str()
                            .parse()
                            .expect("grammar restricts the export container");
                    }
                    Rule::export_wav_prop => {
                        for option in setting.into_inner() {
                            match option.as_rule() {
                                Rule::export_depth_prop => {
                                    let value = option.into_inner().next().unwrap();
                                    settings.wav.depth = value
                                        .as_str()
                                        .parse()
                                        .expect("grammar restricts the export format");
                                }
                                Rule::export_dither_prop => {
                                    let value = option.into_inner().next().unwrap();
                                    settings.wav.dither = value.as_str() == "true";
                                }
                                _ => {}
                            }
                        }
                    }
                    Rule::export_flac_prop => {
                        for option in setting.into_inner() {
                            match option.as_rule() {
                                Rule::export_depth_prop => {
                                    let value = option.into_inner().next().unwrap();
                                    settings.flac.depth = value
                                        .as_str()
                                        .parse()
                                        .expect("grammar restricts the export format");
                                }
                                Rule::export_flac_level_prop => {
                                    let value = option.into_inner().next().unwrap();
                                    // An unreadable level becomes one past the
                                    // range, so the validator names it instead
                                    // of silently substituting the default.
                                    settings.flac.level = value
                                        .as_str()
                                        .parse()
                                        .unwrap_or(crate::ast::MAX_FLAC_LEVEL + 1);
                                }
                                _ => {}
                            }
                        }
                    }
                    Rule::export_mp3_prop => {
                        for option in setting.into_inner() {
                            match option.as_rule() {
                                Rule::export_mp3_bitrate_prop => {
                                    let value = option.into_inner().next().unwrap();
                                    // Out-of-range numbers become one past the
                                    // range, so the validator names them.
                                    settings.mp3.bitrate = value
                                        .as_str()
                                        .parse()
                                        .unwrap_or(crate::ast::MAX_MP3_BITRATE_KBPS + 1);
                                }
                                Rule::export_mp3_vbr_quality_prop => {
                                    let value = option.into_inner().next().unwrap();
                                    settings.mp3.vbr_quality = Some(
                                        value
                                            .as_str()
                                            .parse()
                                            .unwrap_or(crate::ast::MAX_MP3_VBR_QUALITY + 1),
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                    // The historical flat pair, still accepted on load.
                    Rule::export_format_prop => {
                        let value = setting.into_inner().next().unwrap();
                        settings.wav.depth = value
                            .as_str()
                            .parse()
                            .expect("grammar restricts the export format");
                    }
                    Rule::export_dither_prop => {
                        let value = setting.into_inner().next().unwrap();
                        settings.wav.dither = value.as_str() == "true";
                    }
                    Rule::export_sample_rate_prop => {
                        let value = setting.into_inner().next().unwrap();
                        settings.sample_rate = value.as_str().parse().ok();
                    }
                    Rule::export_resample_prop => {
                        let value = setting.into_inner().next().unwrap();
                        settings.resample = value.as_str().parse().ok();
                    }
                    _ => {}
                }
            }
            props.export = Some(settings);
        }
        Rule::live_resample_prop => {
            let value = inner.into_inner().next().unwrap();
            props.live_resample = value.as_str().parse().ok();
        }
        Rule::automation_normalization_prop => {
            let value = inner.into_inner().next().unwrap();
            let (line, col) = value.as_span().start_pos().line_col();
            let version = value
                .as_str()
                .parse::<crate::automation_normalization::AutomationNormalization>()
                .map_err(|error| ParseError::Syntax {
                    line,
                    col,
                    message: error.to_string(),
                })?;
            props.automation_normalization = Some(version.number());
        }
        Rule::tempo_map_prop => {
            for pt in inner.into_inner() {
                if pt.as_rule() == Rule::tempo_map_point {
                    let mut parts = pt.into_inner();
                    let pos = parse_bar_beat(parts.next().unwrap());
                    let value = parts.next().unwrap();
                    let (offset_quarters, bpm) = if value.as_rule() == Rule::tempo_position_offset {
                        (
                            value
                                .into_inner()
                                .next()
                                .unwrap()
                                .as_str()
                                .parse::<f64>()
                                .unwrap(),
                            parts.next().unwrap().as_str().parse::<f64>().unwrap(),
                        )
                    } else {
                        (0.0, value.as_str().parse::<f64>().unwrap())
                    };
                    props.tempo_map.push(TempoPoint {
                        position: pos,
                        offset_quarters,
                        bpm,
                    });
                }
            }
        }
        Rule::time_sig_map_prop => {
            for pt in inner.into_inner() {
                if pt.as_rule() == Rule::time_sig_map_point {
                    let mut parts = pt.into_inner();
                    let pos = parse_bar_beat(parts.next().unwrap());
                    let ts = parse_time_signature(parts.next().unwrap());
                    props.time_sig_map.push(TimeSigPoint {
                        position: pos,
                        numerator: ts.numerator,
                        denominator: ts.denominator,
                    });
                }
            }
        }
        Rule::key_map_prop => {
            for pt in inner.into_inner() {
                if pt.as_rule() == Rule::key_map_point {
                    let mut parts = pt.into_inner();
                    let pos = parse_bar_beat(parts.next().unwrap());
                    let key = parse_key_signature(parts.next().unwrap());
                    props.key_map.push(KeyPoint { position: pos, key });
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// ============================================================================
// Track
// ============================================================================

fn parse_track(pair: Pair) -> Result<Track, ParseError> {
    let mut name = String::new();
    let mut instrument = InstrumentPath::new("unknown", "unknown");
    let mut preset = None;
    let engine = None;
    let mut mix = MixState::default();
    let mut effects = Vec::new();
    let mut phrases = Vec::new();
    let mut patterns = Vec::new();
    let mut automation = Vec::new();
    let mut engine_params = None;
    let mut performance = None;
    let mut inline_patterns = Vec::new();
    let mut clips = Vec::new();
    let mut channel = None;
    let mut output_port = None;
    let mut output_pair = None;
    let mut input_routes = Vec::new();
    let mut gain_automation = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::track_header => {
                for h in inner.into_inner() {
                    match h.as_rule() {
                        Rule::identifier | Rule::string_literal => name = name_of(h),
                        Rule::track_inline_props => {
                            for prop in h.into_inner() {
                                let p = prop.into_inner().next().unwrap();
                                match p.as_rule() {
                                    Rule::instrument_prop => {
                                        let path = p.into_inner().next().unwrap();
                                        instrument = parse_instrument_path(path);
                                    }
                                    Rule::preset_ref_prop => {
                                        let id = p.into_inner().next().unwrap();
                                        preset = Some(name_of(id));
                                    }
                                    Rule::engine_prop => {
                                        return Err(unsupported(
                                            p,
                                            format!(
                                                "track '{name}': `engine:` is not supported; the instrument and preset select the engine"
                                            ),
                                        ));
                                    }
                                    Rule::channel_prop => {
                                        let (line, col) = p.as_span().start_pos().line_col();
                                        let value = p.into_inner().next().unwrap();
                                        channel =
                                            Some(value.as_str().parse::<u8>().map_err(|_| {
                                                ParseError::Syntax {
                                                    line,
                                                    col,
                                                    message: "MIDI channel must be 0-15".into(),
                                                }
                                            })?);
                                    }
                                    Rule::output_port_prop => {
                                        output_port = Some(parse_output_port(p)?);
                                    }
                                    Rule::output_pair_prop => {
                                        output_pair = Some(parse_output_pair(p)?);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Rule::track_content => {
                let content = inner.into_inner().next().unwrap();
                match content.as_rule() {
                    Rule::performance_block => {
                        let (line, col) = content.as_span().start_pos().line_col();
                        let failure = |message| ParseError::Syntax { line, col, message };
                        if performance.is_some() {
                            return Err(failure("duplicate performance block".into()));
                        }
                        let mut events = Vec::new();
                        for event in content.into_inner() {
                            let fields: Vec<_> =
                                event.into_inner().map(|p| p.as_str().to_string()).collect();
                            events.push(
                                crate::performance::parse_event_fields(&fields).map_err(failure)?,
                            );
                        }
                        let timeline = crate::performance::PerformanceTimeline { events };
                        timeline.validate().map_err(failure)?;
                        performance = Some(timeline);
                    }
                    Rule::mix_line => mix = parse_mix_line(content)?,
                    Rule::effects_block => effects = parse_effects_block(content)?,
                    Rule::route_line => input_routes.push(parse_route_line(content)?),
                    Rule::phrase_block => phrases.push(parse_phrase(content)?),
                    Rule::pattern_block => patterns.push(parse_pattern(content)?),
                    Rule::pattern_def_block => inline_patterns.push(parse_pattern_def(content)?),
                    Rule::clip_line => clips.push(parse_clip(content)?),
                    Rule::automation_line => automation.push(parse_automation(content)?),
                    Rule::gain_automation_line => {
                        gain_automation.push(parse_gain_automation(content)?)
                    }
                    Rule::engine_params_line => {
                        engine_params = Some(parse_key_value_list(content)?);
                    }
                    Rule::stray_pedal_line => return Err(pedal_outside_phrase(content)),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(Track {
        name,
        instrument,
        preset,
        engine,
        mix,
        effects,
        phrases,
        patterns,
        inline_patterns,
        clips: {
            let mut pool = ClipPool::new();
            pool.set_pattern_clips(clips);
            pool.set_automation_lanes(automation);
            pool
        },
        gain_automation,
        engine_params,
        performance,
        channel,
        output_port,
        output_pair,
        input_routes,
    })
}

fn parse_output_port(pair: Pair) -> Result<u16, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    pair.into_inner()
        .next()
        .unwrap()
        .as_str()
        .parse::<u16>()
        .map_err(|_| ParseError::Syntax {
            line,
            col,
            message: "output port must be 0-65535".into(),
        })
}

fn parse_output_pair(pair: Pair) -> Result<u16, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    pair.into_inner()
        .next()
        .unwrap()
        .as_str()
        .parse::<u16>()
        .map_err(|_| ParseError::Syntax {
            line,
            col,
            message: "output pair must be 0-65535".into(),
        })
}

fn parse_preset_def(pair: Pair) -> Result<PresetDef, ParseError> {
    let mut name = String::new();
    let mut source = None;
    let mut plugin = None;
    let mut program = None;
    let mut patch = None;
    let mut file = None;
    let mut root_note = None;
    let mut params = Vec::new();
    let mut effects = Vec::new();
    let mut extends = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => name = name_of(inner),
            Rule::preset_content => {
                let content = inner.into_inner().next().unwrap();
                match content.as_rule() {
                    Rule::preset_source_prop => {
                        source = Some(content.into_inner().next().unwrap().as_str().to_string());
                    }
                    Rule::preset_plugin_prop => {
                        plugin = Some(parse_string_literal(content.into_inner().next().unwrap()));
                    }
                    Rule::preset_program_prop => {
                        let n = content.into_inner().next().unwrap();
                        let (line, col) = n.as_span().start_pos().line_col();
                        let written = n.as_str();
                        program = Some(written.parse::<u8>().map_err(|_| ParseError::Syntax {
                            line,
                            col,
                            message: format!("program: {written} is outside 0..=255"),
                        })?);
                    }
                    Rule::preset_patch_prop => {
                        patch = Some(parse_string_literal(content.into_inner().next().unwrap()));
                    }
                    Rule::preset_file_prop => {
                        file = Some(parse_string_literal(content.into_inner().next().unwrap()));
                    }
                    Rule::preset_root_note_prop => {
                        let pitch_pair = content.into_inner().next().unwrap();
                        root_note = Some(parse_pitch(pitch_pair)?);
                    }
                    Rule::preset_params_line => {
                        params = parse_key_value_list(content)?;
                    }
                    Rule::preset_extends_prop => {
                        let id = content.into_inner().next().unwrap();
                        extends = Some(name_of(id));
                    }
                    Rule::effects_block => {
                        effects = parse_effects_block(content)?;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(PresetDef {
        name,
        source,
        plugin,
        program,
        patch,
        file,
        root_note,
        params,
        effects,
        extends,
    })
}

/// Lower an `@kit` block into a builtin preset. Pad and layer fields become
/// canonical `pad.<name>.*` / `layer.<name>.<index>.*` params so the renderer
/// and the GUI share one document format.
fn parse_kit_def(pair: Pair) -> Result<PresetDef, ParseError> {
    let mut name = String::new();
    let mut max_voices = None;
    let mut pads: Vec<(String, Vec<(String, ParamValue)>)> = Vec::new();
    let mut layers: Vec<(String, Vec<(String, ParamValue)>)> = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => name = name_of(inner),
            Rule::kit_content => {
                let content = inner.into_inner().next().unwrap();
                match content.as_rule() {
                    Rule::kit_max_voices_prop => {
                        let value = content.into_inner().next().unwrap();
                        max_voices = value.as_str().parse::<u32>().ok();
                    }
                    Rule::kit_pad_line => {
                        let mut parts = content.into_inner();
                        let pad = parts.next().unwrap().as_str().to_string();
                        let props = parts
                            .next()
                            .map(parse_key_value_pairs)
                            .transpose()?
                            .unwrap_or_default();
                        pads.push((pad, props));
                    }
                    Rule::kit_layer_line => {
                        let mut parts = content.into_inner();
                        let pad = parts.next().unwrap().as_str().to_string();
                        let props = parts
                            .next()
                            .map(parse_key_value_pairs)
                            .transpose()?
                            .unwrap_or_default();
                        layers.push((pad, props));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let mut params = Vec::new();
    if let Some(voices) = max_voices {
        params.push((
            "kit.max_voices".to_string(),
            ParamValue::Number(voices as f64),
        ));
    }
    for (index, (pad, props)) in pads.iter().enumerate() {
        let note = match kit_prop(props, "note") {
            Some(ParamValue::String(text)) => parse_note_text(text).unwrap_or(36 + index as u8),
            Some(value) => value
                .as_f64()
                .map(|n| n.round().clamp(0.0, 127.0) as u8)
                .unwrap_or(36),
            None => 36 + index.min(91) as u8,
        };
        let numeric = |key: &str, default: f64| -> f64 {
            kit_prop(props, key)
                .and_then(ParamValue::as_f64)
                .unwrap_or(default)
        };
        let boolean = |key: &str, default: bool| -> bool {
            match kit_prop(props, key) {
                Some(ParamValue::Bool(value)) => *value,
                Some(ParamValue::String(text)) => text.eq_ignore_ascii_case("true"),
                _ => default,
            }
        };
        let seconds = |key: &str, default: f64| -> f64 {
            kit_prop(props, key)
                .and_then(ParamValue::as_f64)
                .unwrap_or(default)
                .max(0.0)
        };
        let prefix = format!("pad.{pad}.");
        params.push((format!("{prefix}note"), ParamValue::Number(f64::from(note))));
        params.push((
            format!("{prefix}gain"),
            ParamValue::Db(numeric("gain", 0.0)),
        ));
        params.push((
            format!("{prefix}pan"),
            ParamValue::Number(numeric("pan", 0.0).clamp(-1.0, 1.0)),
        ));
        params.push((
            format!("{prefix}tune"),
            ParamValue::Number(numeric("tune", 0.0).round()),
        ));
        params.push((
            format!("{prefix}cents"),
            ParamValue::Number(numeric("cents", 0.0)),
        ));
        params.push((
            format!("{prefix}decay"),
            ParamValue::Time(seconds("decay", 0.0)),
        ));
        params.push((
            format!("{prefix}release"),
            ParamValue::Time(seconds("release", 0.005)),
        ));
        params.push((
            format!("{prefix}reverse"),
            ParamValue::Bool(boolean("reverse", false)),
        ));
        params.push((
            format!("{prefix}one_shot"),
            ParamValue::Bool(boolean("one_shot", true)),
        ));
        let choke = numeric("choke", 0.0).round().clamp(0.0, 127.0);
        params.push((format!("{prefix}choke"), ParamValue::Number(choke)));
        if let Some(value) = kit_prop(props, "voice") {
            let voice = value_to_text(value);
            if !voice.trim().is_empty() {
                params.push((format!("{prefix}voice"), ParamValue::String(voice)));
            }
        }
    }
    // Keep layers for undeclared pads so validation can report the mismatch.
    let mut layer_pads: Vec<&String> = Vec::new();
    for (pad, _) in &layers {
        if !layer_pads.contains(&pad) {
            layer_pads.push(pad);
        }
    }
    for pad in layer_pads {
        let mut index = 0u32;
        for (layer_pad, props) in &layers {
            if layer_pad != pad {
                continue;
            }
            let low = kit_prop(props, "low")
                .and_then(ParamValue::as_f64)
                .unwrap_or(1.0)
                .round()
                .clamp(0.0, 127.0) as u8;
            let high = kit_prop(props, "high")
                .and_then(ParamValue::as_f64)
                .unwrap_or(127.0)
                .round()
                .clamp(0.0, 127.0) as u8;
            let sample = match kit_prop(props, "sample") {
                Some(ParamValue::String(text)) => text.clone(),
                Some(value) => value_to_text(value),
                None => String::new(),
            };
            let gain = kit_prop(props, "gain")
                .and_then(ParamValue::as_f64)
                .unwrap_or(0.0);
            let prefix = format!("layer.{pad}.{index}.");
            params.push((format!("{prefix}low"), ParamValue::Number(f64::from(low))));
            params.push((format!("{prefix}high"), ParamValue::Number(f64::from(high))));
            params.push((format!("{prefix}sample"), ParamValue::String(sample)));
            params.push((format!("{prefix}gain"), ParamValue::Db(gain)));
            index += 1;
        }
    }

    Ok(PresetDef {
        name,
        source: Some("builtin".to_string()),
        plugin: Some(crate::DRUMKIT_PLUGIN_ID.to_string()),
        program: None,
        patch: None,
        file: None,
        root_note: None,
        params,
        effects: Vec::new(),
        extends: None,
    })
}

fn kit_prop<'a>(props: &'a [(String, ParamValue)], key: &str) -> Option<&'a ParamValue> {
    props
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn value_to_text(value: &ParamValue) -> String {
    match value {
        ParamValue::String(text) => text.clone(),
        ParamValue::Number(n) | ParamValue::Db(n) | ParamValue::Percentage(n) => n.to_string(),
        ParamValue::Frequency(n) | ParamValue::Cents(n) | ParamValue::Time(n) => n.to_string(),
        ParamValue::Bool(value) => value.to_string(),
        ParamValue::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(f64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Parse a MIDI number or a scientific note name (`C2`, `A#1`, `Bb0`).
fn parse_note_text(text: &str) -> Option<u8> {
    let text = text.trim();
    if let Ok(number) = text.parse::<f64>() {
        return (0.0..=127.0)
            .contains(&number)
            .then_some(number.round() as u8);
    }
    let bytes = text.as_bytes();
    let semitone = match bytes.first()?.to_ascii_uppercase() {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return None,
    };
    let mut index = 1usize;
    let mut accidental = 0i32;
    match bytes.get(index) {
        Some(b'#') => {
            accidental = 1;
            index += 1;
        }
        Some(b'b') => {
            accidental = -1;
            index += 1;
        }
        _ => {}
    }
    let octave = text.get(index..)?.parse::<i32>().ok()?;
    let midi = (octave + 1) * 12 + semitone + accidental;
    (0..=127).contains(&midi).then_some(midi as u8)
}

fn parse_audio_clip(pair: Pair) -> Result<AudioClip, ParseError> {
    let mut name = String::new();
    let mut track = String::new();
    let mut file = String::new();
    let mut time_range = TimeRange {
        start: BarBeat::new(1, 1, 0),
        end: BarBeat::new(1, 1, 0),
    };
    let mut placement = AudioClipPlacement::default();
    let mut offset_seconds = 0.0;
    let mut gain_db = 0.0;
    let mut fade_in_seconds = 0.0;
    let mut fade_out_seconds = 0.0;
    let mut invert = false;
    let mut crossfade_seconds = 0.0;
    let mut stretch = 1.0;
    let mut stretch_mode = StretchMode::Musical;
    let mut bypass_processing = false;
    let mut tempo_follow = false;
    let mut pitch = 0.0;
    let mut formant = 0.0;
    let mut preserve_formants = false;
    let mut warp_markers = Vec::new();
    let mut loop_seconds = 0.0;
    let mut take_group = None;
    let mut take = None;
    let mut muted = false;
    let mut gain_envelope = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => name = name_of(inner),
            Rule::time_range => time_range = parse_time_range(inner),
            Rule::audio_clip_track_prop => {
                track = inner.into_inner().next().map(name_of).unwrap_or_default();
            }
            Rule::audio_clip_content => {
                let content = inner.into_inner().next().unwrap();
                match content.as_rule() {
                    Rule::audio_clip_file_prop => {
                        file = parse_string_literal(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_placement_start_prop => {
                        placement.start_offset_seconds =
                            parse_time_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_placement_end_prop => {
                        placement.end_offset_seconds =
                            parse_time_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_offset_prop => {
                        offset_seconds = parse_time_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_loop_prop => {
                        loop_seconds = parse_time_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_gain_prop => {
                        gain_db = parse_db_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_gain_envelope_prop => {
                        gain_envelope = content
                            .into_inner()
                            .map(|point| {
                                let mut numbers = point.into_inner();
                                GainEnvelopePoint {
                                    position: parse_number(numbers.next().unwrap()),
                                    gain_db: parse_db_value(numbers.next().unwrap()),
                                }
                            })
                            .collect();
                    }
                    Rule::audio_clip_fade_in_prop => {
                        fade_in_seconds = parse_time_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_fade_out_prop => {
                        fade_out_seconds = parse_time_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_invert_prop => {
                        invert = content.into_inner().next().unwrap().as_str() == "true";
                    }
                    Rule::audio_clip_crossfade_prop => {
                        crossfade_seconds = parse_time_value(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_stretch_prop => {
                        stretch = parse_number(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_stretch_mode_prop => {
                        stretch_mode = match content.into_inner().next().unwrap().as_str() {
                            "off" => StretchMode::Off,
                            "percussive" => StretchMode::Percussive,
                            "varispeed" => StretchMode::Varispeed,
                            _ => StretchMode::Musical,
                        };
                    }
                    Rule::audio_clip_bypass_processing_prop => {
                        bypass_processing = content.into_inner().next().unwrap().as_str() == "true";
                    }
                    Rule::audio_clip_tempo_follow_prop => {
                        tempo_follow = content.into_inner().next().unwrap().as_str() == "true";
                    }
                    Rule::audio_clip_pitch_prop => {
                        pitch = parse_number(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_warp_prop => {
                        let mut times = content.into_inner();
                        warp_markers.push(AudioWarpMarker {
                            source_seconds: parse_time_value(times.next().unwrap()),
                            target_seconds: parse_time_value(times.next().unwrap()),
                        });
                    }
                    Rule::audio_clip_formant_prop => {
                        formant = parse_number(content.into_inner().next().unwrap());
                    }
                    Rule::audio_clip_preserve_formants_prop => {
                        preserve_formants = content.into_inner().next().unwrap().as_str() == "true";
                    }
                    Rule::audio_clip_take_group_prop => {
                        take_group =
                            Some(parse_string_literal(content.into_inner().next().unwrap()));
                    }
                    Rule::audio_clip_take_prop => {
                        take = content.into_inner().next().unwrap().as_str().parse().ok();
                    }
                    Rule::audio_clip_muted_prop => {
                        muted = content.into_inner().next().unwrap().as_str() == "true";
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(AudioClip {
        name,
        track,
        file,
        time_range,
        placement,
        offset_seconds,
        gain_db,
        fade_in_seconds,
        fade_out_seconds,
        invert,
        crossfade_seconds,
        stretch,
        stretch_mode,
        bypass_processing,
        tempo_follow,
        pitch,
        formant,
        preserve_formants,
        warp_markers,
        loop_seconds,
        take_group,
        take,
        muted,
        gain_envelope,
    })
}

const CONTROL_SOURCE_FIELDS: &str =
    "from, attack, release, lfo, lfo_depth, formula, scale, offset or curve";

struct ControlSourceDraft {
    source_track: Option<String>,
    attack: Option<f64>,
    release: Option<f64>,
    lfo: Option<ControlLfo>,
    lfo_depth: Option<f64>,
    formula: Option<String>,
    scale: Option<f64>,
    offset: Option<f64>,
    curve: Option<ControlCurve>,
}

fn parse_control_source(pair: Pair) -> Result<ControlSource, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    let mut name = String::new();
    let mut draft = ControlSourceDraft {
        source_track: None,
        attack: None,
        release: None,
        lfo: None,
        lfo_depth: None,
        formula: None,
        scale: None,
        offset: None,
        curve: None,
    };

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => name = name_of(inner),
            Rule::control_source_props => {
                for prop in inner.into_inner() {
                    apply_control_source_prop(prop, &mut draft)?;
                }
            }
            Rule::control_source_prop => apply_control_source_prop(inner, &mut draft)?,
            _ => {}
        }
    }

    let missing = |field: &str| ParseError::Syntax {
        line,
        col,
        message: format!("@control_source '{name}' needs `{field}:`"),
    };
    let source_track = draft
        .source_track
        .ok_or_else(|| missing("from: track(<name>)"))?;
    let attack = draft.attack.ok_or_else(|| missing("attack"))?;
    let release = draft.release.ok_or_else(|| missing("release"))?;

    Ok(ControlSource {
        name,
        source_track,
        attack,
        release,
        lfo: draft.lfo,
        lfo_depth: draft.lfo_depth.unwrap_or(1.0),
        formula: draft.formula,
        scale: draft.scale.unwrap_or(1.0),
        offset: draft.offset.unwrap_or(0.0),
        curve: draft.curve.unwrap_or_default(),
    })
}

fn apply_control_source_prop(prop: Pair, draft: &mut ControlSourceDraft) -> Result<(), ParseError> {
    let field = prop.into_inner().next().expect("control source prop");
    match field.as_rule() {
        Rule::control_from_prop => {
            let tap = field.clone().into_inner().next().expect("control tap");
            let Some(track) = parse_control_tap(tap) else {
                return Err(unsupported(field, "`from:` needs a track name".to_string()));
            };
            draft.source_track = Some(track);
        }
        Rule::control_attack_prop => {
            draft.attack = Some(parse_time_value(field.into_inner().next().unwrap()));
        }
        Rule::control_release_prop => {
            draft.release = Some(parse_time_value(field.into_inner().next().unwrap()));
        }
        Rule::control_lfo_prop => {
            draft.lfo = parse_control_lfo(field.into_inner().next().unwrap())?;
        }
        Rule::control_lfo_depth_prop => {
            draft.lfo_depth = Some(parse_number(field.into_inner().next().unwrap()));
        }
        Rule::control_formula_prop => {
            draft.formula = parse_control_formula(field)?;
        }
        Rule::control_scale_prop => {
            let value = field.into_inner().next().unwrap();
            draft.scale = Some(value.as_str().parse::<f64>().unwrap_or(1.0));
        }
        Rule::control_offset_prop => {
            let value = field.into_inner().next().unwrap();
            draft.offset = Some(value.as_str().parse::<f64>().unwrap_or(0.0));
        }
        Rule::control_curve_prop => {
            draft.curve = Some(match field.into_inner().next().unwrap().as_str() {
                "squared" => ControlCurve::Squared,
                "sqrt" => ControlCurve::Sqrt,
                "log" => ControlCurve::Log,
                _ => ControlCurve::Linear,
            });
        }
        Rule::control_source_unknown_prop => {
            let key = field
                .clone()
                .into_inner()
                .next()
                .map(|pair| pair.as_str().to_string())
                .unwrap_or_default();
            return Err(unsupported(
                field,
                format!("unknown @control_source field '{key}'; expected {CONTROL_SOURCE_FIELDS}"),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn parse_control_tap(pair: Pair) -> Option<String> {
    pair.into_inner()
        .find(|inner| matches!(inner.as_rule(), Rule::identifier | Rule::string_literal))
        .map(name_of)
}

fn parse_control_lfo(pair: Pair) -> Result<Option<ControlLfo>, ParseError> {
    let mut shape = None;
    let mut rate_hz = 1.0;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::control_lfo_shape => {
                shape = Some(match inner.as_str() {
                    "triangle" => ControlLfoShape::Triangle,
                    "square" => ControlLfoShape::Square,
                    "saw" => ControlLfoShape::Saw,
                    _ => ControlLfoShape::Sine,
                });
            }
            Rule::control_lfo_rate => {
                rate_hz = parse_freq_value(inner.into_inner().next().unwrap());
            }
            _ => {}
        }
    }
    Ok(shape.map(|shape| ControlLfo { shape, rate_hz }))
}

fn parse_control_formula(pair: Pair) -> Result<Option<String>, ParseError> {
    let field = pair.into_inner().next().unwrap();
    let text = field.as_str().trim();
    if text.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    if let Err(error) = crate::formula::Formula::parse(text) {
        return Err(unsupported(
            field,
            format!("invalid control formula: {error}"),
        ));
    }
    Ok(Some(text.to_string()))
}

const VOCAL_TUNE_FIELDS: &str =
    "scale, snap, reference, cap, window, max_move, amount, retune, formant, vibrato or notes";
const VOCAL_NOTE_FIELDS: &str =
    "at, tgt, amount, retune, formant, start, length, vibrato or bypass";
const VOCAL_HARMONY_FIELDS: &str = "interval, level or pan";

fn parse_vocal_tune(pair: Pair) -> Result<VocalTune, ParseError> {
    let mut tune = VocalTune::default();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => tune.name = name_of(inner),
            Rule::vocal_tune_selector => {
                for prop in inner.into_inner() {
                    if prop.as_rule() != Rule::vocal_tune_selector_prop {
                        continue;
                    }
                    let mut fields = prop.into_inner();
                    let key = fields
                        .next()
                        .map(|pair| pair.as_str().to_string())
                        .unwrap_or_default();
                    let value = fields.next().map(name_of).unwrap_or_default();
                    match key.as_str() {
                        "clip" => tune.clip = Some(value),
                        "track" => tune.track = Some(value),
                        _ => {}
                    }
                }
            }
            Rule::vocal_tune_content => {
                parse_vocal_tune_content(inner.into_inner().next().unwrap(), &mut tune)?;
            }
            _ => {}
        }
    }
    Ok(tune)
}

fn parse_vocal_tune_content(content: Pair, tune: &mut VocalTune) -> Result<(), ParseError> {
    match content.as_rule() {
        Rule::vocal_tune_scale_prop => {
            let value = content.into_inner().next().unwrap();
            let inner = value.into_inner().next().unwrap();
            tune.scale = if inner.as_rule() == Rule::vocal_tune_inherit {
                None
            } else {
                Some(parse_key_signature(inner))
            };
        }
        Rule::vocal_tune_snap_prop => {
            tune.snap = match content.into_inner().next().unwrap().as_str() {
                "scale" => VocalSnapPolicy::Scale,
                "reference" => VocalSnapPolicy::Reference,
                "off" => VocalSnapPolicy::Off,
                _ => VocalSnapPolicy::Auto,
            };
        }
        Rule::vocal_tune_reference_prop => {
            tune.reference = Some(name_of(content.into_inner().next().unwrap()));
        }
        Rule::vocal_tune_cap_prop => {
            tune.snap_cap_cents = parse_signed_cents(content.into_inner().next().unwrap());
        }
        Rule::vocal_tune_window_prop => {
            tune.snap_window_cents = parse_signed_cents(content.into_inner().next().unwrap());
        }
        Rule::vocal_tune_max_move_prop => {
            tune.max_move_seconds = parse_time_value(content.into_inner().next().unwrap());
        }
        Rule::vocal_tune_amount_prop => {
            tune.amount = parse_percentage(content.into_inner().next().unwrap());
        }
        Rule::vocal_tune_retune_prop => {
            tune.retune_seconds = parse_time_value(content.into_inner().next().unwrap());
        }
        Rule::vocal_tune_formant_prop => {
            tune.formant_cents = parse_signed_cents(content.into_inner().next().unwrap());
        }
        Rule::vocal_tune_vibrato_prop => {
            tune.vibrato = match content.into_inner().next().unwrap().as_str() {
                "flatten" => VocalVibratoMode::Flatten,
                "scale" => VocalVibratoMode::Scale,
                _ => VocalVibratoMode::Preserve,
            };
        }
        Rule::vocal_tune_notes => {
            for note in content.into_inner() {
                tune.notes.push(parse_vocal_tune_note(note)?);
            }
        }
        Rule::vocal_tune_unknown_prop => {
            let field = content
                .clone()
                .into_inner()
                .next()
                .map(|pair| pair.as_str().to_string())
                .unwrap_or_default();
            return Err(unsupported(
                content,
                format!("unknown @vocal_tune field '{field}'; expected {VOCAL_TUNE_FIELDS}"),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn parse_vocal_tune_note(pair: Pair) -> Result<VocalTuneNote, ParseError> {
    let mut note = VocalTuneNote {
        at: 0.0,
        target: None,
        amount: None,
        retune_seconds: None,
        formant_cents: None,
        start_seconds: None,
        length: None,
        vibrato: None,
        bypass: false,
    };
    for item in pair.into_inner() {
        if item.as_rule() != Rule::vocal_note_item {
            continue;
        }
        let mut fields = item.clone().into_inner();
        let key = fields
            .next()
            .map(|pair| pair.as_str().to_string())
            .unwrap_or_default();
        let token = fields
            .next()
            .map(|pair| pair.as_str().to_string())
            .unwrap_or_default();
        let expected = |what: &str| {
            unsupported(
                item.clone(),
                format!("invalid note field '{key}={token}': expected {what}"),
            )
        };
        match key.as_str() {
            "at" => {
                note.at = parse_seconds_token(&token)
                    .ok_or_else(|| expected("a clip-local time like 2.013s or 2013ms"))?;
            }
            "tgt" => {
                note.target = Some(parse_vocal_target(&token).map_err(expected)?);
            }
            "amount" => {
                note.amount =
                    Some(parse_percent_token(&token).ok_or_else(|| expected("0%..100%"))?);
            }
            "retune" => {
                note.retune_seconds =
                    Some(parse_seconds_token(&token).ok_or_else(|| expected("a time like 25ms"))?);
            }
            "formant" => {
                note.formant_cents =
                    Some(parse_cents_token(&token).ok_or_else(|| expected("cents like -150ct"))?);
            }
            "start" => {
                note.start_seconds = Some(
                    parse_seconds_token(&token)
                        .ok_or_else(|| expected("a relative time like +30ms or -12ms"))?,
                );
            }
            "length" => {
                let value = token
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| expected("a length multiplier like 1.08"))?;
                note.length = Some(value);
            }
            "vibrato" => {
                note.vibrato = Some(match token.as_str() {
                    "preserve" => VocalVibratoMode::Preserve,
                    "flatten" => VocalVibratoMode::Flatten,
                    "scale" => VocalVibratoMode::Scale,
                    _ => return Err(expected("preserve, flatten or scale")),
                });
            }
            "bypass" => {
                note.bypass = parse_bool_token(&token).ok_or_else(|| expected("true or false"))?;
            }
            _ => {
                return Err(unsupported(
                    item,
                    format!("unknown note field '{key}'; expected {VOCAL_NOTE_FIELDS}"),
                ));
            }
        }
    }
    Ok(note)
}

/// Parse one `@vocal_harmony` block: a selector plus interval, level and pan.
fn parse_vocal_harmony(pair: Pair) -> Result<VocalHarmony, ParseError> {
    let mut harmony = VocalHarmony::default();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => harmony.name = name_of(inner),
            Rule::vocal_harmony_selector => {
                for prop in inner.into_inner() {
                    if prop.as_rule() != Rule::vocal_tune_selector_prop {
                        continue;
                    }
                    let mut fields = prop.into_inner();
                    let key = fields
                        .next()
                        .map(|pair| pair.as_str().to_string())
                        .unwrap_or_default();
                    let value = fields.next().map(name_of).unwrap_or_default();
                    match key.as_str() {
                        "clip" => harmony.clip = Some(value),
                        "track" => harmony.track = Some(value),
                        _ => {}
                    }
                }
            }
            Rule::vocal_harmony_content => {
                parse_vocal_harmony_content(inner.into_inner().next().unwrap(), &mut harmony)?;
            }
            _ => {}
        }
    }
    Ok(harmony)
}

fn parse_vocal_harmony_content(
    content: Pair,
    harmony: &mut VocalHarmony,
) -> Result<(), ParseError> {
    match content.as_rule() {
        Rule::vocal_harmony_interval_prop => {
            let value = content.into_inner().next().unwrap();
            harmony.interval = value.as_str().parse().map_err(|_| {
                unsupported(
                    value,
                    "invalid interval: expected whole diatonic steps like 2 or -3".to_string(),
                )
            })?;
        }
        Rule::vocal_harmony_level_prop => {
            harmony.level_db = parse_db_value(content.into_inner().next().unwrap());
        }
        Rule::vocal_harmony_pan_prop => {
            harmony.pan = parse_number(content.into_inner().next().unwrap());
        }
        Rule::vocal_harmony_unknown_prop => {
            let field = content
                .clone()
                .into_inner()
                .next()
                .map(|pair| pair.as_str().to_string())
                .unwrap_or_default();
            return Err(unsupported(
                content,
                format!("unknown @vocal_harmony field '{field}'; expected {VOCAL_HARMONY_FIELDS}"),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn parse_seconds_token(text: &str) -> Option<f64> {
    let value = if let Some(ms) = text.strip_suffix("ms") {
        ms.parse::<f64>().ok()? / 1000.0
    } else {
        text.strip_suffix('s')?.parse::<f64>().ok()?
    };
    value.is_finite().then_some(value)
}

fn parse_percent_token(text: &str) -> Option<f64> {
    let value = text.strip_suffix('%')?.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

fn parse_cents_token(text: &str) -> Option<f64> {
    let value = text.strip_suffix("ct")?.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

fn parse_bool_token(text: &str) -> Option<bool> {
    match text {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Decode one `tgt=` token. The token's shape selects the target kind, so a
/// signed cents value is a relative offset and an unsigned one is absolute.
fn parse_vocal_target(text: &str) -> Result<VocalTarget, &'static str> {
    const EXPECTED: &str = "a pitch like A3, an offset like +2st, a cents target like 5700ct or +25ct, or a degree like 5@4";
    if let Some((degree, octave)) = text.split_once('@') {
        let degree: u8 = degree.parse().map_err(|_| EXPECTED)?;
        let octave: i32 = octave.parse().map_err(|_| EXPECTED)?;
        if !(1..=7).contains(&degree) {
            return Err("a one-based scale degree 1..7");
        }
        return Ok(VocalTarget::Degree { degree, octave });
    }
    if let Some(value) = text.strip_suffix("st") {
        let value: f64 = value.parse().map_err(|_| EXPECTED)?;
        return value
            .is_finite()
            .then_some(VocalTarget::Semitones(value))
            .ok_or(EXPECTED);
    }
    if let Some(value) = text.strip_suffix("ct") {
        let value: f64 = value.parse().map_err(|_| EXPECTED)?;
        if !value.is_finite() {
            return Err(EXPECTED);
        }
        return if text.starts_with('+') || text.starts_with('-') {
            Ok(VocalTarget::OffsetCents(value))
        } else {
            Ok(VocalTarget::Cents(value))
        };
    }
    parse_note_text(text)
        .map(VocalTarget::Pitch)
        .ok_or(EXPECTED)
}

fn parse_signed_cents(pair: Pair) -> f64 {
    pair.as_str()
        .strip_suffix("ct")
        .and_then(|text| text.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn parse_instrument_path(pair: Pair) -> InstrumentPath {
    let s = pair.as_str();
    match s.split_once('/') {
        Some((category, name)) => InstrumentPath::new(category, name),
        // Bare names denote the drum-kit instrument: the pad grid is the patch.
        None => InstrumentPath::new("drums", s),
    }
}

// ============================================================================
// Mix
// ============================================================================

/// One `route:` line: an optional kind (audio is the default), a source and an
/// effect address in the owner's chain.
fn parse_route_line(pair: Pair) -> Result<InputRoute, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    let mut kind = RouteKind::Audio;
    let mut source = String::new();
    let mut target = String::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::route_kind => {
                kind = if inner.as_str() == "control" {
                    RouteKind::Control
                } else {
                    RouteKind::Audio
                };
            }
            Rule::route_target => target = inner.as_str().to_string(),
            Rule::identifier | Rule::string_literal => source = name_of(inner),
            _ => {}
        }
    }
    if source.is_empty() || target.is_empty() {
        return Err(ParseError::Syntax {
            line,
            col,
            message: "route needs a source and an effect target (`route: control kick_env -> compression`)"
                .into(),
        });
    }
    Ok(InputRoute {
        kind,
        source,
        target,
    })
}

fn parse_mix_line(pair: Pair) -> Result<MixState, ParseError> {
    let mut mix = MixState::default();

    for prop in pair.into_inner() {
        if prop.as_rule() != Rule::mix_property {
            continue;
        }
        let inner = prop.into_inner().next().unwrap();
        match inner.as_rule() {
            Rule::vol_prop => {
                let db = inner.into_inner().next().unwrap();
                mix.volume_db = parse_db_value(db);
            }
            Rule::output_prop => {
                mix.output = Some(name_of(inner.into_inner().next().unwrap()));
            }
            Rule::pan_prop => {
                let val = inner.into_inner().next().unwrap();
                mix.pan = val.as_str().parse::<f64>().unwrap_or(0.0);
            }
            Rule::pan_law_prop => {
                mix.stereo_balance = inner.into_inner().next().unwrap().as_str() == "balance";
            }
            Rule::send_prop => {
                let mut parts = inner.into_inner().peekable();
                let source = if parts.peek().map(|p| p.as_rule()) == Some(Rule::aux_source) {
                    let aux = parts.next().unwrap().into_inner().next().unwrap();
                    let mut owner = String::new();
                    let mut output = 0u8;
                    for field in aux.into_inner() {
                        match field.as_rule() {
                            Rule::identifier | Rule::string_literal => owner = name_of(field),
                            Rule::integer => {
                                output = field.as_str().parse::<u8>().unwrap_or(u8::MAX);
                            }
                            _ => {}
                        }
                    }
                    Some(SendSource { owner, output })
                } else {
                    None
                };
                let target = name_of(parts.next().unwrap());
                let value = parts.next().unwrap().into_inner().next().unwrap();
                let amount = match value.as_rule() {
                    Rule::percentage => parse_percentage(value) / 100.0,
                    Rule::db_value => 10.0_f64.powf(parse_db_value(value) / 20.0),
                    _ => value.as_str().parse::<f64>().unwrap_or(0.0),
                };
                let mut pre_fader = false;
                let mut pan = 0.0;
                for extra in parts {
                    let Some(value) = extra.into_inner().next() else {
                        continue;
                    };
                    match value.as_rule() {
                        Rule::send_tap => {
                            pre_fader = value.as_str().trim_start_matches(',').trim() == "pre";
                        }
                        Rule::send_pan => {
                            let number = value.into_inner().next().unwrap();
                            pan = number.as_str().parse::<f64>().unwrap_or(0.0);
                        }
                        _ => {}
                    }
                }
                mix.sends.push(Send {
                    target,
                    amount,
                    source,
                    pre_fader,
                    pan,
                });
            }
            Rule::mute_prop => {
                let val = inner.into_inner().next().unwrap();
                mix.mute = val.as_str() == "true";
            }
            Rule::solo_prop => {
                let val = inner.into_inner().next().unwrap();
                mix.solo = val.as_str() == "true";
            }
            Rule::display_name_prop => {
                let value = inner.into_inner().next().unwrap();
                mix.overrides.display_name = Some(parse_string_literal(value));
            }
            Rule::color_prop => {
                let value = inner.into_inner().next().unwrap();
                mix.overrides.color = Some(parse_string_literal(value));
            }
            Rule::solo_safe_prop => {
                let val = inner.into_inner().next().unwrap();
                mix.overrides.solo_safe = val.as_str() == "true";
            }
            Rule::polarity_prop => {
                let val = inner.into_inner().next().unwrap();
                mix.polarity_inverted = matches!(val.as_str(), "invert" | "true");
            }
            Rule::pdc_prop => {
                let val = inner.into_inner().next().unwrap();
                mix.latency_compensation = match val.as_str() {
                    "off" => LatencyCompensation::Off,
                    "when_recording" => LatencyCompensation::WhenRecording,
                    _ => LatencyCompensation::On,
                };
            }
            Rule::latency_prop => {
                let val = inner.into_inner().next().unwrap();
                let (line, col) = val.as_span().start_pos().line_col();
                let written = val.as_str();
                mix.latency_offset = written.parse::<i32>().map_err(|_| ParseError::Syntax {
                    line,
                    col,
                    message: format!(
                        "latency: {written} is outside -2147483648..=2147483647 samples"
                    ),
                })?;
            }
            Rule::smart_disable_prop => {
                let val = inner.into_inner().next().unwrap();
                mix.smart_disable = val.as_str() == "true";
            }
            Rule::control_prop => {
                let mut source = String::new();
                let mut depth_db = 0.0;
                for field in inner.into_inner() {
                    match field.as_rule() {
                        Rule::identifier | Rule::string_literal => source = name_of(field),
                        Rule::db_value => depth_db = parse_db_value(field),
                        _ => {}
                    }
                }
                mix.control_gain = Some(ControlGain { source, depth_db });
            }
            _ => {}
        }
    }

    Ok(mix)
}

// ============================================================================
// Effects
// ============================================================================

fn parse_effects_block(pair: Pair) -> Result<Vec<Effect>, ParseError> {
    let mut effects = Vec::new();

    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::effect_entry {
            effects.push(parse_effect_entry(inner)?);
        }
    }

    Ok(effects)
}

fn parse_effect_entry(pair: Pair) -> Result<Effect, ParseError> {
    let mut parts = pair.into_inner();
    let type_pair = parts.next().unwrap();
    let type_str = type_pair.as_str();
    let effect_type = if let Some(rest) = type_str.strip_prefix("plugin:") {
        let id = if rest.starts_with('"') {
            serde_json::from_str::<String>(rest).map_err(|_| {
                let (line, col) = type_pair.as_span().start_pos().line_col();
                ParseError::Syntax {
                    line,
                    col,
                    message: "invalid quoted plugin identifier".into(),
                }
            })?
        } else {
            rest.to_string()
        };
        EffectType::Plugin(id)
    } else {
        EffectType::builtin_from_name(type_str).ok_or_else(|| {
            let (line, col) = type_pair.as_span().start_pos().line_col();
            ParseError::Syntax {
                line,
                col,
                message: format!("unknown effect type: {type_str}"),
            }
        })?
    };

    let mut params = Vec::new();
    // `bypass` is an effect field, not a device parameter, so it leaves the
    // parameter list wherever the entry writes it.
    let mut bypass: Option<bool> = None;
    if let Some(params_pair) = parts.next() {
        for ep in params_pair.into_inner() {
            if ep.as_rule() == Rule::effect_param {
                let mut ep_inner = ep.into_inner();
                let name_pair = ep_inner.next().unwrap();
                let name = name_pair.as_str().to_string();
                let raw_value = ep_inner.next().unwrap().as_str().to_string();
                if name == "bypass" {
                    let (line, col) = name_pair.as_span().start_pos().line_col();
                    if bypass.is_some() {
                        return Err(ParseError::Syntax {
                            line,
                            col,
                            message: "bypass appears twice".into(),
                        });
                    }
                    bypass =
                        Some(
                            parse_bool_token(&raw_value).ok_or_else(|| ParseError::Syntax {
                                line,
                                col,
                                message: "bypass expects true or false".into(),
                            })?,
                        );
                    continue;
                }
                let value = parse_effect_value(&raw_value);
                params.push((name, value));
            }
        }
    }

    // Extract sidechain from params
    let sidechain_source =
        params
            .iter()
            .find(|(k, _)| k == "sidechain")
            .and_then(|(_, v)| match v {
                ParamValue::String(s) => Some(s.clone()),
                _ => None,
            });
    params.retain(|(k, _)| k != "sidechain");

    Ok(Effect {
        effect_type,
        params,
        bypass: bypass.unwrap_or(false),
        sidechain_source,
    })
}

// ============================================================================
// Phrase
// ============================================================================

fn parse_phrase(pair: Pair) -> Result<Phrase, ParseError> {
    let mut name = String::new();
    let mut time_range = None;
    let mut items = Vec::new();
    let mut pedals = Vec::new();
    let mut hairpins = Vec::new();
    let mut channel = None;
    let mut previous_end_line = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::phrase_header => {
                for h in inner.into_inner() {
                    match h.as_rule() {
                        Rule::identifier | Rule::string_literal => name = name_of(h),
                        Rule::time_range => time_range = Some(parse_time_range(h)),
                        Rule::channel_prop => {
                            let (line, col) = h.as_span().start_pos().line_col();
                            let value = h.into_inner().next().unwrap();
                            channel = Some(value.as_str().parse::<u8>().map_err(|_| {
                                ParseError::Syntax {
                                    line,
                                    col,
                                    message: "MIDI channel must be 0-15".into(),
                                }
                            })?);
                        }
                        _ => {}
                    }
                }
            }
            Rule::phrase_event => {
                let end_line = inner.as_span().end_pos().line_col().0;
                let evt = inner.into_inner().next().unwrap();
                match evt.as_rule() {
                    Rule::note_line => items.push(parse_note_line(evt)?),
                    Rule::rest_event => {
                        let position = evt.as_span().start_pos().line_col();
                        items.push(ParsedNote::Single(parse_rest_event(evt)?, position));
                    }
                    Rule::pedal_event => pedals.push(parse_pedal_event(evt)),
                    Rule::pedal_missing_position => return Err(pedal_needs_position(evt)),
                    Rule::hairpin_event => hairpins.push(parse_hairpin_event(evt)),
                    Rule::hairpin_missing_position => return Err(hairpin_needs_position(evt)),
                    Rule::unclosed_chord => return Err(unclosed_chord_error(evt)),
                    Rule::bad_comma_line => return Err(comma_error(evt, previous_end_line)),
                    Rule::bar_line => {} // skip bar lines (visual only)
                    _ => {}
                }
                previous_end_line = Some(end_line);
            }
            _ => {}
        }
    }

    // Sequential model: each event starts where the previous one ended; rests
    // advance the position without sounding. A chord token expands into one
    // event per pitch, all sharing the chord's start and duration. A pedal mark
    // always carries its own position, so it never takes part in the sequence.
    let mut events = Vec::new();
    let mut positions = Vec::new();
    expand_notes(items, &mut events, &mut positions);
    check_gliss_successors(&events, &positions, "phrase")?;

    Ok(Phrase {
        name,
        time_range,
        events,
        pedals,
        hairpins,
        channel,
    })
}

fn parse_pedal_event(pair: Pair) -> PedalEvent {
    let mut beat = 0.0;
    let mut down = true;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::note_position => beat = inner.as_str()[1..].parse().unwrap_or(0.0),
            Rule::pedal_state => down = inner.as_str() == "down",
            _ => {}
        }
    }
    PedalEvent { beat, down }
}

fn parse_hairpin_event(pair: Pair) -> HairpinMark {
    let mut beat = 0.0;
    let mut kind = HairpinKind::Crescendo;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::note_position => beat = inner.as_str()[1..].parse().unwrap_or(0.0),
            Rule::hairpin_kind => {
                kind = HairpinKind::from_keyword(inner.as_str()).unwrap_or(kind);
            }
            _ => {}
        }
    }
    HairpinMark { beat, kind }
}

fn hairpin_needs_position(pair: Pair) -> ParseError {
    unsupported(
        pair,
        "hairpin needs a position: write `@2.0 cresc` or `@2.0 dim`".into(),
    )
}

fn hairpin_in_hits(pair: Pair) -> ParseError {
    unsupported(
        pair,
        "a hairpin needs sustained notes: drum hits are one-shots, so put `@2.0 cresc` in a @phrase or a pattern's `events:` block".into(),
    )
}

fn pedal_needs_position(pair: Pair) -> ParseError {
    unsupported(
        pair,
        "pedal needs a position: write `@2.0 pedal down` or `@4.0 pedal up`".into(),
    )
}

fn pedal_in_hits(pair: Pair) -> ParseError {
    unsupported(
        pair,
        "pedal needs notes: drum hits are one-shots, so put the pedal in a @phrase or in a pattern's `events:` block".into(),
    )
}

fn pedal_outside_phrase(pair: Pair) -> ParseError {
    unsupported(
        pair,
        "pedal belongs to a phrase: write it inside `@phrase` or a pattern's `events:` block"
            .into(),
    )
}

/// A parsed note event before sequential positions are assigned. Events written
/// on one comma-separated line keep their members together until then, so every
/// member gets the line's start.
enum ParsedNote {
    Single(NoteEvent, (usize, usize)),
    Line(Vec<(NoteEvent, (usize, usize))>),
}

/// Assign `start_beat` in sequential order; a line's members all share its
/// start, and the line advances the position by its longest member. `positions`
/// keeps each emitted event's source position for the refusals that need the
/// whole phrase (a gliss mark has to name its destination).
fn expand_notes(
    items: Vec<ParsedNote>,
    events: &mut Vec<NoteEvent>,
    positions: &mut Vec<(usize, usize)>,
) {
    let mut sequential_beat = 0.0;
    for item in items {
        match item {
            ParsedNote::Single(mut event, position) => {
                if !event.explicit_position {
                    event.start_beat = sequential_beat;
                }
                sequential_beat = sequential_beat.max(event.start_beat + event.duration.to_beats());
                events.push(event);
                positions.push(position);
            }
            ParsedNote::Line(members) => {
                let start_beat = if members[0].0.explicit_position {
                    members[0].0.start_beat
                } else {
                    sequential_beat
                };
                let mut end_beat = start_beat;
                for (member, _) in &members {
                    end_beat = end_beat.max(start_beat + member.duration.to_beats());
                }
                sequential_beat = sequential_beat.max(end_beat);
                for (mut member, position) in members {
                    member.start_beat = start_beat;
                    events.push(member);
                    positions.push(position);
                }
            }
        }
    }
}

// ============================================================================
// Note Event
// ============================================================================

/// One source line: `@N <member>, <member>, ...`. Every member shares the
/// line's position; the members may also carry chord tokens, which expand to
/// one event per pitch.
fn parse_note_line(pair: Pair) -> Result<ParsedNote, ParseError> {
    let line_start = pair.as_span().start_pos().line_col().0;
    let mut start_beat = 0.0;
    let mut explicit_position = false;
    let mut grace = false;
    let mut members: Vec<NoteMember> = Vec::new();
    let mut pending_comma: Option<Pair> = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::grace_prefix => grace = true,
            Rule::note_position => {
                let raw = inner.as_str();
                if let Ok(n) = raw[1..].parse::<f64>() {
                    start_beat = n;
                    explicit_position = true;
                }
            }
            Rule::comma => pending_comma = Some(inner),
            Rule::note_member => {
                if let Some(comma) = pending_comma.take() {
                    let (comma_line, comma_col) = comma.as_span().start_pos().line_col();
                    let member_line = inner.as_span().start_pos().line_col().0;
                    if comma_line != line_start || member_line != line_start {
                        return Err(ParseError::Syntax {
                            line: comma_line,
                            col: comma_col,
                            message:
                                "comma list cannot span a line break: keep every member on one line"
                                    .into(),
                        });
                    }
                }
                members.push(parse_note_member(inner)?);
            }
            _ => {}
        }
    }

    let mut seen: Vec<Pitch> = Vec::new();
    let mut events = Vec::new();
    for member in &members {
        let mark =
            crate::gliss::form(&member.event.modifiers).map_err(|message| ParseError::Syntax {
                line: member.line,
                col: member.col,
                message,
            })?;
        // A mark slides from its own note to a destination, so a note that
        // shares its onset with another pitch has no line of its own to slide.
        if members.len() > 1
            && let Some(mark) = mark
        {
            return Err(ParseError::Syntax {
                line: member.line,
                col: member.col,
                message: format!(
                    "{} needs a line of its own: a comma-list member sounds with every other member, so it cannot glide — move the mark to a single note",
                    mark.spelling()
                ),
            });
        }
        if members.len() > 1
            && let Some(ornament) =
                crate::ornament::form(&member.event.modifiers).map_err(|message| {
                    ParseError::Syntax {
                        line: member.line,
                        col: member.col,
                        message,
                    }
                })?
            && ornament.changes_pitch()
        {
            return Err(ParseError::Syntax {
                line: member.line,
                col: member.col,
                message: format!(
                    "ornament={} needs a line of its own: a comma-list member sounds with every other member, so it cannot become a trill or mordent — move the mark to a single note",
                    ornament.as_token()
                ),
            });
        }
        for pitch in &member.pitches {
            if seen.contains(pitch) {
                return Err(ParseError::Syntax {
                    line: member.line,
                    col: member.col,
                    message: format!(
                        "comma list repeats pitch {}: list each pitch once",
                        format_pitch_text(pitch)
                    ),
                });
            }
            seen.push(*pitch);
            events.push((
                NoteEvent {
                    pitch: *pitch,
                    start_beat,
                    explicit_position,
                    grace,
                    ..member.event.clone()
                },
                (member.line, member.col),
            ));
        }
    }

    if events.len() == 1 {
        let (event, position) = events.pop().unwrap();
        Ok(ParsedNote::Single(event, position))
    } else {
        Ok(ParsedNote::Line(events))
    }
}

struct NoteMember {
    pitches: Vec<Pitch>,
    event: NoteEvent,
    line: usize,
    col: usize,
}

fn parse_note_member(pair: Pair) -> Result<NoteMember, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    let mut pitches = None;
    let mut duration = None;
    let mut dynamics = None;
    let mut articulation = None;
    let mut vibrato = None;
    let mut expression = None;
    let mut pitch_bend = None;
    let mut modifiers = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::note_head => {
                let head = inner.into_inner().next().unwrap();
                match head.as_rule() {
                    Rule::pitch => pitches = Some(vec![parse_pitch(head)?]),
                    Rule::chord => pitches = Some(parse_chord(head)?),
                    _ => {}
                }
            }
            Rule::duration => duration = Some(parse_duration(inner)?),
            Rule::dynamics => dynamics = Some(parse_dynamics(inner)?),
            Rule::note_modifier => {
                let modifier = inner.into_inner().next().unwrap();
                match modifier.as_rule() {
                    Rule::articulation_mod => {
                        let val = modifier.into_inner().next().unwrap();
                        articulation = Some(parse_articulation(val)?);
                    }
                    Rule::vibrato_mod => {
                        vibrato = Some(parse_vibrato(modifier)?);
                    }
                    Rule::expression_mod => {
                        expression
                            .get_or_insert_with(Vec::new)
                            .extend(parse_expression(modifier)?);
                    }
                    Rule::pitch_bend_mod => {
                        let cents = modifier.into_inner().next().unwrap();
                        pitch_bend = Some(parse_cents_value(cents));
                    }
                    Rule::gliss_mod => {
                        let (mark_line, mark_col) = modifier.as_span().start_pos().line_col();
                        let value =
                            parse_gliss_value(modifier).map_err(|message| ParseError::Syntax {
                                line: mark_line,
                                col: mark_col,
                                message,
                            })?;
                        modifiers.push((crate::gliss::MODIFIER.to_string(), value));
                    }
                    Rule::generic_mod => {
                        let mut gm = modifier.into_inner();
                        let key = gm.next().unwrap().as_str().to_string();
                        let val = parse_param_value(gm.next().unwrap());
                        modifiers.push((key, val));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Extract well-known modifiers into dedicated fields
    let muted = modifiers.iter().any(|(k, v)| {
        k == "muted"
            && (v == &ParamValue::Bool(true) || v.as_f64().map(|f| f != 0.0).unwrap_or(false))
    });
    modifiers.retain(|(k, _)| k != "muted");

    let velocity = modifiers
        .iter()
        .find(|(k, _)| k == "v")
        .and_then(|(_, v)| v.as_f64().map(|f| f as u8));
    modifiers.retain(|(k, _)| k != "v");

    let text = modifiers
        .iter()
        .find(|(k, _)| k == "text")
        .and_then(|(_, v)| match v {
            ParamValue::String(s) => Some(s.clone()),
            _ => None,
        });
    modifiers.retain(|(k, _)| k != "text");

    let pitches = pitches.ok_or_else(|| ParseError::InvalidPitch("missing pitch".into()))?;
    let mark = crate::gliss::form(&modifiers).map_err(|message| ParseError::Syntax {
        line,
        col,
        message,
    })?;
    if let Some(mark) = mark {
        if pitches.len() > 1 {
            return Err(ParseError::Syntax {
                line,
                col,
                message: format!(
                    "{} needs a line of its own: a chord member sounds with the other pitches, so it cannot glide — move the mark to a single note",
                    mark.spelling()
                ),
            });
        }
        // Every other pitch control is channel-wide or note-wide; a mark writes
        // the note's own pitch curve, so it cannot share the job.
        if let Some(conflict) = [
            ("bend", pitch_bend.is_some()),
            ("vibrato", vibrato.is_some()),
            ("expression", expression.is_some()),
            (
                "pitch_curve",
                modifiers.iter().any(|(k, _)| k == "pitch_curve"),
            ),
        ]
        .into_iter()
        .find_map(|(name, present)| present.then_some(name))
        {
            return Err(ParseError::Syntax {
                line,
                col,
                message: format!(
                    "{} writes the note's own pitch curve, so it cannot share the note with {conflict}=: remove one of them",
                    mark.spelling()
                ),
            });
        }
    }
    let event = NoteEvent {
        start_beat: 0.0,
        explicit_position: false,
        pitch: pitches[0],
        duration: duration.ok_or_else(|| ParseError::InvalidDuration("missing duration".into()))?,
        dynamics,
        velocity,
        articulation,
        vibrato,
        expression,
        pitch_bend,
        muted,
        grace: false,
        text,
        modifiers,
    };
    if let Some(ornament) = crate::ornament::form(&event.modifiers)
        .map_err(|message| ParseError::Syntax { line, col, message })?
    {
        crate::ornament::check_combinations(ornament, &event)
            .map_err(|message| ParseError::Syntax { line, col, message })?;
    }
    Ok(NoteMember {
        pitches,
        event,
        line,
        col,
    })
}

/// A malformed comma line. `previous_end_line` is the line of the last event
/// the phrase produced, which separates a trailing comma from a leading one.
fn comma_error(pair: Pair, previous_end_line: Option<usize>) -> ParseError {
    let fallback_span = pair.as_span();
    let mut comma = None;
    let mut position = None;
    let mut tail = String::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::note_position => position = Some(inner.as_str().to_string()),
            Rule::comma => comma = Some(inner.as_span()),
            Rule::line_tail => tail = inner.as_str().to_string(),
            _ => {}
        }
    }
    let span = comma.unwrap_or(fallback_span);
    let (line, col) = span.start_pos().line_col();
    let tail = tail.trim();
    let message = match (position.as_deref(), tail.is_empty()) {
        // `@0.0 ,`: a position with no event at all.
        (Some(position), true) => {
            format!("position '{position}' has no note event: write e.g. '{position} G4 q mf'")
        }
        // A comma with an event after it is a leading comma: nothing precedes it
        // as the first member of the line.
        (Some(_), false) => leading_comma_message(),
        (None, _) if previous_end_line == Some(line) => {
            if tail.is_empty() {
                "trailing comma: remove the ',' or add another note event after it".to_string()
            } else {
                member_problem(tail)
            }
        }
        (None, _) => leading_comma_message(),
    };
    ParseError::Syntax { line, col, message }
}

fn leading_comma_message() -> String {
    "comma list starts with ',': write the first note event before the comma".to_string()
}

fn member_problem(tail: &str) -> String {
    let token = tail.split_whitespace().next().unwrap_or("");
    if token == "rest" {
        return "rest is not a comma list member: write the rest on its own line".to_string();
    }
    if token.starts_with('@') {
        return "a comma list member shares the line's position: write it on its own line"
            .to_string();
    }
    if parse_pitch_text(token).is_some() {
        return format!("comma list member '{token}' has no duration: write e.g. '{token} q mf'");
    }
    format!("expected a note event after ',': found '{token}'")
}

fn unclosed_chord_error(pair: Pair) -> ParseError {
    let (line, col) = pair.as_span().start_pos().line_col();
    ParseError::Syntax {
        line,
        col,
        message: "unclosed chord: expected ']' on the same line".into(),
    }
}

/// Parse a chord token's contents. The token text is `[<member> <member> ...]`;
/// whitespace both inside and around members is insignificant.
fn parse_chord(pair: Pair) -> Result<Vec<Pitch>, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    let text = pair.as_str();
    let inner = &text[1..text.len() - 1];
    let refuse = |message: String| ParseError::Syntax { line, col, message };

    let mut pitches: Vec<Pitch> = Vec::new();
    for member in inner.split_whitespace() {
        if member == "rest" {
            return Err(refuse(
                "rest is not a chord member: write the rest outside the brackets".into(),
            ));
        }
        let pitch = parse_pitch_text(member).ok_or_else(|| {
            refuse(format!(
                "invalid chord member '{member}': expected a pitch like C4, F#3, or Bb5"
            ))
        })?;
        if pitches.contains(&pitch) {
            return Err(refuse(format!(
                "chord repeats pitch {member}: list each pitch once"
            )));
        }
        pitches.push(pitch);
    }
    if pitches.is_empty() {
        return Err(refuse(
            "empty chord: expected at least one pitch between '[' and ']'".into(),
        ));
    }
    Ok(pitches)
}

fn parse_rest_event(pair: Pair) -> Result<NoteEvent, ParseError> {
    let dur_pair = pair.into_inner().next().unwrap();
    let duration = parse_duration(dur_pair)?;

    // A rest is a note event with a special "rest" representation
    // We use C0 as a sentinel pitch for rests
    Ok(NoteEvent {
        start_beat: 0.0,
        explicit_position: false,
        pitch: Pitch {
            note: NoteName::C,
            accidental: Accidental::Natural,
            octave: 0,
        },
        duration,
        dynamics: None,
        articulation: None,
        vibrato: None,
        expression: None,
        pitch_bend: None,
        velocity: None,
        muted: false,
        grace: false,
        text: None,
        modifiers: vec![("rest".to_string(), ParamValue::Bool(true))],
    })
}

// ============================================================================
// Pitch
// ============================================================================

fn parse_pitch(pair: Pair) -> Result<Pitch, ParseError> {
    parse_pitch_text(pair.as_str())
        .ok_or_else(|| ParseError::InvalidPitch(pair.as_str().to_string()))
}

/// Parse one scientific pitch (`C4`, `F#3`, `Bb5`); `None` when the text is not
/// a pitch. Shared by the grammar-driven path and chord members.
fn parse_pitch_text(text: &str) -> Option<Pitch> {
    Pitch::from_text(text)
}

/// Render one pitch (`C4`, `F#3`, `Bb5`) for diagnostics.
fn format_pitch_text(pitch: &Pitch) -> String {
    let note = match pitch.note {
        NoteName::C => "C",
        NoteName::D => "D",
        NoteName::E => "E",
        NoteName::F => "F",
        NoteName::G => "G",
        NoteName::A => "A",
        NoteName::B => "B",
    };
    let accidental = match pitch.accidental {
        Accidental::Natural => "",
        Accidental::Sharp => "#",
        Accidental::DoubleSharp => "##",
        Accidental::Flat => "b",
        Accidental::DoubleFlat => "bb",
    };
    format!("{note}{accidental}{}", pitch.octave)
}

// ============================================================================
// Duration
// ============================================================================

fn parse_duration(pair: Pair) -> Result<Duration, ParseError> {
    let s = pair.as_str();

    // Handle numeric multiplier: "8w" = 8 whole notes (for multi-bar rests)
    if s.len() >= 2 && s.as_bytes()[0].is_ascii_digit() && s.ends_with('w') {
        let _multiplier: f64 = s[..s.len() - 1]
            .parse()
            .map_err(|_| ParseError::InvalidDuration(s.to_string()))?;
        return Ok(Duration {
            base: BaseDuration::Whole,
            dots: 0,
            triplet: false,
            tuplet: None,
            ties: vec![],
        });
    }

    // Parse base duration
    let base = parse_base_from_str(s)?;
    let rest = &s[base_duration_len(base)..];

    // Parse dots
    let (dots, rest) = if let Some(stripped) = rest.strip_prefix("..") {
        (2, stripped)
    } else if let Some(stripped) = rest.strip_prefix('.') {
        (1, stripped)
    } else {
        (0, rest)
    };

    // Parse tuplet: "5:4" for quintuplet, "3:2" for triplet, "7:8" for septuplet
    let (tuplet, rest) = if !rest.is_empty() && rest.as_bytes()[0].is_ascii_digit() {
        let colon = rest.find(':').unwrap_or(rest.len());
        if colon < rest.len() {
            let n: u8 = rest[..colon].parse().unwrap_or(3);
            let after_colon = &rest[colon + 1..];
            let num_end = after_colon
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after_colon.len());
            let m: u8 = after_colon[..num_end].parse().unwrap_or(2);
            (Some((n, m)), &after_colon[num_end..])
        } else {
            (None, rest)
        }
    } else {
        (None, rest)
    };

    // Parse classic triplet "t"
    let is_triplet = tuplet.is_none() && rest.starts_with('t');
    let rest_after_t = if is_triplet { &rest[1..] } else { rest };
    let rest_final = if tuplet.is_some() { rest } else { rest_after_t };

    // Parse multi-tie chain: ~q~q~h
    let mut ties = Vec::new();
    let mut rest = rest_final;
    while rest.starts_with('~') {
        let after = &rest[1..];
        if let Ok(tb) = parse_base_from_str(after) {
            ties.push(tb);
            let skip = base_duration_len(tb);
            rest = &after[skip..];
            if rest.starts_with("..") {
                rest = &rest[2..];
            } else if rest.starts_with('.') {
                rest = &rest[1..];
            }
        } else {
            break;
        }
    }

    Ok(Duration {
        base,
        dots,
        triplet: is_triplet,
        tuplet,
        ties,
    })
}

fn parse_base_from_str(s: &str) -> Result<BaseDuration, ParseError> {
    if s.starts_with("64th") {
        Ok(BaseDuration::SixtyFourth)
    } else if s.starts_with("32nd") {
        Ok(BaseDuration::ThirtySecond)
    } else if s.starts_with("16th") {
        Ok(BaseDuration::Sixteenth)
    } else if s.starts_with("8th") {
        Ok(BaseDuration::Eighth)
    } else if s.starts_with('w') {
        Ok(BaseDuration::Whole)
    } else if s.starts_with('h') {
        Ok(BaseDuration::Half)
    } else if s.starts_with('q') {
        Ok(BaseDuration::Quarter)
    } else {
        Err(ParseError::InvalidDuration(s.to_string()))
    }
}

fn base_duration_len(d: BaseDuration) -> usize {
    match d {
        BaseDuration::SixtyFourth => 4,
        BaseDuration::ThirtySecond => 4,
        BaseDuration::Sixteenth => 4,
        BaseDuration::Eighth => 3,
        _ => 1,
    }
}

// ============================================================================
// Dynamics
// ============================================================================

fn parse_dynamics(pair: Pair) -> Result<Dynamics, ParseError> {
    Dynamics::from_token(pair.as_str())
        .ok_or_else(|| ParseError::InvalidDynamics(pair.as_str().to_owned()))
}

// ============================================================================
// Articulation
// ============================================================================

fn parse_articulation(pair: Pair) -> Result<Articulation, ParseError> {
    Articulation::from_token(pair.as_str()).ok_or_else(|| ParseError::Syntax {
        line: 0,
        col: 0,
        message: format!("Unknown articulation: '{}'", pair.as_str()),
    })
}

// ============================================================================
// Vibrato & Expression
// ============================================================================

fn parse_vibrato(pair: Pair) -> Result<VibratoParams, ParseError> {
    let mut rate_hz = None;
    let mut depth_cents = None;
    let mut delay_s = None;

    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::vibrato_params {
            for param in inner.into_inner() {
                if param.as_rule() != Rule::vibrato_param {
                    continue;
                }
                let vp = param.into_inner().next().unwrap();
                match vp.as_rule() {
                    Rule::vibrato_rate => {
                        let fv = vp.into_inner().next().unwrap();
                        rate_hz = Some(parse_freq_value(fv));
                    }
                    Rule::vibrato_depth => {
                        let cv = vp.into_inner().next().unwrap();
                        depth_cents = Some(parse_cents_value(cv));
                    }
                    Rule::vibrato_delay => {
                        let tv = vp.into_inner().next().unwrap();
                        delay_s = Some(parse_time_value(tv));
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(VibratoParams {
        rate_hz,
        depth_cents,
        delay_s,
    })
}

fn parse_expression(pair: Pair) -> Result<Vec<ExpressionCurve>, ParseError> {
    let mut curves = Vec::new();

    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::expression_params {
            for param in inner.into_inner() {
                if param.as_rule() != Rule::expression_param {
                    continue;
                }
                let mut parts = param.into_inner();
                let param_name = parts.next().unwrap().as_str().to_string();
                let expr_value = parts.next().unwrap();

                let mut atoms = expr_value.into_inner();
                let start = parse_expr_atom(atoms.next().unwrap());
                let end = atoms.next().map(parse_expr_atom);

                curves.push(ExpressionCurve {
                    param: param_name,
                    start,
                    end,
                });
            }
        }
    }

    Ok(curves)
}

/// Parse an expression atom: either a dynamics name or a param_value
fn parse_expr_atom(pair: Pair) -> ParamValue {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::dynamics => {
            // Convert dynamics name to a string value (e.g. "mp", "ff")
            ParamValue::String(inner.as_str().to_string())
        }
        Rule::param_value => parse_param_value(inner),
        _ => ParamValue::String(inner.as_str().to_string()),
    }
}

// ============================================================================
// Pattern (drums)
// ============================================================================

fn parse_pattern(pair: Pair) -> Result<Pattern, ParseError> {
    let mut name = String::new();
    let mut time_range = None;
    let mut repeat = 1;
    let mut hits = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::pattern_header => {
                for h in inner.into_inner() {
                    match h.as_rule() {
                        Rule::identifier | Rule::string_literal => name = name_of(h),
                        Rule::time_range => time_range = Some(parse_time_range(h)),
                        Rule::repeat_count => {
                            // atomic rule: "x4" → strip "x" prefix, parse number
                            let s = h.as_str();
                            repeat = s[1..].parse().unwrap_or(1);
                        }
                        _ => {}
                    }
                }
            }
            Rule::pattern_event => {
                let evt = inner.into_inner().next().unwrap();
                match evt.as_rule() {
                    Rule::hit_event => hits.push(parse_hit_event(evt)?),
                    Rule::pedal_in_hits => return Err(pedal_in_hits(evt)),
                    Rule::hairpin_in_pattern => return Err(hairpin_in_hits(evt)),
                    Rule::pattern_rest => {} // skip rests (they're silence)
                    Rule::bar_line => {}     // skip bar lines (visual only)
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(Pattern {
        name,
        time_range,
        repeat,
        hits,
    })
}

fn parse_hit_event(pair: Pair) -> Result<HitEvent, ParseError> {
    let mut drum = String::new();
    let mut position = BeatPosition {
        beat: 1,
        subdivision: 0,
    };
    let mut dynamics = None;
    let mut articulation = None;
    let mut modifiers = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::drum_name => drum = inner.as_str().to_string(),
            Rule::beat_position => {
                let mut parts = inner.into_inner();
                let beat = parts.next().unwrap().as_str().parse().unwrap_or(1);
                let sub = parts.next().unwrap().as_str().parse().unwrap_or(0);
                position = BeatPosition {
                    beat,
                    subdivision: sub,
                };
            }
            Rule::dynamics => dynamics = Some(parse_dynamics(inner)?),
            Rule::hit_modifier => {
                let m = inner.into_inner().next().unwrap();
                match m.as_rule() {
                    Rule::hit_articulation_mod => {
                        let val = m.into_inner().next().unwrap();
                        articulation = Some(parse_drum_articulation(val)?);
                    }
                    Rule::generic_mod => {
                        let mut gm = m.into_inner();
                        let key = gm.next().unwrap().as_str().to_string();
                        let val = parse_param_value(gm.next().unwrap());
                        modifiers.push((key, val));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Hit velocity uses the same `v=NN` token a note uses; it is a typed field,
    // not a generic modifier, so the engine and the roll see one value.
    let velocity = modifiers
        .iter()
        .find(|(k, _)| k == "v")
        .and_then(|(_, v)| v.as_f64().map(|f| f as u8));
    modifiers.retain(|(k, _)| k != "v");

    Ok(HitEvent {
        drum,
        position,
        dynamics,
        velocity,
        articulation,
        modifiers,
    })
}

fn parse_drum_articulation(pair: Pair) -> Result<DrumArticulation, ParseError> {
    match pair.as_str() {
        "closed" => Ok(DrumArticulation::Closed),
        "open" => Ok(DrumArticulation::Open),
        "half_open" => Ok(DrumArticulation::HalfOpen),
        "ghost_note" => Ok(DrumArticulation::GhostNote),
        "flam" => Ok(DrumArticulation::Flam),
        "roll" => Ok(DrumArticulation::Roll),
        "rim_shot" => Ok(DrumArticulation::RimShot),
        "cross_stick" => Ok(DrumArticulation::CrossStick),
        other => Err(ParseError::Syntax {
            line: 0,
            col: 0,
            message: format!("Unknown drum articulation: '{}'", other),
        }),
    }
}

// ============================================================================
// Automation
// ============================================================================

fn parse_automation(pair: Pair) -> Result<AutomationLane, ParseError> {
    let mut param = String::new();
    let mut time_range = None;
    let mut muted = false;
    let mut ppq = 0;
    let mut smoothing_ms = 0.0;
    let mut mode = LaneMode::Envelope;
    let mut lfo = LaneLfo::default();
    let mut points = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::automation_target => param = inner.as_str().to_string(),
            Rule::automation_time_range => {
                let mut edges = inner.into_inner().map(parse_bar_beat);
                if let (Some(start), Some(end)) = (edges.next(), edges.next()) {
                    time_range = Some(TimeRange { start, end });
                }
            }
            Rule::automation_muted => muted = true,
            Rule::automation_ppq => {
                ppq = inner
                    .into_inner()
                    .next()
                    .and_then(|value| value.as_str().parse().ok())
                    .unwrap_or(0);
            }
            Rule::automation_smoothing => {
                smoothing_ms = inner
                    .into_inner()
                    .next()
                    .map(parse_time_value)
                    .unwrap_or(0.0)
                    * 1000.0;
            }
            Rule::automation_mode => {
                mode = inner
                    .into_inner()
                    .next()
                    .and_then(|name| LaneMode::from_keyword(name.as_str()))
                    .unwrap_or(LaneMode::Envelope);
            }
            Rule::automation_speed => {
                if let Some(speed) = inner
                    .into_inner()
                    .next()
                    .and_then(|pair| parse_lfo_speed(pair))
                {
                    lfo.speed = speed;
                }
            }
            Rule::automation_mult => {
                lfo.mult = inner
                    .into_inner()
                    .next()
                    .map(parse_number)
                    .unwrap_or(lfo.mult);
            }
            Rule::automation_pw => {
                lfo.pulse_width = inner
                    .into_inner()
                    .next()
                    .map(parse_number)
                    .unwrap_or(lfo.pulse_width);
            }
            Rule::automation_skew => {
                lfo.skew = inner
                    .into_inner()
                    .next()
                    .map(parse_number)
                    .unwrap_or(lfo.skew);
            }
            Rule::automation_tension => {
                lfo.tension = inner
                    .into_inner()
                    .next()
                    .map(parse_number)
                    .unwrap_or(lfo.tension);
            }
            Rule::automation_amount => {
                lfo.amount = inner
                    .into_inner()
                    .next()
                    .map(parse_number)
                    .unwrap_or(lfo.amount);
            }
            Rule::automation_points => {
                for ap in inner.into_inner() {
                    if ap.as_rule() == Rule::automation_point {
                        let mut parts = ap.into_inner();
                        let pos = parse_bar_beat(parts.next().unwrap());
                        let val = parse_param_value(parts.next().unwrap());
                        let mut curve = Curve::Linear;
                        let mut tension = None;
                        for attribute in parts {
                            match attribute.as_rule() {
                                Rule::automation_curve => {
                                    curve = attribute
                                        .into_inner()
                                        .next()
                                        .and_then(|name| Curve::from_keyword(name.as_str()))
                                        .unwrap_or(Curve::Linear);
                                }
                                Rule::automation_tension => {
                                    tension = attribute
                                        .into_inner()
                                        .next()
                                        .and_then(|value| value.as_str().parse().ok());
                                }
                                _ => {}
                            }
                        }
                        points.push(AutomationPoint {
                            position: pos,
                            value: val,
                            curve,
                            tension,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(AutomationLane {
        param,
        time_range,
        muted,
        ppq,
        smoothing_ms,
        mode,
        lfo,
        points,
    })
}

/// `speed=2.5Hz` is absolute; `speed=1/4` is a note length that follows tempo.
fn parse_lfo_speed(pair: Pair) -> Option<LfoSpeed> {
    match pair.as_rule() {
        Rule::lfo_speed_hz => pair.into_inner().next().map(|value| LfoSpeed::Hz {
            hz: parse_number(value),
        }),
        Rule::lfo_speed_note => {
            let mut parts = pair.into_inner();
            let numerator = parts.next()?.as_str().parse().ok()?;
            let denominator = parts.next()?.as_str().parse().ok()?;
            Some(LfoSpeed::Note {
                numerator,
                denominator,
            })
        }
        _ => None,
    }
}

// ============================================================================
// Control links
// ============================================================================

fn parse_control_link(pair: Pair) -> Result<ControlLink, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    let mut name = String::new();
    let mut source = None;
    let mut targets = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => name = name_of(inner),
            Rule::control_link_source => source = Some(parse_control_link_source(inner)?),
            Rule::control_link_target => targets.push(parse_control_link_target(inner)?),
            _ => {}
        }
    }
    let source = source.ok_or_else(|| ParseError::Syntax {
        line,
        col,
        message: format!("@control_link '{name}' needs `from:`"),
    })?;
    Ok(ControlLink {
        name,
        source,
        targets,
    })
}

fn parse_control_link_source(pair: Pair) -> Result<ControlLinkSource, ParseError> {
    let value = pair
        .into_inner()
        .next()
        .and_then(|value| value.into_inner().next())
        .ok_or_else(|| ParseError::Syntax {
            line: 0,
            col: 0,
            message: "empty @control_link source".into(),
        })?;
    let numbers = |pair: Pair| -> Vec<u8> {
        pair.into_inner()
            .map(|number| number.as_str().parse::<u8>().unwrap_or(0))
            .collect()
    };
    match value.as_rule() {
        Rule::control_link_cc => {
            let values = numbers(value);
            Ok(ControlLinkSource::MidiCc {
                controller: *values.first().unwrap_or(&0),
                channel: values.get(1).copied().unwrap_or(1),
            })
        }
        Rule::control_link_macro => {
            let values = numbers(value);
            Ok(ControlLinkSource::Macro {
                index: *values.first().unwrap_or(&1),
            })
        }
        Rule::control_link_lane => {
            let mut names = value.into_inner().map(name_of);
            let track = names.next().unwrap_or_default();
            let param = names.next().unwrap_or_default();
            Ok(ControlLinkSource::Lane { track, param })
        }
        other => Err(ParseError::Syntax {
            line: 0,
            col: 0,
            message: format!("unsupported @control_link source '{other:?}'"),
        }),
    }
}

fn parse_control_link_target(pair: Pair) -> Result<ControlLinkTarget, ParseError> {
    let (line, col) = pair.as_span().start_pos().line_col();
    let body = pair
        .into_inner()
        .flat_map(|target| target.into_inner())
        .find(|body| body.as_rule() == Rule::control_link_target_body)
        .ok_or_else(|| ParseError::Syntax {
            line,
            col,
            message: "empty @control_link target".into(),
        })?;
    let mut track = String::new();
    let mut param = String::new();
    let mut formula = LinkFormula::Linear;
    let mut smoothing_ms = 0u32;
    for inner in body.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => track = name_of(inner),
            Rule::control_link_param => {
                param = inner.into_inner().next().map(name_of).unwrap_or_default();
            }
            Rule::control_link_formula => {
                formula = inner
                    .as_str()
                    .parse::<LinkFormula>()
                    .map_err(|message| ParseError::Syntax { line, col, message })?;
            }
            Rule::control_link_smoothing => {
                let value = inner
                    .into_inner()
                    .next()
                    .ok_or_else(|| ParseError::Syntax {
                        line,
                        col,
                        message: "empty smoothing time".into(),
                    })?;
                smoothing_ms = (parse_time_value(value) * 1000.0).round().max(0.0) as u32;
            }
            _ => {}
        }
    }
    Ok(ControlLinkTarget {
        track,
        param,
        formula,
        smoothing_ms,
    })
}

// ============================================================================
// Bus, Master, Section
// ============================================================================

fn parse_gain_automation(pair: Pair) -> Result<GainAutomation, ParseError> {
    let mut parts = pair.into_inner();
    let interpolation = if parts.next().unwrap().as_str() == "step" {
        GainInterpolation::Step
    } else {
        GainInterpolation::Linear
    };
    let mut range = parts.next().unwrap();
    let ppq = if range.as_rule() == Rule::gain_ppq {
        let ppq = range
            .into_inner()
            .next()
            .unwrap()
            .as_str()
            .parse()
            .unwrap_or(0);
        range = parts.next().unwrap();
        ppq
    } else {
        4
    };
    let time_range = parse_time_range(range);
    let points = parts
        .next()
        .unwrap()
        .into_inner()
        .map(|point| {
            let mut values = point.into_inner();
            AutomationPoint {
                position: parse_bar_beat(values.next().unwrap()),
                value: parse_param_value(values.next().unwrap()),
                curve: Curve::Linear,
                tension: None,
            }
        })
        .collect();
    Ok(GainAutomation {
        time_range,
        interpolation,
        ppq,
        points,
    })
}

fn parse_bus(pair: Pair) -> Result<Bus, ParseError> {
    let mut name = String::new();
    let mut routes = Vec::new();
    let mut effects = Vec::new();
    let mut gain_automation = Vec::new();
    let mut automation = Vec::new();
    let mut volume_db = 0.0;
    let mut mute = false;
    let mut solo = false;
    let mut input_routes = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::bus_header => {
                for h in inner.into_inner() {
                    if h.as_rule() == Rule::identifier || h.as_rule() == Rule::string_literal {
                        name = name_of(h);
                    }
                }
            }
            Rule::bus_content => {
                let content = inner.into_inner().next().unwrap();
                match content.as_rule() {
                    Rule::routes_line => {
                        for r in content.into_inner() {
                            if r.as_rule() == Rule::name_list {
                                for id in r.into_inner() {
                                    if id.as_rule() == Rule::identifier
                                        || id.as_rule() == Rule::string_literal
                                    {
                                        routes.push(name_of(id));
                                    }
                                }
                            }
                        }
                    }
                    Rule::effects_block => {
                        effects = parse_effects_block(content)?;
                    }
                    Rule::route_line => input_routes.push(parse_route_line(content)?),
                    Rule::bus_mix_line => {
                        for prop in content.into_inner() {
                            let inner = prop.into_inner().next().unwrap();
                            match inner.as_rule() {
                                Rule::vol_prop => {
                                    volume_db = parse_db_value(inner.into_inner().next().unwrap());
                                }
                                Rule::mute_prop => {
                                    mute = inner.into_inner().next().unwrap().as_str() == "true";
                                }
                                Rule::solo_prop => {
                                    solo = inner.into_inner().next().unwrap().as_str() == "true";
                                }
                                _ => {}
                            }
                        }
                    }
                    Rule::gain_automation_line => {
                        gain_automation.push(parse_gain_automation(content)?)
                    }
                    Rule::automation_line => automation.push(parse_automation(content)?),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(Bus {
        name,
        routes,
        effects,
        gain_automation,
        automation,
        volume_db,
        mute,
        solo,
        input_routes,
    })
}

fn parse_master(pair: Pair) -> Result<Master, ParseError> {
    let mut effects = Vec::new();
    let mut loudness_target = None;
    let mut gain_automation = Vec::new();
    let mut automation = Vec::new();
    let mut volume_db = 0.0;
    let mut input_routes = Vec::new();

    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::master_content {
            let content = inner.into_inner().next().unwrap();
            match content.as_rule() {
                Rule::effects_block => {
                    effects = parse_effects_block(content)?;
                }
                Rule::route_line => input_routes.push(parse_route_line(content)?),
                Rule::loudness_line => {
                    let db = content.into_inner().next().unwrap();
                    loudness_target = Some(parse_db_value(db));
                }
                Rule::master_volume_line => {
                    let db = content.into_inner().next().unwrap();
                    volume_db = parse_db_value(db);
                }
                Rule::gain_automation_line => gain_automation.push(parse_gain_automation(content)?),
                Rule::automation_line => automation.push(parse_automation(content)?),
                _ => {}
            }
        }
    }

    Ok(Master {
        effects,
        loudness_target,
        gain_automation,
        automation,
        volume_db,
        input_routes,
    })
}

fn parse_marker(pair: Pair) -> Marker {
    let mut name = String::new();
    let mut position = BarBeat::new(1, 1, 0);
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::string_literal | Rule::identifier => name = name_of(inner),
            Rule::bar_beat => position = parse_bar_beat(inner),
            _ => {}
        }
    }
    Marker { name, position }
}

fn parse_group(pair: Pair) -> ClipGroup {
    let mut id = String::new();
    let mut members = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::string_literal | Rule::identifier => id = name_of(inner),
            Rule::group_members_prop => {
                members = inner
                    .into_inner()
                    .flat_map(|list| list.into_inner())
                    .map(name_of)
                    .collect();
            }
            _ => {}
        }
    }
    ClipGroup { id, members }
}

fn parse_section(pair: Pair) -> Result<Section, ParseError> {
    let mut name = String::new();
    let mut time_range = TimeRange {
        start: BarBeat::new(1, 1, 0),
        end: BarBeat::new(1, 1, 0),
    };
    let mut repeat = None;
    let mut label = None;
    let mut flags = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => name = name_of(inner),
            Rule::time_range => time_range = parse_time_range(inner),
            Rule::section_content => {
                let content = inner.into_inner().next().unwrap();
                match content.as_rule() {
                    Rule::repeat_line => {
                        let num = content.into_inner().next().unwrap();
                        repeat = Some(num.as_str().parse().unwrap_or(1));
                    }
                    Rule::label_line => {
                        let s = content.into_inner().next().unwrap();
                        label = Some(parse_string_literal(s));
                    }
                    Rule::flags_line => {
                        if flags.is_some() {
                            return Err(ParseError::Syntax {
                                line: content.as_span().start_pos().line_col().0,
                                col: content.as_span().start_pos().line_col().1,
                                message: "a section carries one `flags:` line".into(),
                            });
                        }
                        let value = content.into_inner().next().unwrap();
                        flags = Some(parse_sound_flag(value)?);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(Section {
        name,
        time_range,
        repeat,
        label,
        flags,
    })
}

fn parse_sound_flag(pair: Pair) -> Result<Articulation, ParseError> {
    match pair.as_str() {
        "pizz" | "pizzicato" => Ok(Articulation::Pizzicato),
        "arco" => Ok(Articulation::Arco),
        "con_sord" | "muted" => Ok(Articulation::Muted),
        "sul_pont" | "sul_ponticello" => Ok(Articulation::SulPonticello),
        "sul_tasto" => Ok(Articulation::SulTasto),
        other => Err(ParseError::Syntax {
            line: pair.as_span().start_pos().line_col().0,
            col: pair.as_span().start_pos().line_col().1,
            message: format!("unknown section sound flag '{other}'"),
        }),
    }
}

// ============================================================================
// Key-value list (for engine_params)
// ============================================================================

/// Parse a `key_value_list` pair's direct children (pad/layer inline props).
fn parse_key_value_pairs(pair: Pair) -> Result<Vec<(String, ParamValue)>, ParseError> {
    let mut result = Vec::new();
    for kv in pair.into_inner() {
        if kv.as_rule() == Rule::key_value_pair {
            let mut parts = kv.into_inner();
            let key = parts.next().unwrap().as_str().to_string();
            let val = parse_param_value(parts.next().unwrap());
            result.push((key, val));
        }
    }
    Ok(result)
}

fn parse_key_value_list(pair: Pair) -> Result<Vec<(String, ParamValue)>, ParseError> {
    let mut result = Vec::new();

    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::key_value_list {
            for kv in inner.into_inner() {
                if kv.as_rule() == Rule::key_value_pair {
                    let mut parts = kv.into_inner();
                    let key = parts.next().unwrap().as_str().to_string();
                    let val = parse_param_value(parts.next().unwrap());
                    result.push((key, val));
                }
            }
        }
    }

    Ok(result)
}

// ============================================================================
// Primitive value parsers
// ============================================================================

fn parse_time_range(pair: Pair) -> TimeRange {
    let mut inner = pair.into_inner();
    let start = parse_bar_beat(inner.next().unwrap());
    // End is optional — if absent, use start as end (single position)
    let end = inner.next().map(parse_bar_beat).unwrap_or(start);
    TimeRange { start, end }
}

fn parse_bar_beat(pair: Pair) -> BarBeat {
    let mut parts = pair.into_inner();
    let bar = parts.next().unwrap().as_str().parse().unwrap_or(1);
    let beat = parts.next().unwrap().as_str().parse().unwrap_or(1);
    let sub = parts
        .next()
        .map(|p| p.as_str().parse().unwrap_or(0))
        .unwrap_or(0);
    BarBeat::new(bar, beat, sub)
}

fn parse_time_signature(pair: Pair) -> TimeSignature {
    let mut parts = pair.into_inner();
    let num = parts.next().unwrap().as_str().parse().unwrap_or(4);
    let den = parts.next().unwrap().as_str().parse().unwrap_or(4);
    TimeSignature {
        numerator: num,
        denominator: den,
    }
}

fn parse_key_signature(pair: Pair) -> KeySignature {
    let mut note = NoteName::C;
    let mut accidental = Accidental::Natural;
    let mut mode = KeyMode::Major;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::note_name => {
                note = match inner.as_str() {
                    "C" => NoteName::C,
                    "D" => NoteName::D,
                    "E" => NoteName::E,
                    "F" => NoteName::F,
                    "G" => NoteName::G,
                    "A" => NoteName::A,
                    "B" => NoteName::B,
                    _ => NoteName::C,
                };
            }
            Rule::accidental => {
                accidental = match inner.as_str() {
                    "#" => Accidental::Sharp,
                    "##" => Accidental::DoubleSharp,
                    "b" => Accidental::Flat,
                    "bb" => Accidental::DoubleFlat,
                    _ => Accidental::Natural,
                };
            }
            Rule::key_mode => {
                mode = match inner.as_str() {
                    "major" | "maj" => KeyMode::Major,
                    "minor" | "min" => KeyMode::Minor,
                    _ => KeyMode::Major,
                };
            }
            _ => {}
        }
    }

    KeySignature {
        root: note,
        accidental,
        mode,
    }
}

fn parse_param_value(pair: Pair) -> ParamValue {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::db_value => ParamValue::Db(parse_db_value(inner)),
        Rule::percentage => ParamValue::Percentage(parse_percentage(inner)),
        Rule::freq_value => ParamValue::Frequency(parse_freq_value(inner)),
        Rule::cents_value => ParamValue::Cents(parse_cents_value(inner)),
        Rule::time_value => ParamValue::Time(parse_time_value(inner)),
        Rule::signed_number => ParamValue::Number(parse_number(inner)),
        Rule::array_value => ParamValue::Array(inner.into_inner().map(parse_number).collect()),
        Rule::string_literal => ParamValue::String(parse_string_literal(inner)),
        Rule::boolean => ParamValue::Bool(inner.as_str() == "true"),
        _ => ParamValue::String(inner.as_str().to_string()),
    }
}

/// Parse a raw effect parameter value string into a ParamValue.
/// Tries numeric+unit patterns first, falls back to string.
fn parse_effect_value(raw: &str) -> ParamValue {
    // Try dB: "-3dB", "+2dB", "0dB"
    if raw.ends_with("dB")
        && let Ok(v) = raw[..raw.len() - 2].parse::<f64>()
    {
        return ParamValue::Db(v);
    }
    // Try percentage: "100%", "30%"
    if raw.ends_with('%')
        && let Ok(v) = raw[..raw.len() - 1].parse::<f64>()
    {
        return ParamValue::Percentage(v);
    }
    // Try Hz: "440Hz", "10kHz" — but also complex like "-2dB@10kHz"
    if let Some(num_part) = raw.strip_suffix("Hz") {
        if let Ok(v) = num_part.parse::<f64>() {
            return ParamValue::Frequency(v);
        }
        // Handle kHz
        if num_part.ends_with('k')
            && let Ok(v) = num_part[..num_part.len() - 1].parse::<f64>()
        {
            return ParamValue::Frequency(v * 1000.0);
        }
    }
    // Try time: "3.2s", "30ms"
    if raw.ends_with("ms")
        && let Ok(v) = raw[..raw.len() - 2].parse::<f64>()
    {
        return ParamValue::Time(v / 1000.0);
    }
    if raw.ends_with('s')
        && !raw.ends_with("ms")
        && let Ok(v) = raw[..raw.len() - 1].parse::<f64>()
    {
        return ParamValue::Time(v);
    }
    // Try cents: "20ct"
    if raw.ends_with("ct")
        && let Ok(v) = raw[..raw.len() - 2].parse::<f64>()
    {
        return ParamValue::Cents(v);
    }
    // Try bare number
    if let Ok(v) = raw.parse::<f64>() {
        return ParamValue::Number(v);
    }
    // Try boolean
    if raw == "true" {
        return ParamValue::Bool(true);
    }
    if raw == "false" {
        return ParamValue::Bool(false);
    }
    // Fallback: raw string (handles "hall", "large", "plate", "2:1", "-2dB@10kHz", etc.)
    ParamValue::String(raw.to_string())
}

fn parse_db_value(pair: Pair) -> f64 {
    let num = pair.into_inner().next().unwrap();
    num.as_str().parse().unwrap_or(0.0)
}

fn parse_percentage(pair: Pair) -> f64 {
    let num = pair.into_inner().next().unwrap();
    num.as_str().parse().unwrap_or(0.0)
}

fn parse_freq_value(pair: Pair) -> f64 {
    let num = pair.into_inner().next().unwrap();
    num.as_str().parse().unwrap_or(440.0)
}

fn parse_cents_value(pair: Pair) -> f64 {
    let num = pair.into_inner().next().unwrap();
    num.as_str().parse().unwrap_or(0.0)
}

fn parse_time_value(pair: Pair) -> f64 {
    let s = pair.as_str();
    if let Some(num_str) = s.strip_suffix("ms") {
        num_str.trim().parse::<f64>().unwrap_or(0.0) / 1000.0
    } else if let Some(num_str) = s.strip_suffix('s') {
        num_str.trim().parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    }
}

/// The value of a `gliss` mark: absent (slide to the next note), a pitch, a
/// length, or a run style.
fn parse_gliss_value(pair: Pair) -> Result<ParamValue, String> {
    let Some(value) = pair
        .into_inner()
        .find(|inner| inner.as_rule() == Rule::gliss_value)
    else {
        return Ok(ParamValue::String(crate::gliss::NEXT.to_string()));
    };
    let value = value
        .into_inner()
        .next()
        .expect("gliss_value has one child");
    match value.as_rule() {
        Rule::gliss_pitch => match Pitch::from_text(value.as_str()) {
            Some(pitch) => Ok(ParamValue::String(pitch.to_text())),
            None => Err(format!(
                "gliss={} is not a pitch: {}",
                value.as_str(),
                crate::gliss::FORMS
            )),
        },
        Rule::time_value => Ok(ParamValue::Time(parse_time_value(value))),
        _ => {
            let text = value.as_str();
            if text == crate::gliss::RUN || text == crate::gliss::RUN_DIATONIC {
                Ok(ParamValue::String(text.to_string()))
            } else {
                Err(format!(
                    "gliss={text} is not a glissando form: {}",
                    crate::gliss::FORMS
                ))
            }
        }
    }
}

/// A mark with no written destination slides to the next note of its own
/// phrase, so that note has to exist and has to be a single pitch.
fn check_gliss_successors(
    events: &[NoteEvent],
    positions: &[(usize, usize)],
    place: &str,
) -> Result<(), ParseError> {
    for (index, event) in events.iter().enumerate() {
        let (line, col) = positions.get(index).copied().unwrap_or((1, 1));
        let mark = crate::gliss::form(&event.modifiers).map_err(|message| ParseError::Syntax {
            line,
            col,
            message,
        })?;
        let Some(mark) = mark.filter(|mark| mark.needs_successor()) else {
            continue;
        };
        match crate::gliss::successor(events, index) {
            crate::gliss::Successor::One(_) => {}
            crate::gliss::Successor::None => {
                return Err(ParseError::Syntax {
                    line,
                    col,
                    message: format!(
                        "{} at @{}: no note follows it in this {place}, so there is nothing to slide to — write the destination pitch with gliss=G4",
                        mark.spelling(),
                        event.start_beat
                    ),
                });
            }
            crate::gliss::Successor::Chord(pitches) => {
                let pitches = pitches
                    .iter()
                    .map(format_pitch_text)
                    .collect::<Vec<_>>()
                    .join(" ");
                return Err(ParseError::Syntax {
                    line,
                    col,
                    message: format!(
                        "{} at @{}: the next note is a chord ({pitches}), so the destination is not one pitch — write it with gliss=G4",
                        mark.spelling(),
                        event.start_beat
                    ),
                });
            }
        }
    }
    Ok(())
}

fn parse_number(pair: Pair) -> f64 {
    pair.as_str().parse().unwrap_or(0.0)
}

fn parse_channel_range(pair: Pair) -> MpeMemberRange {
    let mut channels = pair
        .into_inner()
        .map(|channel| channel.as_str().parse::<u8>().unwrap_or(1).clamp(1, 16));
    MpeMemberRange {
        first: channels.next().unwrap_or(1),
        last: channels.next().unwrap_or(16),
    }
}

fn parse_string_literal(pair: Pair) -> String {
    let mut result = String::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::string_inner {
            result = inner.as_str().to_string();
        }
    }
    result
}

/// A declaration/reference name: bare identifier or a quoted literal.
fn name_of(pair: Pair) -> String {
    if pair.as_rule() == Rule::string_literal {
        parse_string_literal(pair)
    } else {
        pair.as_str().to_string()
    }
}

/// Reject a keyword whose value has no consumer, at the token that carries it.
fn unsupported(pair: Pair, message: String) -> ParseError {
    let (line, col) = pair.as_span().start_pos().line_col();
    ParseError::Syntax { line, col, message }
}

// ============================================================================
// Named Pattern Definition (top-level @pattern)
// ============================================================================

fn parse_pattern_def(pair: Pair) -> Result<PatternDef, ParseError> {
    let mut name = String::new();
    let mut instrument = InstrumentPath::new("unknown", "unknown");
    let mut preset = None;
    let engine = None;
    let mut transpose = None;
    let mut swing = None;
    let mut bars = None;
    let mut color = None;
    let mut events = Vec::new();
    let mut positions = Vec::new();
    let mut pedals = Vec::new();
    let mut hairpins = Vec::new();
    let mut hits = Vec::new();
    let effects = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => name = name_of(inner),
            Rule::pattern_inline_props => {
                for prop in inner.into_inner() {
                    let p = prop.into_inner().next().unwrap();
                    match p.as_rule() {
                        Rule::instrument_prop => {
                            let path = p.into_inner().next().unwrap();
                            instrument = parse_instrument_path(path);
                        }
                        Rule::preset_ref_prop => {
                            let id = p.into_inner().next().unwrap();
                            preset = Some(name_of(id));
                        }
                        Rule::engine_prop => {
                            return Err(unsupported(
                                p,
                                format!(
                                    "pattern '{name}': `engine:` is not supported; the instrument and preset select the engine"
                                ),
                            ));
                        }
                        Rule::pattern_transpose_prop => {
                            let val = p.into_inner().next().unwrap();
                            let (line, col) = val.as_span().start_pos().line_col();
                            let written = val.as_str();
                            let semitones =
                                written.parse::<i16>().map_err(|_| ParseError::Syntax {
                                    line,
                                    col,
                                    message: format!(
                                        "transpose: {written} is outside -127..=127 semitones"
                                    ),
                                })?;
                            if !(-127..=127).contains(&semitones) {
                                return Err(ParseError::Syntax {
                                    line,
                                    col,
                                    message: format!(
                                        "transpose: {written} is outside -127..=127 semitones"
                                    ),
                                });
                            }
                            transpose = Some(semitones as i8);
                        }
                        Rule::pattern_swing_prop => {
                            let pct = p.into_inner().next().unwrap();
                            swing = Some(parse_percentage(pct));
                        }
                        Rule::pattern_bars_prop => {
                            let val = p.into_inner().next().unwrap();
                            let (line, col) = val.as_span().start_pos().line_col();
                            let length = parse_number(val);
                            if !length.is_finite() || length <= 0.0 {
                                return Err(ParseError::Syntax {
                                    line,
                                    col,
                                    message: "bars: must be a number of bars above zero".into(),
                                });
                            }
                            bars = Some(length);
                        }
                        Rule::pattern_color_prop => {
                            color = Some(parse_string_literal(p.into_inner().next().unwrap()));
                        }
                        _ => {}
                    }
                }
            }
            Rule::pattern_def_content => {
                let content = inner.into_inner().next().unwrap();
                match content.as_rule() {
                    Rule::events_block => {
                        let mut items = Vec::new();
                        let mut previous_end_line = None;
                        for evt in content.into_inner() {
                            if evt.as_rule() == Rule::phrase_event {
                                let end_line = evt.as_span().end_pos().line_col().0;
                                let ev = evt.into_inner().next().unwrap();
                                match ev.as_rule() {
                                    Rule::note_line => items.push(parse_note_line(ev)?),
                                    Rule::rest_event => {
                                        let position = ev.as_span().start_pos().line_col();
                                        items.push(ParsedNote::Single(
                                            parse_rest_event(ev)?,
                                            position,
                                        ));
                                    }
                                    Rule::pedal_event => pedals.push(parse_pedal_event(ev)),
                                    Rule::pedal_missing_position => {
                                        return Err(pedal_needs_position(ev));
                                    }
                                    Rule::hairpin_event => hairpins.push(parse_hairpin_event(ev)),
                                    Rule::hairpin_missing_position => {
                                        return Err(hairpin_needs_position(ev));
                                    }
                                    Rule::unclosed_chord => return Err(unclosed_chord_error(ev)),
                                    Rule::bad_comma_line => {
                                        return Err(comma_error(ev, previous_end_line));
                                    }
                                    _ => {}
                                }
                                previous_end_line = Some(end_line);
                            }
                        }
                        expand_notes(items, &mut events, &mut positions);
                        check_gliss_successors(&events, &positions, "pattern")?;
                    }
                    Rule::hits_block => {
                        for evt in content.into_inner() {
                            if evt.as_rule() == Rule::hit_entry {
                                let h = evt.into_inner().next().unwrap();
                                match h.as_rule() {
                                    Rule::hit_event => hits.push(parse_hit_event(h)?),
                                    Rule::pedal_in_hits => return Err(pedal_in_hits(h)),
                                    Rule::hairpin_in_pattern => {
                                        return Err(hairpin_in_hits(h));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Rule::effects_block => {
                        return Err(unsupported(
                            content,
                            format!(
                                "pattern '{name}': `effects:` is not supported; put the effect chain on the track that hosts the clip"
                            ),
                        ));
                    }
                    Rule::automation_line => {
                        return Err(unsupported(
                            content,
                            format!(
                                "pattern '{name}': `automation:` is not supported; automate the track that hosts the clip"
                            ),
                        ));
                    }
                    Rule::mix_line => {
                        return Err(unsupported(
                            content,
                            format!(
                                "pattern '{name}': `mix:` is not supported; set the mix on the track that hosts the clip"
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    Ok(PatternDef {
        name,
        instrument,
        preset,
        engine,
        transpose,
        swing,
        bars,
        effects,
        events,
        pedals,
        hairpins,
        hits,
        color,
    })
}

fn parse_clip(pair: Pair) -> Result<Clip, ParseError> {
    let mut pattern_name = String::new();
    let mut time_range = TimeRange {
        start: BarBeat::new(1, 1, 0),
        end: BarBeat::new(1, 1, 0),
    };
    let mut offset_quarters = 0.0;
    let mut loop_quarters = 0.0;
    let mut id = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier | Rule::string_literal => pattern_name = name_of(inner),
            Rule::time_range => time_range = parse_time_range(inner),
            Rule::clip_offset_prop => offset_quarters = parse_beat_value(inner),
            Rule::clip_loop_prop => loop_quarters = parse_beat_value(inner),
            Rule::clip_id_prop => {
                id = inner.into_inner().next().map(name_of);
            }
            _ => {}
        }
    }

    Ok(Clip {
        pattern: pattern_name,
        time_range,
        offset_quarters,
        loop_quarters,
        id,
    })
}

/// Quarter notes from a `<number>q` value, e.g. `1.5q`.
fn parse_beat_value(pair: Pair) -> f64 {
    let Some(value) = pair
        .into_inner()
        .find(|inner| inner.as_rule() == Rule::beat_value)
    else {
        return 0.0;
    };
    value
        .as_str()
        .trim()
        .strip_suffix('q')
        .and_then(|number| number.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kit_pad_notes_accept_sharps_and_flats() {
        let project = parse(
            r#"@project "Accidentals"
  tempo: 120
@kit basic
  pad hat_closed | note: F#2
  pad hat_open | note: Bb2
@track drums | instrument: drumkit, preset: basic
  @pattern beat [1:1 - 2:1]
    hat_closed 1:0 ff
"#,
        )
        .unwrap();
        let params = &project.presets[0].params;
        let note = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == &format!("pad.{name}.note"))
                .and_then(|(_, value)| value.as_f64())
        };
        assert_eq!(note("hat_closed"), Some(42.0), "F#2 is the closed hi-hat");
        assert_eq!(note("hat_open"), Some(46.0), "Bb2 is the open hi-hat");
    }

    #[test]
    fn numeric_drum_modifier_does_not_consume_next_hit_as_a_unit() {
        let project = parse(
            r#"@project "Boundary"
  tempo: 120
  time_signature: 4/4
@track drums | instrument: drums/acoustic_kit
  @pattern main [1:1 - 2:1]
    kick 1:0 mf velocity=110
    snare 2:0 mf velocity=77
    splash 3:0 mf velocity=95
"#,
        )
        .unwrap();
        let hits = &project.tracks[0].patterns[0].hits;
        assert_eq!(
            hits.iter().map(|h| h.drum.as_str()).collect::<Vec<_>>(),
            vec!["kick", "snare", "splash"]
        );
        for (hit, velocity) in hits.iter().zip([110.0, 77.0, 95.0]) {
            assert_eq!(
                hit.modifiers,
                vec![("velocity".into(), ParamValue::Number(velocity))]
            );
        }
        crate::validate(&project).unwrap();
    }

    #[test]
    fn physical_units_keep_numeric_children_and_horizontal_spacing() {
        for (source, expected) in [
            ("25ms", ParamValue::Time(0.025)),
            ("25 ms", ParamValue::Time(0.025)),
            ("2 s", ParamValue::Time(2.0)),
            ("440 Hz", ParamValue::Frequency(440.0)),
            ("-6 dB", ParamValue::Db(-6.0)),
            ("50 %", ParamValue::Percentage(50.0)),
            ("7 ct", ParamValue::Cents(7.0)),
        ] {
            let pair = OcpParser::parse(Rule::param_value, source)
                .unwrap()
                .next()
                .unwrap();
            assert_eq!(parse_param_value(pair), expected, "{source}");
        }
    }

    #[test]
    fn test_parse_minimal_project() {
        let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4
  key: C major

@track piano | instrument: piano/acoustic_grand
  mix: vol=-3dB, pan=0

  @phrase intro [1:1:0 - 4:4:0]
    C4 q mf
    E4 q mf articulation=legato
    G4 h f
"#;
        let project = parse(source).expect("Failed to parse");
        assert_eq!(project.name, Some("Test".to_string()));
        assert_eq!(project.properties.tempo, 120.0);
        assert_eq!(project.properties.time_signature.numerator, 4);
        assert_eq!(project.properties.time_signature.denominator, 4);
        assert_eq!(project.tracks.len(), 1);

        let track = &project.tracks[0];
        assert_eq!(track.name, "piano");
        assert_eq!(track.instrument.as_path(), "piano/acoustic_grand");
        assert_eq!(track.mix.volume_db, -3.0);

        let phrase = &track.phrases[0];
        assert_eq!(phrase.name, "intro");
        assert_eq!(phrase.events.len(), 3);

        // First note: C4 q mf
        let n0 = &phrase.events[0];
        assert_eq!(n0.pitch.note, NoteName::C);
        assert_eq!(n0.pitch.octave, 4);
        assert_eq!(n0.pitch.to_midi(), 60);
        assert_eq!(n0.dynamics, Some(Dynamics::MF));

        // Third note: G4 h f
        let n2 = &phrase.events[2];
        assert_eq!(n2.pitch.note, NoteName::G);
        assert_eq!(n2.duration.base, BaseDuration::Half);
        assert_eq!(n2.dynamics, Some(Dynamics::F));
    }

    #[test]
    fn test_parse_pitch_variants() {
        // Test various pitch formats
        let cases = vec![
            ("C4", NoteName::C, Accidental::Natural, 4, 60),
            ("A4", NoteName::A, Accidental::Natural, 4, 69),
            ("F#5", NoteName::F, Accidental::Sharp, 5, 78),
            ("Bb3", NoteName::B, Accidental::Flat, 3, 58),
            ("Eb2", NoteName::E, Accidental::Flat, 2, 39),
        ];

        for (input, note, acc, oct, midi) in cases {
            let pair = OcpParser::parse(Rule::pitch, input)
                .unwrap()
                .next()
                .unwrap();
            let pitch = parse_pitch(pair).unwrap();
            assert_eq!(pitch.note, note, "note mismatch for {}", input);
            assert_eq!(pitch.accidental, acc, "accidental mismatch for {}", input);
            assert_eq!(pitch.octave, oct, "octave mismatch for {}", input);
            assert_eq!(pitch.to_midi(), midi, "midi mismatch for {}", input);
        }
    }

    #[test]
    fn test_parse_duration_variants() {
        let cases = vec![
            ("q", BaseDuration::Quarter, 0, false, 1.0),
            ("h", BaseDuration::Half, 0, false, 2.0),
            ("w", BaseDuration::Whole, 0, false, 4.0),
            ("8th", BaseDuration::Eighth, 0, false, 0.5),
            ("16th", BaseDuration::Sixteenth, 0, false, 0.25),
            ("q.", BaseDuration::Quarter, 1, false, 1.5),
            ("h.", BaseDuration::Half, 1, false, 3.0),
            ("qt", BaseDuration::Quarter, 0, true, 2.0 / 3.0),
        ];

        for (input, base, dots, triplet, beats) in cases {
            let pair = OcpParser::parse(Rule::duration, input)
                .unwrap()
                .next()
                .unwrap();
            let dur = parse_duration(pair).unwrap();
            assert_eq!(dur.base, base, "base mismatch for {}", input);
            assert_eq!(dur.dots, dots, "dots mismatch for {}", input);
            assert_eq!(dur.triplet, triplet, "triplet mismatch for {}", input);
            assert!(
                (dur.to_beats() - beats).abs() < 0.01,
                "beats mismatch for {}: expected {}, got {}",
                input,
                beats,
                dur.to_beats()
            );
        }
    }

    #[test]
    fn test_parse_dynamics() {
        let cases = vec![
            ("ppp", Dynamics::PPP, 16),
            ("pp", Dynamics::PP, 33),
            ("p", Dynamics::P, 49),
            ("mp", Dynamics::MP, 64),
            ("mf", Dynamics::MF, 80),
            ("f", Dynamics::F, 96),
            ("ff", Dynamics::FF, 112),
            ("fff", Dynamics::FFF, 127),
        ];

        for (input, expected, velocity) in cases {
            let pair = OcpParser::parse(Rule::dynamics, input)
                .unwrap()
                .next()
                .unwrap();
            let dyn_val = parse_dynamics(pair).unwrap();
            assert_eq!(dyn_val, expected);
            assert_eq!(dyn_val.to_velocity(), velocity);
        }
    }

    #[test]
    fn test_parse_with_bar_lines() {
        let source = r#"@project
  tempo: 120
  time_signature: 4/4

@track piano | instrument: piano/acoustic_grand
  @phrase test [1:1:0 - 2:4:0]
    C4 q mf
    E4 q mf
    G4 q f
    C5 q f
    |
    D5 h mf
    C5 h mp
"#;
        let project = parse(source).expect("Failed to parse");
        let phrase = &project.tracks[0].phrases[0];
        // Bar lines are stripped, only notes remain
        assert_eq!(phrase.events.len(), 6);
    }

    #[test]
    fn test_parse_repeat_count() {
        // Verify the repeat_count rule parses "x4"
        let pairs = OcpParser::parse(Rule::repeat_count, "x4");
        assert!(
            pairs.is_ok(),
            "repeat_count should parse 'x4': {:?}",
            pairs.err()
        );
        let val = pairs.unwrap().next().unwrap().as_str();
        assert_eq!(val, "x4");
    }

    #[test]
    fn test_parse_pattern_header() {
        // Verify pattern_header with repeat
        let input = "@pattern beat [1:1:0] x4";
        let pairs = OcpParser::parse(Rule::pattern_header, input);
        assert!(
            pairs.is_ok(),
            "pattern_header should parse: {:?}",
            pairs.err()
        );
    }

    #[test]
    fn a_pattern_transpose_round_trips_through_the_serializer() {
        let source = "@project\n  tempo: 120\n@pattern riff | instrument: piano/acoustic_grand, transpose: -2\n  events:\n    C4 q mf\n";
        let project = parse(source).expect("the pattern parses");
        assert_eq!(project.patterns[0].transpose, Some(-2));
        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("transpose: -2"), "{text}");
        let reopened = parse(&text).expect("the serialized pattern parses");
        assert_eq!(reopened.patterns[0].transpose, Some(-2));
    }

    #[test]
    fn a_pattern_transpose_outside_the_accepted_range_is_refused_by_name() {
        for value in ["200", "-128", "128"] {
            let source = format!(
                "@project\n  tempo: 120\n@pattern riff | instrument: piano/acoustic_grand, transpose: {value}\n  events:\n    C4 q mf\n"
            );
            let error = parse(&source).expect_err("an out-of-range transpose");
            let message = error.to_string();
            assert!(message.contains("transpose"), "{message}");
            assert!(message.contains("-127..=127"), "{message}");
            assert!(message.contains(value), "{message}");
        }
        let source = "@project\n  tempo: 120\n@pattern riff | instrument: piano/acoustic_grand, transpose: -2\n  events:\n    C4 q mf\n";
        let project = parse(source).expect("a range transpose still parses");
        assert_eq!(project.patterns[0].transpose, Some(-2));
    }

    #[test]
    fn a_latency_offset_outside_i32_is_refused_by_name() {
        let source = "@project\n@track t | instrument: keys/piano\n  mix: latency=2147483648\n";
        let error = parse(source).expect_err("an out-of-range latency");
        let message = error.to_string();
        assert!(message.contains("latency"), "{message}");
        assert!(message.contains("-2147483648..=2147483647"), "{message}");
        let valid = parse("@project\n@track t | instrument: keys/piano\n  mix: latency=-128\n")
            .expect("a range latency still parses");
        assert_eq!(valid.tracks[0].mix.latency_offset, -128);
    }

    #[test]
    fn a_preset_program_outside_u8_is_refused_by_name() {
        let source = "@project\n@preset grand\n  source: soundfont\n  program: 256\n";
        let error = parse(source).expect_err("an out-of-range program");
        let message = error.to_string();
        assert!(message.contains("program"), "{message}");
        assert!(message.contains("0..=255"), "{message}");
        let valid = parse("@project\n@preset grand\n  source: soundfont\n  program: 255\n")
            .expect("a range program still parses");
        assert_eq!(valid.presets[0].program, Some(255));
    }

    #[test]
    fn a_hit_velocity_round_trips_through_the_serializer() {
        let source = "@project\n  tempo: 120\n@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    kick 1:0 mf v=120\n";
        let project = parse(source).expect("the hit velocity parses");
        let hit = &project.patterns[0].hits[0];
        assert_eq!(hit.velocity, Some(120));
        assert!(
            hit.modifiers.iter().all(|(key, _)| key != "v"),
            "the typed field owns the token: {:?}",
            hit.modifiers
        );
        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("kick 1:0 mf v=120"), "{text}");
        let reopened = parse(&text).expect("the serialized hit parses");
        assert_eq!(reopened, project);
    }

    #[test]
    fn a_hit_without_a_velocity_serializes_as_before() {
        let source = "@project\n  tempo: 120\n@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    kick 1:0 mf\n    snare 2:0\n";
        let project = parse(source).expect("the fixture parses");
        let text = super::super::serializer::serialize(&project);
        assert!(
            text.contains("kick 1:0 mf\n    snare 2:0\n"),
            "no velocity token is written: {text}"
        );
        assert!(!text.contains(" v="), "{text}");
        assert_eq!(
            parse(&text).expect("the serialized fixture parses"),
            project
        );
    }

    #[test]
    fn a_project_without_the_new_fields_keeps_its_json_shape() {
        let source = "@project\n  tempo: 120\n@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    kick 1:0 mf\n";
        let project = parse(source).expect("the fixture parses");
        let json = serde_json::to_string(&project).expect("the project serialises");
        assert!(!json.contains("\"hairpins\""), "{json}");
        assert!(!json.contains("\"velocity\""), "{json}");
    }

    #[test]
    fn a_hit_velocity_outside_one_to_127_is_refused_by_validation() {
        for written in ["0", "128", "200"] {
            let source = format!(
                "@project\n  tempo: 120\n@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    kick 1:0 mf v={written}\n"
            );
            let project = parse(&source).expect("the token parses");
            let errors = crate::validator::validate(&project)
                .expect_err("an out-of-range hit velocity is invalid");
            let message = errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(message.contains("hit.velocity"), "{message}");
            assert!(message.contains("1..=127"), "{message}");
        }
    }

    #[test]
    fn test_parse_drum_pattern() {
        let source = "@project\n  tempo: 128\n  time_signature: 4/4\n\n@track drums | instrument: drums/acoustic_kit\n  @pattern beat [1:1:0 - 1:4:0] x4\n    kick 1:0 ff\n    snare 2:0 f\n    hihat 1:0 mp articulation=closed\n    hihat 2:0 mf articulation=open\n";
        let project = parse(source).expect("Failed to parse");
        let pattern = &project.tracks[0].patterns[0];
        assert_eq!(pattern.name, "beat");
        assert_eq!(pattern.repeat, 4);
        assert_eq!(pattern.hits.len(), 4);
        assert_eq!(pattern.hits[0].drum, "kick");
        assert_eq!(pattern.hits[2].articulation, Some(DrumArticulation::Closed));
        assert_eq!(pattern.hits[3].articulation, Some(DrumArticulation::Open));
    }

    #[test]
    fn test_parse_bus_and_master() {
        let source = r#"@project
  tempo: 120
  time_signature: 4/4

@bus reverb_bus
  routes: [master]
  effects:
    - reverb: decay=2.5s, mix=100%

@master
  volume: -6dB
  loudness: -14dB
  effects:
    - limiter: ceiling=-0.3dB
"#;
        let project = parse(source).expect("Failed to parse");
        assert_eq!(project.buses.len(), 1);
        assert_eq!(project.buses[0].name, "reverb_bus");
        assert_eq!(project.buses[0].routes, vec!["master"]);
        assert_eq!(project.buses[0].effects.len(), 1);

        let master = project.master.as_ref().unwrap();
        assert_eq!(master.effects.len(), 1);
        assert_eq!(master.volume_db, -6.0);
        assert_eq!(master.loudness_target, Some(-14.0));
    }

    #[test]
    fn test_master_volume_defaults_to_unity() {
        let project = parse("@project\n@master\n").unwrap();
        assert_eq!(project.master.unwrap().volume_db, 0.0);
    }

    #[test]
    fn test_parse_explicit_start_beat() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 2:4:0]\n    @2.0 C4 q mf\n    @0.0 E4 q mf\n";
        let project = parse(source).expect("Failed to parse");
        let events = &project.tracks[0].phrases[0].events;
        assert_eq!(events.len(), 2);
        assert!((events[0].start_beat - 2.0).abs() < 0.001);
        assert!(events[0].explicit_position);
        assert!((events[1].start_beat - 0.0).abs() < 0.001);
        assert!(events[1].explicit_position);
    }

    #[test]
    fn test_parse_negative_generic_numeric_modifier() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 2:1:0]\n    C4 q mf humanize_offset_beats=-0.021\n";
        let project = parse(source).expect("Failed to parse");
        let modifier = &project.tracks[0].phrases[0].events[0].modifiers[0];
        assert_eq!(modifier.0, "humanize_offset_beats");
        assert!(
            matches!(modifier.1, ParamValue::Number(value) if (value + 0.021).abs() < 0.000_001)
        );
    }

    #[test]
    fn test_parse_chord_with_explicit_positions() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase chord [1:1:0 - 1:4:0]\n    @0.0 C4 q mf\n    @0.0 E4 q mf\n    @0.0 G4 q mf\n";
        let project = parse(source).expect("Failed to parse");
        let events = &project.tracks[0].phrases[0].events;
        assert_eq!(events.len(), 3);
        for e in events {
            assert!((e.start_beat - 0.0).abs() < 0.001);
            assert!(e.explicit_position);
        }
    }

    #[test]
    fn test_parse_mixed_explicit_and_sequential() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase mixed [1:1:0 - 2:4:0]\n    @0.0 C4 q mf\n    @0.0 E4 q mf\n    D4 h mf\n    F4 q mf\n";
        let project = parse(source).expect("Failed to parse");
        let events = &project.tracks[0].phrases[0].events;
        assert_eq!(events.len(), 4);
        assert!((events[2].start_beat - 1.0).abs() < 0.001);
        assert!(!events[2].explicit_position);
        assert!((events[3].start_beat - 3.0).abs() < 0.001);
        assert!(!events[3].explicit_position);
    }

    #[test]
    fn test_parse_plugin_effect() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - eq: high_shelf=-2dB@10kHz\n    - plugin:\"org.chowdsp.ChowMultiTool\"\n    - compression: ratio=2:1\n";
        let project = parse(source).expect("Failed to parse");
        let effects = &project.tracks[0].effects;
        assert_eq!(effects.len(), 3);
        assert_eq!(effects[0].effect_type, EffectType::Eq);
        assert_eq!(
            effects[1].effect_type,
            EffectType::Plugin("org.chowdsp.ChowMultiTool".into())
        );
        assert_eq!(effects[2].effect_type, EffectType::Compression);
    }

    #[test]
    fn test_parse_lv2_preset_source_and_effect() {
        let source = "@project\n  tempo: 120\n@preset gain\n  source: lv2\n  plugin: \"http://example.org/lv2/gain\"\n  params: {gain: 0.5}\n@track t | preset: gain, instrument: synth/lead\n  effects:\n    - plugin:\"lv2.http://example.org/lv2/gain\": gain=0.5\n";
        let project = parse(source).expect("Failed to parse");
        assert_eq!(project.presets[0].source.as_deref(), Some("lv2"));
        assert_eq!(
            project.presets[0].plugin.as_deref(),
            Some("http://example.org/lv2/gain")
        );
        assert_eq!(
            project.tracks[0].effects[0].effect_type,
            EffectType::Plugin("lv2.http://example.org/lv2/gain".into())
        );
    }

    #[test]
    fn test_parse_doubler_exciter_and_ducked_send_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@bus verb\n  routes: [master]\n  effects:\n    - ducked_send: predelay=30ms, depth=9dB, attack=20ms, release=320ms, threshold=-45dB, width=120%, sidechain=t\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - doubler: voices=3, delay=18ms, rate=0.35, depth=2ms, detune=7, focus=300Hz, mix=60%\n    - exciter: amount=60%, drive=8dB, character=30%, low_freq=10kHz, high_freq=16kHz, output=-2dB\n  mix: send(verb=50%)\n";
        let project = crate::parse(source).unwrap();
        let effects = &project.tracks[0].effects;
        assert_eq!(effects[0].effect_type, EffectType::Doubler);
        assert_eq!(effects[1].effect_type, EffectType::Exciter);
        let bus_effects = &project.buses[0].effects;
        assert_eq!(bus_effects[0].effect_type, EffectType::DuckedSend);
        assert_eq!(bus_effects[0].sidechain_source.as_deref(), Some("t"));

        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("doubler:"), "{text}");
        assert!(text.contains("exciter:"), "{text}");
        assert!(text.contains("ducked_send:"), "{text}");
        assert!(text.contains("sidechain=t"), "{text}");
        let reparsed = crate::parse(&text).unwrap();
        assert_eq!(
            reparsed.tracks[0].effects[0].effect_type,
            EffectType::Doubler
        );
        assert_eq!(
            reparsed.buses[0].effects[0].effect_type,
            EffectType::DuckedSend
        );
    }

    #[test]
    fn test_parse_resonance_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track vocal | instrument: vocal/lead\n  effects:\n    - resonance: threshold=6dB, depth=9dB, selectivity=35%, attack=10ms, release=150ms, mix=100%, enabled=t\n";
        let project = crate::parse(source).unwrap();
        let effect = &project.tracks[0].effects[0];
        assert_eq!(effect.effect_type, EffectType::Resonance);
        assert_eq!(effect.params.len(), 7);
        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("resonance:"), "{text}");
        assert!(text.contains("selectivity=35%"), "{text}");
        let reparsed = crate::parse(&text).unwrap();
        assert_eq!(
            reparsed.tracks[0].effects[0].effect_type,
            EffectType::Resonance
        );
    }

    #[test]
    fn test_parse_tuner_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track vocal | instrument: vocal/lead\n  effects:\n    - tuner: amount=80%, retune=40ms, key=A, scale=major, mode=low_latency, enabled=t\n";
        let project = crate::parse(source).unwrap();
        let effect = &project.tracks[0].effects[0];
        assert_eq!(effect.effect_type, EffectType::Tuner);
        assert_eq!(effect.params.len(), 6);
        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("tuner:"), "{text}");
        assert!(text.contains("scale=major"), "{text}");
        let reparsed = crate::parse(&text).unwrap();
        assert_eq!(reparsed.tracks[0].effects[0].effect_type, EffectType::Tuner);
    }

    #[test]
    fn test_parse_detector_eq_and_multiband_switches_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track vocal | instrument: vocal/lead\n  effects:\n    - compression: threshold=-24dB, sidechain_eq1_on=true, sidechain_eq1_type=high_shelf, sidechain_eq1_frequency=6kHz, sidechain_eq1_gain=9dB, sidechain_eq1_q=0.9\n    - multiband: lookahead=5ms, band1_lookahead=4ms, band2_solo=true, band3_mute=true\n";
        let project = crate::parse(source).unwrap();
        let effects = &project.tracks[0].effects;
        assert_eq!(effects[0].effect_type, EffectType::Compression);
        assert_eq!(effects[0].params.len(), 6);
        assert!(effects[0].params.iter().any(|(name, value)| {
            name == "sidechain_eq1_type"
                && matches!(value, crate::ast::ParamValue::String(text) if text == "high_shelf")
        }));
        assert!(effects[0].params.iter().any(|(name, value)| {
            name == "sidechain_eq1_frequency"
                && matches!(value, crate::ast::ParamValue::Frequency(hz) if (*hz - 6_000.0).abs() < 1e-9)
        }));
        assert_eq!(effects[1].effect_type, EffectType::Multiband);
        assert_eq!(effects[1].params.len(), 4);

        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("sidechain_eq1_type=high_shelf"), "{text}");
        assert!(text.contains("sidechain_eq1_frequency=6000Hz"), "{text}");
        assert!(text.contains("band2_solo=true"), "{text}");
        assert!(text.contains("lookahead=5ms"), "{text}");
        let reparsed = crate::parse(&text).unwrap();
        assert_eq!(reparsed.tracks[0].effects, project.tracks[0].effects);
    }

    #[test]
    fn test_parse_limiter_link_and_decay_eq_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - limiter: link=40%, ceiling=-0.8dB\n    - reverb_fdn: decay_band1=6dB, decay_band1_freq=2kHz, decay_band1_q=1.4, decay_band1_shape=low_shelf\n";
        let project = crate::parse(source).expect("Failed to parse the new device controls");
        let effects = &project.tracks[0].effects;
        assert_eq!(effects[0].effect_type, EffectType::Limiter);
        assert_eq!(
            effects[0].params[0],
            ("link".into(), crate::ast::ParamValue::Percentage(40.0))
        );
        assert_eq!(effects[1].effect_type, EffectType::ReverbFdn);
        assert!(effects[1].params.iter().any(|(name, value)| {
            name == "decay_band1_shape"
                && matches!(value, crate::ast::ParamValue::String(text) if text == "low_shelf")
        }));
        assert!(effects[1].params.iter().any(|(name, value)| {
            name == "decay_band1_freq"
                && matches!(value, crate::ast::ParamValue::Frequency(hz) if (*hz - 2_000.0).abs() < 1e-9)
        }));

        let text = crate::serialize(&project);
        assert!(text.contains("link=40%"), "{text}");
        assert!(text.contains("decay_band1_shape=low_shelf"), "{text}");
        assert!(text.contains("decay_band1_freq=2000Hz"), "{text}");
        let reopened = crate::parse(&text).expect("serialized controls should parse");
        assert_eq!(&reopened.tracks[0].effects, effects);
    }

    #[test]
    fn test_parse_leveler_and_opto_effects_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - leveler: target=-24dB, speed=4, window=800ms, gate=-50dB, gate_release=1200ms\n    - opto: peak_reduction=9dB, release=2s, makeup=+2dB\n";
        let project = parse(source).expect("Failed to parse vocal devices");
        let effects = &project.tracks[0].effects;
        assert_eq!(effects[0].effect_type, EffectType::Leveler);
        assert_eq!(
            effects[0].params[2],
            ("window".into(), ParamValue::Time(0.8))
        );
        assert_eq!(effects[1].effect_type, EffectType::Opto);
        assert_eq!(
            effects[1].params[1],
            ("release".into(), ParamValue::Time(2.0))
        );

        let text = crate::serialize(&project);
        assert!(text.contains("leveler:"), "{text}");
        assert!(text.contains("opto:"), "{text}");
        let reopened = parse(&text).expect("serialized vocal devices should parse");
        assert_eq!(&reopened.tracks[0].effects, effects);
    }

    #[test]
    fn test_parse_fet_effect_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - fet: drive=6dB, attack=0.3ms, release=120ms, output=-1dB, bias=75%\n";
        let project = parse(source).expect("Failed to parse the FET");
        let effects = &project.tracks[0].effects;
        assert_eq!(effects[0].effect_type, EffectType::Fet);
        assert_eq!(effects[0].params[0], ("drive".into(), ParamValue::Db(6.0)));
        assert_eq!(
            effects[0].params[1],
            ("attack".into(), ParamValue::Time(0.0003))
        );
        assert_eq!(
            effects[0].params[4],
            ("bias".into(), ParamValue::Percentage(75.0))
        );

        let text = crate::serialize(&project);
        assert!(text.contains("fet:"), "{text}");
        let reopened = parse(&text).expect("serialized FET should parse");
        assert_eq!(&reopened.tracks[0].effects, effects);
    }

    #[test]
    fn test_parse_iron_and_passive_eq_effects_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - iron: drive=6dB, bias=0.2, ceiling=30%, low_freq=25Hz, high_freq=18kHz, output=-2dB\n    - passive_eq: low_freq=100Hz, low_boost=5dB, low_cut=5dB, high_freq=8kHz, high_boost=3dB, high_q=1.2, high_cut_freq=10kHz, high_cut=4dB, tilt=2dB, air_freq=10kHz, air=3dB\n";
        let project = parse(source).expect("Failed to parse tone devices");
        let effects = &project.tracks[0].effects;
        assert_eq!(effects[0].effect_type, EffectType::Iron);
        assert_eq!(effects[0].params[0], ("drive".into(), ParamValue::Db(6.0)));
        assert_eq!(
            effects[0].params[2],
            ("ceiling".into(), ParamValue::Percentage(30.0))
        );
        assert_eq!(
            effects[0].params[4],
            ("high_freq".into(), ParamValue::Frequency(18_000.0))
        );
        assert_eq!(effects[1].effect_type, EffectType::PassiveEq);
        assert_eq!(
            effects[1].params[1],
            ("low_boost".into(), ParamValue::Db(5.0))
        );
        assert_eq!(
            effects[1].params[9],
            ("air_freq".into(), ParamValue::Frequency(10_000.0))
        );

        let text = crate::serialize(&project);
        assert!(text.contains("iron:"), "{text}");
        assert!(text.contains("passive_eq:"), "{text}");
        let reopened = parse(&text).expect("serialized tone devices should parse");
        assert_eq!(&reopened.tracks[0].effects, effects);
    }

    #[test]
    fn test_parse_dynamic_eq_effect_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - dynamic_eq: band1_enabled=true, band1_type=bell, band1_freq=1kHz, band1_q=1, band1_gain=+3dB, band1_dynamic=true, band1_threshold=-30dB, band1_range=-6dB, band1_attack=10ms, band1_release=120ms, low_cut_enabled=true, low_cut_freq=30Hz, high_cut_enabled=true, high_cut_freq=18kHz\n";
        let project = parse(source).expect("Failed to parse dynamic EQ");
        let effect = &project.tracks[0].effects[0];
        assert_eq!(effect.effect_type, EffectType::DynamicEq);
        assert_eq!(effect.params.len(), 14);
        assert_eq!(
            effect.params[1],
            ("band1_type".into(), ParamValue::String("bell".into()))
        );
        assert_eq!(
            effect.params[2],
            ("band1_freq".into(), ParamValue::Frequency(1000.0))
        );

        let text = crate::serialize(&project);
        assert!(text.contains("dynamic_eq:"), "{text}");
        let reopened = parse(&text).expect("serialized dynamic EQ should parse");
        assert_eq!(project, reopened);
    }

    #[test]
    fn test_parse_deesser_and_multiband_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - de_esser: threshold=-42dB, range=9dB, band_processing=split_band, high_frequency=6kHz\n    - multiband: band_count=three_bands, slope=48dB/oct, phase_mode=linear, crossover_1=250Hz, crossover_2=4kHz, band1_threshold=-24dB, band1_range=-6dB, band1_ratio=4:1, band2_enabled=false\n";
        let project = parse(source).expect("Failed to parse pro devices");
        let effects = &project.tracks[0].effects;
        assert_eq!(effects[0].effect_type, EffectType::DeEsser);
        assert_eq!(effects[1].effect_type, EffectType::Multiband);
        let text = crate::serialize(&project);
        assert!(text.contains("de_esser:"), "{text}");
        assert!(text.contains("multiband:"), "{text}");
        let reopened = parse(&text).expect("serialized pro devices should parse");
        assert_eq!(project, reopened);
    }

    #[test]
    fn breath_split_parses_and_serializes() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track vocal | instrument: vocal/lead\n  effects:\n    - breath_split: breath_level=-12dB, voice_level=-3dB, threshold=0.6, smoothing=20ms, monitor=true, split_frequency=2kHz\n";
        let project = parse(source).expect("Failed to parse breath split");
        let effect = &project.tracks[0].effects[0];
        assert_eq!(effect.effect_type, EffectType::BreathSplit);
        assert_eq!(
            effect.params[0],
            ("breath_level".into(), ParamValue::Db(-12.0))
        );
        assert_eq!(
            effect.params[1],
            ("voice_level".into(), ParamValue::Db(-3.0))
        );
        assert_eq!(
            effect.params[3],
            ("smoothing".into(), ParamValue::Time(0.020))
        );
        assert_eq!(effect.params[4], ("monitor".into(), ParamValue::Bool(true)));
        assert_eq!(
            effect.params[5],
            ("split_frequency".into(), ParamValue::Frequency(2_000.0))
        );
        let text = crate::serialize(&project);
        assert!(text.contains("breath_split:"), "{text}");
        let reopened = parse(&text).expect("serialized breath split should parse");
        assert_eq!(project, reopened);
    }

    #[test]
    fn plosive_parses_and_serializes() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track vocal | instrument: vocal/lead\n  effects:\n    - plosive: amount=80%, threshold=7dB, frequency=110Hz, release=55ms, monitor=true\n";
        let project = parse(source).expect("Failed to parse plosive");
        let effect = &project.tracks[0].effects[0];
        assert_eq!(effect.effect_type, EffectType::Plosive);
        assert_eq!(effect.effect_type.device_id(), Some("plosive"));
        assert_eq!(effect.effect_type.lane_name(), "plosive");
        assert_eq!(
            effect.params,
            vec![
                ("amount".into(), ParamValue::Percentage(80.0)),
                ("threshold".into(), ParamValue::Db(7.0)),
                ("frequency".into(), ParamValue::Frequency(110.0)),
                ("release".into(), ParamValue::Time(0.055)),
                ("monitor".into(), ParamValue::Bool(true)),
            ]
        );
        let text = crate::serialize(&project);
        assert!(text.contains("plosive:"), "{text}");
        let reopened = parse(&text).expect("serialized plosive should parse");
        assert_eq!(project, reopened);
    }

    #[test]
    fn test_parse_custom_drum_names() {
        let source = "@project\n  tempo: 128\n  time_signature: 4/4\n\n@track drums | instrument: drums/acoustic_kit\n  @pattern beat [1:1:0] x2\n    tabla 1:0 mf\n    djembe 2:0 f\n";
        let project = parse(source).expect("Failed to parse");
        let hits = &project.tracks[0].patterns[0].hits;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].drum, "tabla");
        assert_eq!(hits[1].drum, "djembe");
    }

    #[test]
    fn test_parse_version_header() {
        let source = "@ocp \"0.2.0\"\n\n@project\n  tempo: 120\n  time_signature: 4/4\n";
        let project = parse(source).expect("Failed to parse");
        assert_eq!(project.ocp_version, Some("0.2.0".to_string()));
    }

    #[test]
    fn test_parse_without_version_header() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n";
        let project = parse(source).expect("Failed to parse");
        assert_eq!(project.ocp_version, None);
    }
    #[test]
    fn parses_aux_output_sends_and_midi_channels() {
        let source = "@project\n  tempo: 120\n@preset kontakt\n  source: vst3\n  plugin: \"com.native-instruments.kontakt\"\n@track kit | preset: kontakt, instrument: keys/kontakt, channel: 4\n  mix: send(kit.out3->drums=50%)\n  @phrase ride [1:1 - 2:1] | channel: 6\n    C4 q\n@bus drums\n  routes: [master]\n";
        let project = parse(source).unwrap();
        crate::validate(&project).unwrap();
        assert_eq!(project.tracks[0].channel, Some(4));
        assert_eq!(project.tracks[0].phrases[0].channel, Some(6));
        let send = &project.tracks[0].mix.sends[0];
        assert_eq!(send.target, "drums");
        assert_eq!(
            send.source,
            Some(SendSource {
                owner: "kit".to_string(),
                output: 3,
            })
        );
        assert_eq!(send.source.as_ref().unwrap().pair(), 2);

        let text = crate::serializer::serialize(&project);
        assert!(text.contains("send(kit.out3->drums=50%)"), "{text}");
        assert!(text.contains("channel: 4"), "{text}");
        assert!(text.contains("| channel: 6"), "{text}");
        assert_eq!(parse(&text).unwrap(), project);
    }

    #[test]
    fn parses_and_round_trips_track_output_ports() {
        let source = "@project\n  tempo: 120\n@preset kontakt\n  source: vst3\n  plugin: \"com.native-instruments.kontakt\"\n@track out2 | preset: kontakt, instrument: keys/kontakt, port: 2, pair: 1\n@track main | preset: kontakt, instrument: keys/kontakt\n";
        let project = parse(source).unwrap();
        crate::validate(&project).unwrap();
        assert_eq!(project.tracks[0].output_port, Some(2));
        assert_eq!(project.tracks[0].output_pair, Some(1));
        assert_eq!(project.tracks[1].output_port, None);
        assert_eq!(project.tracks[1].output_pair, None);

        let text = crate::serializer::serialize(&project);
        assert!(text.contains("port: 2"), "{text}");
        assert!(text.contains("pair: 1"), "{text}");
        assert_eq!(parse(&text).unwrap(), project);
    }

    #[test]
    fn rejects_out_of_range_track_output_ports() {
        let mut project = parse("@project\n@track t | instrument: keys/piano\n").unwrap();
        project.tracks[0].output_port = Some(64);
        assert!(crate::validate(&project).is_err());
        project.tracks[0].output_port = None;
        project.tracks[0].output_pair = Some(64);
        assert!(crate::validate(&project).is_err());
    }

    #[test]
    fn track_mix_smart_disable_round_trips() {
        let project = parse(
            "@project\n@track t | instrument: keys/piano\n  mix: vol=-3dB, smart_disable=false\n",
        )
        .unwrap();
        assert!(!project.tracks[0].mix.smart_disable);

        let text = crate::serializer::serialize(&project);
        assert!(text.contains("smart_disable=false"), "{text}");
        assert_eq!(parse(&text).unwrap(), project);

        let default = parse("@project\n@track t | instrument: keys/piano\n").unwrap();
        assert!(
            default.tracks[0].mix.smart_disable,
            "the gate is on by default"
        );
    }

    #[test]
    fn an_effect_smart_disable_parameter_stays_a_parameter() {
        let project = parse(
            "@project\n@track t | instrument: keys/piano\n  effects:\n    - plugin:\"vendor.reverb\": smart_disable=false\n",
        )
        .unwrap();
        assert_eq!(
            project.tracks[0].effects[0].params,
            vec![(
                "smart_disable".to_string(),
                crate::ast::ParamValue::Bool(false)
            )]
        );
    }

    #[test]
    fn rejects_main_output_and_foreign_owner_as_aux_source() {
        let main = parse("@project\n@preset p\n  source: vst3\n  plugin: \"x\"\n@track t | preset: p, instrument: keys/k\n  mix: send(t.out1->bus=100%)\n@bus bus\n").unwrap();
        assert!(crate::validate(&main).is_err());

        let foreign = parse("@project\n@preset p\n  source: vst3\n  plugin: \"x\"\n@track t | preset: p, instrument: keys/k\n  mix: send(other.out2->bus=100%)\n@bus bus\n").unwrap();
        assert!(crate::validate(&foreign).is_err());
    }

    #[test]
    fn pattern_automation_is_rejected() {
        let err = parse(
            "@project\n@pattern p | instrument: synth/lead\n  events:\n    C4 q\n  automation: cutoff {1:1=0%, 2:1=50%}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("automation"), "{err}");
        assert!(err.contains("pattern 'p'"), "{err}");
    }

    #[test]
    fn pattern_mix_is_rejected() {
        let err = parse(
            "@project\n@pattern p | instrument: synth/lead\n  mix: vol=-3dB\n  events:\n    C4 q\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("mix"), "{err}");
        assert!(err.contains("pattern 'p'"), "{err}");
    }

    #[test]
    fn pattern_effects_are_rejected() {
        let err = parse(
            "@project\n@pattern p | instrument: synth/lead\n  effects:\n    - reverb: mix=20%\n  events:\n    C4 q\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("effects"), "{err}");
        assert!(err.contains("pattern 'p'"), "{err}");
    }

    #[test]
    fn pattern_engine_is_rejected() {
        let err = parse("@project\n@pattern p | instrument: synth/lead, engine: opensynth\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("engine"), "{err}");
        assert!(err.contains("pattern 'p'"), "{err}");
    }

    #[test]
    fn track_engine_is_rejected() {
        let err = parse("@project\n@track t | instrument: synth/lead, engine: opensynth\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("engine"), "{err}");
        assert!(err.contains("track 't'"), "{err}");
    }

    #[test]
    fn track_engine_params_still_parse() {
        let project = parse(
            "@project\n@track t | instrument: synth/lead\n  engine_params: { midi_program: 4 }\n",
        )
        .unwrap();
        assert_eq!(project.tracks[0].engine_params.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn rejects_midi_channels_above_fifteen() {
        let track = parse("@project\n@track t | instrument: synth/lead\n").unwrap();
        let mut project = track;
        project.tracks[0].channel = Some(16);
        assert!(crate::validate(&project).is_err());

        let mut project =
            parse("@project\n@track t | instrument: synth/lead\n  @phrase p\n    C4 q\n").unwrap();
        project.tracks[0].phrases[0].channel = Some(16);
        assert!(crate::validate(&project).is_err());
    }

    #[test]
    fn effect_parameter_lanes_round_trip_on_track_bus_and_master() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB, ratio=4\n    - plugin:\"vst3.test.fake\"\n  automation: compression.threshold {1:1:0=-24dB, 2:1:0=-6dB}\n  automation: fx2.Gain [ppq=480] {1:1:0=0.25, 1:1:2=0.75}\n\n@bus b\n  routes: [master]\n  effects:\n    - eq: mid_freq=1kHz\n  automation: eq.mid_freq {1:1:0=1000Hz, 3:1:0=4000Hz}\n\n@master\n  effects:\n    - limiter: ceiling=-1dB\n  automation: limiter.ceiling {1:1:0=-3dB, 3:1:0=-1dB}\n";
        let project = crate::parse(source).unwrap();
        assert_eq!(
            project.tracks[0].automation_lane(0).unwrap().param,
            "compression.threshold"
        );
        assert_eq!(
            project.tracks[0].automation_lane(1).unwrap().param,
            "fx2.Gain"
        );
        assert_eq!(project.tracks[0].automation_lane(1).unwrap().ppq, 480);
        assert_eq!(project.buses[0].automation[0].param, "eq.mid_freq");
        assert_eq!(
            project.master.as_ref().unwrap().automation[0].param,
            "limiter.ceiling"
        );

        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("automation: compression.threshold"), "{text}");
        assert!(text.contains("automation: fx2.Gain [ppq=480]"), "{text}");
        assert!(text.contains("automation: eq.mid_freq"), "{text}");
        assert!(text.contains("automation: limiter.ceiling"), "{text}");
        let reparsed = crate::parse(&text).unwrap();
        assert_eq!(reparsed, project);
    }

    #[test]
    fn smoothing_and_point_curves_round_trip_with_safe_defaults() {
        let source = "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB\n  automation: compression.threshold smoothing=25ms {1:1:0=-24dB curve=stairs, 2:1:0=-6dB curve=half_sine, 3:1:0=-3dB curve=smooth tension=0.4, 4:1:0=-12dB}\n";
        let project = crate::parse(source).unwrap();
        assert!(crate::validate(&project).is_ok());
        let lane = &project.tracks[0].automation_lane(0).unwrap();
        assert_eq!(lane.smoothing_ms, 25.0);
        assert_eq!(lane.points[0].curve, Curve::Stairs);
        assert_eq!(lane.points[1].curve, Curve::HalfSine);
        assert_eq!(lane.points[2].curve, Curve::Smooth);
        assert_eq!(lane.points[2].tension, Some(0.4));
        assert_eq!(lane.points[3].curve, Curve::Linear);
        assert_eq!(lane.points[3].tension, None);

        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("smoothing=25ms"), "{text}");
        assert!(text.contains("curve=stairs"), "{text}");
        assert!(text.contains("curve=half_sine"), "{text}");
        assert!(text.contains("curve=smooth tension=0.4"), "{text}");
        assert_eq!(crate::parse(&text).unwrap(), project);

        // Absent fields keep the legacy defaults: no smoothing, linear segments.
        let plain = crate::parse(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1:0=1, 2:1:0=0.5}\n",
        )
        .unwrap();
        let lane = &plain.tracks[0].automation_lane(0).unwrap();
        assert_eq!(lane.smoothing_ms, 0.0);
        assert!(lane.points.iter().all(|point| point.curve == Curve::Linear));
        assert_eq!(lane.mode, LaneMode::Envelope);
        assert_eq!(lane.lfo, LaneLfo::default());
    }

    /// A lane in LFO mode carries its cycle as lane attributes, and the whole
    /// thing survives the serializer unchanged. Old projects parse to the
    /// envelope defaults.
    #[test]
    fn lfo_lane_attributes_round_trip_and_default_to_envelope() {
        let source = "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-18dB\n  automation: compression.threshold mode=lfo speed=1/8 mult=2 pw=0.25 skew=-0.5 tension=0.3 amount=0.75 smoothing=10ms {1:1=0, 1:2=1 curve=smooth tension=0.4, 1:3=-1}\n";
        let project = crate::parse(source).unwrap();
        assert!(crate::validate(&project).is_ok());
        let lane = &project.tracks[0].automation_lane(0).unwrap();
        assert_eq!(lane.mode, LaneMode::Lfo);
        assert_eq!(
            lane.lfo.speed,
            LfoSpeed::Note {
                numerator: 1,
                denominator: 8
            }
        );
        assert_eq!(lane.lfo.mult, 2.0);
        assert_eq!(lane.lfo.pulse_width, 0.25);
        assert_eq!(lane.lfo.skew, -0.5);
        assert_eq!(lane.lfo.tension, 0.3);
        assert_eq!(lane.lfo.amount, 0.75);
        assert_eq!(lane.smoothing_ms, 10.0);
        assert_eq!(lane.points[1].curve, Curve::Smooth);
        assert_eq!(lane.points[1].tension, Some(0.4));

        let text = super::super::serializer::serialize(&project);
        assert!(text.contains("mode=lfo"), "{text}");
        assert!(text.contains("speed=1/8"), "{text}");
        assert!(text.contains("mult=2"), "{text}");
        assert!(text.contains("pw=0.25"), "{text}");
        assert!(text.contains("skew=-0.5"), "{text}");
        assert!(text.contains("tension=0.3"), "{text}");
        assert!(text.contains("amount=0.75"), "{text}");
        assert_eq!(crate::parse(&text).unwrap(), project);

        // An absolute speed and the default cycle controls: only the attributed
        // fields are written, so a mostly-default lane stays short.
        let hz = crate::parse(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor mode=lfo speed=2.5Hz {1:1=1, 1:2=0}\n",
        )
        .unwrap();
        assert!(crate::validate(&hz).is_ok());
        assert_eq!(
            hz.tracks[0].automation_lane(0).unwrap().lfo.speed,
            LfoSpeed::Hz { hz: 2.5 }
        );
        let text = super::super::serializer::serialize(&hz);
        assert!(text.contains("mode=lfo speed=2.5Hz {"), "{text}");
        assert!(!text.contains("mult="), "{text}");
        assert_eq!(crate::parse(&text).unwrap(), hz);

        // Envelope mode never writes cycle attributes, even when a lane carries
        // them in memory; switching modes changes nothing else in the file.
        let mut project = hz;
        project.tracks[0].automation_lane_mut(0).unwrap().mode = LaneMode::Envelope;
        let text = super::super::serializer::serialize(&project);
        assert!(!text.contains("mode="), "{text}");
        assert!(!text.contains("speed="), "{text}");
        assert_eq!(
            crate::parse(&text).unwrap().tracks[0]
                .automation_lane(0)
                .unwrap()
                .mode,
            LaneMode::Envelope
        );

        // Old projects are unchanged: no mode attribute, envelope evaluator.
        let old = crate::parse(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1=1, 1:2=0}\n",
        )
        .unwrap();
        assert_eq!(
            old.tracks[0].automation_lane(0).unwrap().mode,
            LaneMode::Envelope
        );
        assert!(!super::super::serializer::serialize(&old).contains("mode="));
    }

    #[test]
    fn lfo_cycles_are_validated() {
        let base = "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor mode=lfo ";
        for (attribute, ok) in [
            ("speed=0Hz", false),
            ("speed=1000Hz", true),
            ("speed=1001Hz", false),
            ("speed=1/0", false),
            ("speed=65/4", false),
            ("mult=0", false),
            ("mult=64", true),
            ("pw=0", false),
            ("pw=1", false),
            ("pw=0.5", true),
            ("skew=-1.5", false),
            ("skew=1", true),
            ("tension=1.5", false),
            ("amount=0", true),
            ("amount=4.5", false),
        ] {
            let source = format!("{base}{attribute} {{1:1=1, 1:2=0}}\n");
            let parsed = crate::parse(&source);
            assert!(parsed.is_ok(), "{attribute}: {parsed:?}");
            assert_eq!(
                crate::validate(&parsed.unwrap()).is_ok(),
                ok,
                "{attribute} validation"
            );
        }

        // LFO mode is an effect-lane feature; a control lane stays a step list.
        let control = crate::parse(
            "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  automation: brightness mode=lfo speed=1Hz {1:1=1, 1:2=0}\n",
        )
        .unwrap();
        let errors = crate::validate(&control).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| format!("{error}").contains("LFO mode")),
            "{errors:?}"
        );
    }

    #[test]
    fn control_sources_and_gain_rides_round_trip_byte_for_byte() {
        let source = "@project \"control\"\n  tempo: 120\n  time_signature: 4/4\n\n@control_source kick_env | from: track(kick), attack: 5ms, release: 120ms, lfo: none, formula: none\n\n@control_source wobble | from: track(kick), attack: 20ms, release: 300ms, lfo: sine@2Hz, lfo_depth: 0.5, formula: 1-x, scale: 2.5, offset: 0.125, curve: sqrt\n\n@preset tone\n  source: builtin\n  plugin: \"builtin.open_synth\"\n\n@track kick | preset: tone, instrument: drums/kick\n\n@track bass | preset: tone, instrument: synth/bass\n  mix: control(kick_env=-12dB)\n";
        let project = crate::parse(source).unwrap();
        assert_eq!(project.properties.control_sources.len(), 2);
        let kick = &project.properties.control_sources[0];
        assert_eq!(kick.name, "kick_env");
        assert_eq!(kick.source_track, "kick");
        assert_eq!(kick.attack, 0.005);
        assert_eq!(kick.release, 0.12);
        assert_eq!(kick.lfo, None);
        assert_eq!(kick.formula, None);
        assert_eq!(kick.scale, 1.0);
        assert_eq!(kick.offset, 0.0);
        assert_eq!(kick.curve, crate::ast::ControlCurve::Linear);
        let wobble = &project.properties.control_sources[1];
        assert_eq!(wobble.lfo.unwrap().shape, crate::ast::ControlLfoShape::Sine);
        assert_eq!(wobble.lfo.unwrap().rate_hz, 2.0);
        assert_eq!(wobble.formula.as_deref(), Some("1-x"));
        assert_eq!(wobble.curve, crate::ast::ControlCurve::Sqrt);
        let gain = project.tracks[1].mix.control_gain.as_ref().unwrap();
        assert_eq!(gain.source, "kick_env");
        assert_eq!(gain.depth_db, -12.0);

        // `sidechain(...)` is the same tap; the canonical spelling is `track(...)`.
        let alias = crate::parse(
            "@project\n@control_source duck | from: sidechain(kick), attack: 5ms, release: 5ms\n",
        )
        .unwrap();
        assert_eq!(alias.properties.control_sources[0].source_track, "kick");

        let text = super::super::serializer::serialize(&project);
        assert_eq!(text, source, "serializer must reproduce the canonical text");
        assert_eq!(crate::parse(&text).unwrap(), project);

        crate::validate(&project).unwrap();
    }

    #[test]
    fn control_source_blocks_accept_properties_on_their_own_lines() {
        let project = crate::parse(
            "@project\n@control_source duck | from: track(kick)\n  attack: 3ms\n  release: 80ms\n  lfo: triangle@0.5Hz\n  lfo_depth: 0.25\n  formula: x*x\n  scale: 1.5\n  offset: 0.1\n  curve: squared\n\n@track kick | instrument: drums/kick\n",
        )
        .unwrap();
        let duck = &project.properties.control_sources[0];
        assert_eq!(duck.attack, 0.003);
        assert_eq!(duck.release, 0.08);
        assert_eq!(
            duck.lfo.unwrap().shape,
            crate::ast::ControlLfoShape::Triangle
        );
        assert_eq!(duck.lfo.unwrap().rate_hz, 0.5);
        assert_eq!(duck.lfo_depth, 0.25);
        assert_eq!(duck.formula.as_deref(), Some("x*x"));
        assert_eq!(duck.scale, 1.5);
        assert_eq!(duck.offset, 0.1);
        assert_eq!(duck.curve, crate::ast::ControlCurve::Squared);
    }

    #[test]
    fn control_source_rejects_unknown_fields_and_bad_formulas() {
        let unknown = crate::parse("@project\n@control_source duck | from: track(k), atack: 5ms\n")
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("atack"), "{unknown}");
        assert!(unknown.contains("attack"), "{unknown}");

        let formula = crate::parse(
            "@project\n@control_source duck | from: track(k), attack: 5ms, release: 5ms, formula: sin(x)\n",
        )
        .unwrap_err()
        .to_string();
        assert!(formula.contains("unknown function"), "{formula}");

        let missing = crate::parse("@project\n@control_source duck | from: track(k)\n")
            .unwrap_err()
            .to_string();
        assert!(missing.contains("attack"), "{missing}");
    }

    // ---- Chord tokens ([G4 A4] q mf) ----

    fn chord_phrase(events: &str) -> Vec<NoteEvent> {
        let source = format!(
            "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 4:1:0]\n{events}"
        );
        let mut project = parse(&source).unwrap_or_else(|error| panic!("{error}\n{source}"));
        crate::validate(&project).unwrap_or_else(|errors| panic!("{errors:?}"));
        project.tracks.remove(0).phrases.remove(0).events
    }

    fn chord_refusal(events: &str) -> ParseError {
        let source = format!(
            "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 4:1:0]\n{events}"
        );
        parse(&source).expect_err("the chord should be refused")
    }

    fn syntax_message(error: &ParseError) -> &str {
        match error {
            ParseError::Syntax { message, .. } => message,
            other => panic!("expected a syntax error, got {other}"),
        }
    }

    #[test]
    fn two_note_chord_parses_as_one_simultaneous_event_group() {
        let events = chord_phrase("    [G4 A4] q mf\n    B4 q mf\n");
        assert_eq!(events.len(), 3);
        for event in &events[..2] {
            assert_eq!(event.start_beat, 0.0);
            assert!((event.duration.to_beats() - 1.0).abs() < 1e-9);
            assert_eq!(event.dynamics, Some(Dynamics::MF));
            assert!(!event.explicit_position);
        }
        assert_eq!(
            events[0].pitch,
            Pitch {
                note: NoteName::G,
                accidental: Accidental::Natural,
                octave: 4,
            }
        );
        assert_eq!(
            events[1].pitch,
            Pitch {
                note: NoteName::A,
                accidental: Accidental::Natural,
                octave: 4,
            }
        );
        assert_eq!(events[2].start_beat, 1.0);
    }

    #[test]
    fn chords_accept_three_and_eight_pitches() {
        let events = chord_phrase("    [C4 E4 G4] q mf\n    [C3 D3 E3 F3 G3 A3 B3 C4] q mf\n");
        assert_eq!(events.len(), 11);
        assert_eq!(
            events[..3]
                .iter()
                .map(|event| event.pitch.to_midi())
                .collect::<Vec<_>>(),
            vec![60, 64, 67]
        );
        assert_eq!(
            events[3..]
                .iter()
                .map(|event| event.pitch.to_midi())
                .collect::<Vec<_>>(),
            vec![48, 50, 52, 53, 55, 57, 59, 60]
        );
        assert!(events[3..].iter().all(|event| event.start_beat == 1.0));
    }

    #[test]
    fn dotted_chord_applies_duration_dynamic_and_modifier_to_every_note() {
        let events = chord_phrase("    [C4 E4 G4] q. mf articulation=legato\n    D4 q mf\n");
        assert_eq!(events.len(), 4);
        for event in &events[..3] {
            assert_eq!(event.duration.dots, 1);
            assert!((event.duration.to_beats() - 1.5).abs() < 1e-9);
            assert_eq!(event.articulation, Some(Articulation::Legato));
            assert_eq!(event.dynamics, Some(Dynamics::MF));
        }
        assert_eq!(events[3].start_beat, 1.5);
    }

    #[test]
    fn chord_whitespace_and_bar_lines_leave_positions_alone() {
        let events = chord_phrase("    [ G4   A4 ]\tq mf |\n    |\n    [B4 C5] q mf\n    |\n");
        assert_eq!(events.len(), 4);
        assert!(events[..2].iter().all(|event| event.start_beat == 0.0));
        assert!(events[2..].iter().all(|event| event.start_beat == 1.0));
    }

    #[test]
    fn chord_after_an_explicit_position_keeps_that_position() {
        let events = chord_phrase("    @2 [C4 E4] q mf\n    D4 q mf\n");
        assert_eq!(events.len(), 3);
        assert!(events[..2].iter().all(|event| event.start_beat == 2.0));
        assert!(events[..2].iter().all(|event| event.explicit_position));
        assert_eq!(events[2].start_beat, 3.0);
    }

    #[test]
    fn chord_refusals_are_readable() {
        let rest = chord_refusal("    [rest] q mf\n");
        assert_eq!(
            rest.to_string(),
            "Syntax error at line 7, column 5: rest is not a chord member: write the rest outside the brackets"
        );

        assert_eq!(
            syntax_message(&chord_refusal("    [] q mf\n")),
            "empty chord: expected at least one pitch between '[' and ']'"
        );
        assert_eq!(
            syntax_message(&chord_refusal("    [G4 q mf\n")),
            "unclosed chord: expected ']' on the same line"
        );
        assert_eq!(
            syntax_message(&chord_refusal("    [C4 C4] q mf\n")),
            "chord repeats pitch C4: list each pitch once"
        );
        assert_eq!(
            syntax_message(&chord_refusal("    [C4 H5] q mf\n")),
            "invalid chord member 'H5': expected a pitch like C4, F#3, or Bb5"
        );
    }

    // ---- Comma lists (one position, several note events) ----

    fn comma_phrase(events: &str) -> Vec<NoteEvent> {
        chord_phrase(events)
    }

    fn comma_refusal(events: &str) -> ParseError {
        chord_refusal(events)
    }

    fn syntax_at(error: &ParseError) -> (usize, usize, String) {
        match error {
            ParseError::Syntax { line, col, message } => (*line, *col, message.clone()),
            other => panic!("expected a syntax error, got {other}"),
        }
    }

    #[test]
    fn comma_list_at_one_position_parses_as_simultaneous_events() {
        let events = comma_phrase("    @0.0 G4 q mf, G5 q mf\n");
        assert_eq!(events.len(), 2);
        for event in &events {
            assert_eq!(event.start_beat, 0.0);
            assert!(event.explicit_position);
            assert_eq!(event.duration.base, BaseDuration::Quarter);
            assert_eq!(event.dynamics, Some(Dynamics::MF));
        }
        assert_eq!(events[0].pitch.to_midi(), 67);
        assert_eq!(events[1].pitch.to_midi(), 79);
    }

    #[test]
    fn comma_list_may_carry_a_chord_member() {
        let events = comma_phrase("    @0.0 [G4 G5] q mf, C4 q mf\n");
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|event| event.start_beat == 0.0));
        let pitches: Vec<_> = events.iter().map(|event| event.pitch.to_midi()).collect();
        assert_eq!(pitches, vec![67, 79, 60]);
    }

    #[test]
    fn comma_list_members_keep_their_own_duration_dynamics_and_modifiers() {
        let events = comma_phrase("    @0.0 G4 q mf, G5 h mp articulation=legato\n    A4 q mf\n");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].start_beat, 0.0);
        assert_eq!(events[1].start_beat, 0.0);
        assert!((events[0].duration.to_beats() - 1.0).abs() < 1e-9);
        assert!((events[1].duration.to_beats() - 2.0).abs() < 1e-9);
        assert_eq!(events[0].dynamics, Some(Dynamics::MF));
        assert_eq!(events[1].dynamics, Some(Dynamics::MP));
        assert_eq!(events[0].articulation, None);
        assert_eq!(events[1].articulation, Some(Articulation::Legato));
        assert_eq!(
            events[2].start_beat, 2.0,
            "the line advances by its longest member"
        );
    }

    #[test]
    fn comma_list_without_a_position_shares_the_sequential_beat() {
        let events = comma_phrase("    C4 q mf, E4 q mf\n    G4 q mf\n");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].start_beat, 0.0);
        assert_eq!(events[1].start_beat, 0.0);
        assert!(!events[0].explicit_position);
        assert!(!events[1].explicit_position);
        assert_eq!(events[2].start_beat, 1.0);
    }

    #[test]
    fn comma_list_accepts_three_or_more_members() {
        let events = comma_phrase("    @2 G4 q mf, A4 q mf, B4 q mf, C5 h mp\n");
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|event| event.start_beat == 2.0));
        assert_eq!(events[3].duration.base, BaseDuration::Half);
    }

    #[test]
    fn comma_list_refusals_are_readable_and_at_the_comma() {
        let (line, col, message) = syntax_at(&comma_refusal("    @0.0 G4 q mf,\n"));
        assert_eq!((line, col), (7, 17));
        assert_eq!(
            message,
            "trailing comma: remove the ',' or add another note event after it"
        );

        let (line, col, message) = syntax_at(&comma_refusal("    , G4 q mf\n"));
        assert_eq!((line, col), (7, 5));
        assert_eq!(
            message,
            "comma list starts with ',': write the first note event before the comma"
        );

        let (line, col, message) = syntax_at(&comma_refusal("    @0.0 ,\n"));
        assert_eq!((line, col), (7, 10));
        assert_eq!(
            message,
            "position '@0.0' has no note event: write e.g. '@0.0 G4 q mf'"
        );

        let (line, col, message) = syntax_at(&comma_refusal("    @0.0 G4 q mf, G5\n"));
        assert_eq!((line, col), (7, 17));
        assert_eq!(
            message,
            "comma list member 'G5' has no duration: write e.g. 'G5 q mf'"
        );

        let (line, col, message) = syntax_at(&comma_refusal("    @0.0 G4 q mf,\n    G5 q mf\n"));
        assert_eq!((line, col), (7, 17));
        assert_eq!(
            message,
            "comma list cannot span a line break: keep every member on one line"
        );
    }

    #[test]
    fn comma_list_works_in_a_pattern_events_block() {
        let source = "@project\n@pattern p | instrument: synth/lead\n  events:\n    @0 [G4 G5] q mf, C4 q mf\n    D4 q mf\n";
        let project = parse(source).unwrap();
        let events = &project.patterns[0].events;
        assert_eq!(events.len(), 4);
        assert!(events[..3].iter().all(|event| event.start_beat == 0.0));
        assert_eq!(events[3].start_beat, 1.0);
    }

    #[test]
    fn comma_list_refuses_a_repeated_pitch() {
        let (line, col, message) = syntax_at(&comma_refusal("    @0.0 C4 q mf, C4 q mf\n"));
        assert_eq!((line, col), (7, 19));
        assert_eq!(message, "comma list repeats pitch C4: list each pitch once");
    }

    // ---- Sustain pedal (@2.0 pedal down / @4.0 pedal up) ----

    fn pedal_source(events: &str) -> String {
        format!(
            "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 4:1:0]\n{events}"
        )
    }

    #[test]
    fn pedal_marks_parse_in_a_phrase_without_moving_the_notes() {
        let source = pedal_source(
            "    @0 pedal down\n    C4 q mf\n    rest q\n    @4.0 pedal up\n    E4 h mf\n",
        );
        let phrase = parse(&source).unwrap().tracks.remove(0).phrases.remove(0);
        assert_eq!(
            phrase.pedals,
            vec![
                PedalEvent {
                    beat: 0.0,
                    down: true
                },
                PedalEvent {
                    beat: 4.0,
                    down: false
                },
            ]
        );
        // A pedal mark takes no time: the notes keep their sequential positions.
        assert_eq!(phrase.events.len(), 3);
        assert_eq!(phrase.events[0].start_beat, 0.0);
        assert_eq!(phrase.events[1].start_beat, 1.0);
        assert_eq!(phrase.events[2].start_beat, 2.0);
    }

    #[test]
    fn pedal_marks_parse_in_a_pattern_events_block() {
        let source = "@project\n@pattern p | instrument: piano/acoustic_grand\n  events:\n    @0 pedal down\n    C4 q mf\n    @2 pedal up\n";
        let pattern = parse(source).unwrap().patterns.remove(0);
        assert_eq!(pattern.pedals.len(), 2);
        assert!(pattern.pedals[0].down);
        assert_eq!(pattern.pedals[1].beat, 2.0);
    }

    #[test]
    fn pedal_without_a_position_is_refused_by_name() {
        let error = parse(&pedal_source("    pedal down\n")).expect_err("no position");
        assert_eq!(
            syntax_message(&error),
            "pedal needs a position: write `@2.0 pedal down` or `@4.0 pedal up`"
        );
    }

    #[test]
    fn pedal_outside_a_phrase_is_refused_by_name() {
        let source = "@project\n@track t | instrument: piano/acoustic_grand\n  @2.0 pedal down\n";
        let error = parse(source).expect_err("a pedal outside a phrase");
        assert_eq!(
            syntax_message(&error),
            "pedal belongs to a phrase: write it inside `@phrase` or a pattern's `events:` block"
        );

        let top = "@project\n@2.0 pedal down\n";
        let error = parse(top).expect_err("a pedal at the project level");
        assert_eq!(
            syntax_message(&error),
            "pedal belongs to a phrase: write it inside `@phrase` or a pattern's `events:` block"
        );
    }

    #[test]
    fn pedal_in_a_hits_block_is_refused_by_name() {
        let source = "@project\n@pattern beat | instrument: drums/acoustic_kit\n  hits:\n    @2.0 pedal down\n    kick 1:0 f\n";
        let error = parse(source).expect_err("a pedal on drum hits");
        assert_eq!(
            syntax_message(&error),
            "pedal needs notes: drum hits are one-shots, so put the pedal in a @phrase or in a pattern's `events:` block"
        );
    }

    // Every top-level block parses in any order. The blocks below differ only in
    // where they sit, so the ASTs must be identical: a track may be followed by
    // a pattern definition or by a section.

    const ORDER_HEADER: &str =
        "@project \"Order\"\n  tempo: 120\n  time_signature: 4/4\n  key: C major\n";

    const ORDER_PRESET: &str =
        "\n@preset lead_patch\n  source: builtin\n  params: {waveform: saw}\n";

    const ORDER_PATTERNS: &str = r#"
@pattern riff | instrument: synth/lead_square, preset: lead_patch
  events:
    C4 q mf
    E4 q mf
    G4 h mf

@pattern groove | instrument: drums/acoustic_kit
  hits:
    kick 1:0 ff
    snare 2:0 f
"#;

    const ORDER_SECTION: &str = r#"
@section verse [1:1 - 9:1]
  repeat: 2
  label: "Verse"
"#;

    const ORDER_LEAD_TRACK: &str = r#"
@track lead | instrument: synth/lead_square, preset: lead_patch
  clip: riff [1:1 - 9:1]
"#;

    const ORDER_DRUM_TRACK: &str = r#"
@track drums | instrument: drums/acoustic_kit
  @pattern beat [1:1 - 3:1] x2
    kick 1:0 ff
    snare 2:0 f
"#;

    const ORDER_TAIL: &str = "\n@bus drums_bus\n  mix: vol=-2dB\n\n@master\n  volume: -1dB\n";

    /// The five block kinds in the order the serializer writes them.
    fn canonical_order() -> String {
        format!(
            "{ORDER_HEADER}{ORDER_PRESET}{ORDER_PATTERNS}{ORDER_SECTION}\
             {ORDER_LEAD_TRACK}{ORDER_DRUM_TRACK}{ORDER_TAIL}"
        )
    }

    fn parsed(source: &str) -> Project {
        parse(source).unwrap_or_else(|error| panic!("{error}\n{source}"))
    }

    #[test]
    fn a_pattern_definition_after_a_track_parses_to_the_same_ast() {
        let source = format!(
            "{ORDER_HEADER}{ORDER_PRESET}{ORDER_SECTION}{ORDER_LEAD_TRACK}\
             {ORDER_PATTERNS}{ORDER_DRUM_TRACK}{ORDER_TAIL}"
        );
        let project = parsed(&source);
        assert_eq!(project, parsed(&canonical_order()));
        assert_eq!(project.patterns.len(), 2, "both definitions are top level");
        assert_eq!(
            project.tracks[0].patterns.len(),
            0,
            "the lead track carries no drum pattern"
        );
    }

    #[test]
    fn a_section_after_a_track_parses_to_the_same_ast() {
        let source = format!(
            "{ORDER_HEADER}{ORDER_PRESET}{ORDER_PATTERNS}{ORDER_LEAD_TRACK}\
             {ORDER_DRUM_TRACK}{ORDER_SECTION}{ORDER_TAIL}"
        );
        let project = parsed(&source);
        assert_eq!(project, parsed(&canonical_order()));
        assert_eq!(project.sections.len(), 1);
        assert_eq!(project.sections[0].repeat, Some(2));
    }

    /// The same five block kinds interleaved: a section, a track, a preset, a
    /// definition, a bus, a track with an inline drum pattern, a definition and
    /// the master, in that order.
    const ORDER_INTERLEAVED: &str = r#"@project "Order"
  tempo: 120
  time_signature: 4/4
  key: C major

@section verse [1:1 - 9:1]
  repeat: 2
  label: "Verse"

@track lead | instrument: synth/lead_square, preset: lead_patch
  clip: riff [1:1 - 9:1]

@preset lead_patch
  source: builtin
  params: {waveform: saw}

@pattern riff | instrument: synth/lead_square, preset: lead_patch
  events:
    C4 q mf
    E4 q mf
    G4 h mf

@bus drums_bus
  mix: vol=-2dB

@track drums | instrument: drums/acoustic_kit
  @pattern beat [1:1 - 3:1] x2
    kick 1:0 ff
    snare 2:0 f

@pattern groove | instrument: drums/acoustic_kit
  hits:
    kick 1:0 ff
    snare 2:0 f

@master
  volume: -1dB
"#;

    #[test]
    fn every_block_kind_parses_in_any_order() {
        assert_eq!(parsed(ORDER_INTERLEAVED), parsed(&canonical_order()));
    }

    #[test]
    fn an_interleaved_project_round_trips_through_the_serializer() {
        let project = parsed(ORDER_INTERLEAVED);
        let text = crate::serializer::serialize(&project);
        let reparsed = parsed(&text);
        assert_eq!(reparsed, project);
        assert_eq!(crate::serializer::serialize(&reparsed), text);
    }

    #[test]
    fn an_inline_drum_pattern_stays_inside_its_track() {
        let source = format!("{ORDER_HEADER}{ORDER_DRUM_TRACK}");
        let project = parsed(&source);
        assert!(project.patterns.is_empty(), "no top-level definition");
        assert_eq!(project.tracks[0].patterns.len(), 1);
        assert_eq!(project.tracks[0].patterns[0].repeat, 2);
    }

    #[test]
    fn track_content_after_a_block_is_refused_by_name() {
        let source = "@project\n@track lead | instrument: synth/lead_square\n@section verse [1:1 - 9:1]\n  mix: vol=0dB\n";
        let error = parse(source).expect_err("the mix line belongs to the track");
        assert_eq!(
            syntax_message(&error),
            "`mix:` is track content, not a block: write it inside its `@track` block, above the next `@` keyword; line 4 is outside every open block"
        );

        let source = "@project\n@track lead | instrument: synth/lead_square\n@section verse [1:1 - 9:1]\n  @phrase intro [1:1 - 2:1]\n    C4 w\n";
        let error = parse(source).expect_err("the phrase belongs to the track");
        assert_eq!(
            syntax_message(&error),
            "`@phrase` is track content, not a block: write it inside its `@track` block, above the next `@` keyword; line 4 is outside every open block"
        );
    }

    #[test]
    fn an_inline_drum_pattern_outside_a_track_is_refused_by_name() {
        let source = "@project\n@bus main\n  mix: vol=0dB\n@pattern beat [1:1 - 2:1]\n  kick 1:0\n";
        let error = parse(source).expect_err("a drum pattern needs a track");
        assert_eq!(
            syntax_message(&error),
            "an inline drum pattern is track content, not a block: write it inside its `@track` block, above the next `@` keyword; line 4 is outside every open block"
        );
    }

    #[test]
    fn a_malformed_section_still_names_its_own_problem() {
        let source = "@project\n@track lead | instrument: synth/lead_square\n@section verse [1:1 - 9:1]\n  label: Verse\n";
        let error = parse(source).expect_err("the label needs quotes");
        assert_eq!(syntax_message(&error), "expected string_literal");
    }

    #[test]
    fn swing_subdivision_round_trips_and_defaults_to_eighth() {
        let plain = parse(
            r#"@project "x"
  tempo: 120
"#,
        )
        .unwrap();
        assert_eq!(
            plain.properties.swing_subdivision,
            crate::ast::SwingSubdivision::Eighth
        );
        assert!(!crate::serializer::serialize(&plain).contains("swing_subdivision"));

        let sixteenth = parse(
            r#"@project "x"
  tempo: 120
  swing: 55%
  swing_subdivision: 16th
"#,
        )
        .unwrap();
        assert_eq!(
            sixteenth.properties.swing_subdivision,
            crate::ast::SwingSubdivision::Sixteenth
        );
        let text = crate::serializer::serialize(&sixteenth);
        assert!(text.contains("swing_subdivision: 16th"), "{text}");
        let reparsed = parse(&text).unwrap();
        assert_eq!(
            reparsed.properties.swing_subdivision,
            crate::ast::SwingSubdivision::Sixteenth
        );
    }
}
