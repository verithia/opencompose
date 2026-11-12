//! Serializer: AST → OpenCompose DSL text
//!
//! Produces canonical .ocp text from an AST. The output should round-trip:
//! parse(serialize(ast)) == ast (modulo whitespace/comments)

use crate::ast::*;

// Public helpers for ops module (serialize individual nodes)
/// Append a track and its phrases to DSL text.
pub fn serialize_track_to(track: &Track, out: &mut String) {
    serialize_track(track, None, out);
}
/// Append a phrase to DSL text.
pub fn serialize_phrase_to(phrase: &Phrase, out: &mut String) {
    serialize_phrase(phrase, None, out);
}
/// Append a pattern instance to DSL text.
pub fn serialize_pattern_to(pattern: &Pattern, out: &mut String) {
    serialize_pattern(pattern, out);
}
/// Append a named pattern definition to DSL text.
pub fn serialize_pattern_def_to(pat: &PatternDef, out: &mut String) {
    serialize_pattern_def(pat, out);
}
/// Append a bus to DSL text.
pub fn serialize_bus_to(bus: &Bus, out: &mut String) {
    serialize_bus(bus, out);
}
/// Append the master strip to DSL text.
pub fn serialize_master_to(master: &Master, out: &mut String) {
    serialize_master(master, out);
}
/// Append a preset definition to DSL text.
pub fn serialize_preset_to(preset: &PresetDef, out: &mut String) {
    serialize_preset_def(preset, out);
}
/// Append a section to DSL text.
pub fn serialize_section(section: &Section, out: &mut String) {
    serialize_section_inner(section, out);
}
/// Append an automation lane to DSL text.
pub fn serialize_automation_to(lane: &AutomationLane, out: &mut String) {
    serialize_automation(lane, out);
}
/// Append an effect chain to DSL text.
pub fn serialize_effects_to(effects: &[Effect], out: &mut String) {
    serialize_effects(effects, out);
}

/// Serialize a Project AST to OpenCompose DSL text
pub fn serialize(project: &Project) -> String {
    let mut out = String::new();

    // Version header
    if let Some(ver) = &project.ocp_version {
        out.push_str(&format!("@ocp \"{}\"\n\n", ver));
    }

    // Project header
    if let Some(name) = &project.name {
        out.push_str(&format!("@project \"{}\"\n", name));
    } else {
        out.push_str("@project\n");
    }
    out.push_str(&format!(
        "  tempo: {}\n",
        format_number(project.properties.tempo)
    ));
    out.push_str(&format!(
        "  time_signature: {}/{}\n",
        project.properties.time_signature.numerator, project.properties.time_signature.denominator
    ));
    if let Some(key) = &project.properties.key {
        out.push_str(&format!("  key: {}\n", format_key(key)));
    }
    if let Some(swing) = project.properties.swing {
        out.push_str(&format!("  swing: {}%\n", format_number(swing)));
    }
    if project.properties.swing_subdivision == crate::ast::SwingSubdivision::Sixteenth {
        out.push_str("  swing_subdivision: 16th\n");
    }
    if !project.properties.solo_mode.is_both() {
        out.push_str(&format!("  solo_mode: {}\n", project.properties.solo_mode));
    }
    if let Some(mpe) = &project.properties.mpe {
        let mut settings = Vec::new();
        if let Some(range) = mpe.lower {
            settings.push(format!("lower: {}-{}", range.first, range.last));
        }
        if let Some(range) = mpe.upper {
            settings.push(format!("upper: {}-{}", range.first, range.last));
        }
        if let Some(range) = mpe.bend_range {
            settings.push(format!("bend_range: {}", format_number(range)));
        }
        out.push_str(&format!("  mpe: {{{}}}\n", settings.join(", ")));
    }
    if let Some(export) = &project.properties.export {
        let mut settings = format!(
            "container: {}, wav: {{depth: {}, dither: {}}}",
            export.container.as_str(),
            export.wav.depth.as_str(),
            export.wav.dither
        );
        // The FLAC block only appears when it says something the defaults do
        // not, so a WAV project's block keeps its historical spelling.
        if export.flac != crate::ast::FlacSettings::default() {
            settings.push_str(&format!(
                ", flac: {{depth: {}, level: {}}}",
                export.flac.depth.as_str(),
                export.flac.level
            ));
        }
        // The same rule for MP3: a WAV project never grows an `mp3:` block.
        if export.mp3 != crate::ast::Mp3Settings::default() {
            let mut mp3 = vec![format!("bitrate: {}", export.mp3.bitrate)];
            if let Some(quality) = export.mp3.vbr_quality {
                mp3.push(format!("vbr_quality: {quality}"));
            }
            settings.push_str(&format!(", mp3: {{{}}}", mp3.join(", ")));
        }
        if let Some(rate) = export.sample_rate {
            settings.push_str(&format!(", sample_rate: {rate}"));
        }
        if let Some(step) = export.resample {
            settings.push_str(&format!(", resample: {step}"));
        }
        out.push_str(&format!("  export: {{{settings}}}\n"));
    }
    if let Some(step) = project.properties.live_resample {
        out.push_str(&format!("  live_resample: {step}\n"));
    }
    if let Some(version) = project.properties.automation_normalization {
        out.push_str(&format!("  automation_normalization: {version}\n"));
    }

    if let Some(clock) = &project.properties.picture_clock {
        out.push_str("  ");
        out.push_str(&picture_clock_property(clock));
        out.push('\n');
    }
    // Tempo map
    if !project.properties.tempo_map.is_empty() {
        out.push_str("  tempo_map:\n");
        for pt in &project.properties.tempo_map {
            out.push_str(&format!(
                "    {}{} = {} bpm\n",
                format_bar_beat(&pt.position),
                if pt.offset_quarters == 0.0 {
                    String::new()
                } else {
                    format!(" + {}q", pt.offset_quarters)
                },
                format_number(pt.bpm)
            ));
        }
    }

    // Time signature map
    if !project.properties.time_sig_map.is_empty() {
        out.push_str("  time_sig_map:\n");
        for pt in &project.properties.time_sig_map {
            out.push_str(&format!(
                "    {} = {}/{}\n",
                format_bar_beat(&pt.position),
                pt.numerator,
                pt.denominator
            ));
        }
    }

    // Key map
    if !project.properties.key_map.is_empty() {
        out.push_str("  key_map:\n");
        for pt in &project.properties.key_map {
            out.push_str(&format!(
                "    {} = {}\n",
                format_bar_beat(&pt.position),
                format_key(&pt.key)
            ));
        }
    }

    // Audio-derived control sources, before the tracks that consume them.
    for source in &project.properties.control_sources {
        out.push('\n');
        serialize_control_source(source, &mut out);
    }

    // Control links next to the sources; both are project-level records.
    for link in &project.properties.control_links {
        out.push('\n');
        serialize_control_link(link, &mut out);
    }

    // Presets; drum kits are authored as @kit blocks, not raw preset params.
    for preset in &project.presets {
        out.push('\n');
        if is_kit_preset(preset) {
            serialize_kit(preset, &mut out);
        } else {
            serialize_preset_def(preset, &mut out);
        }
    }

    // Named patterns (top-level)
    for pattern in &project.patterns {
        out.push('\n');
        serialize_pattern_def(pattern, &mut out);
    }

    // Audio clips (top-level)
    for clip in project.audio_clips() {
        out.push('\n');
        serialize_audio_clip(clip, &mut out);
    }

    // Stored tuning decisions, after the clips they reference
    for tune in &project.properties.vocal_tunes {
        out.push('\n');
        serialize_vocal_tune(tune, &mut out);
    }

    // Harmony voices, after the tuning they read their scale from
    for harmony in &project.properties.vocal_harmonies {
        out.push('\n');
        serialize_vocal_harmony(harmony, &mut out);
    }

    // Sections
    for section in &project.sections {
        out.push('\n');
        serialize_section_inner(section, &mut out);
    }

    // Markers last of the timeline landmarks: they annotate the spans above.
    for marker in &project.markers {
        out.push('\n');
        out.push_str(&format!(
            "@marker {} at {}\n",
            format_name(&marker.name),
            format_bar_beat(&marker.position)
        ));
    }

    // Clip groups, next to the markers: both annotate the arrangement, and a
    // group names its members by identifier rather than by position.
    for group in &project.groups {
        out.push('\n');
        out.push_str(&format!("@group {} {{\n", format_name(&group.id)));
        out.push_str("  members:");
        for (index, member) in group.members.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!(" {}", format_name(member)));
        }
        out.push_str("\n}\n");
    }

    // Tracks
    for track in &project.tracks {
        out.push('\n');
        serialize_track(track, Some(&project.properties), &mut out);
    }

    // Buses
    for bus in &project.buses {
        out.push('\n');
        serialize_bus(bus, &mut out);
    }

    // Master
    if let Some(master) = &project.master {
        out.push('\n');
        serialize_master(master, &mut out);
    }

    out
}

fn serialize_audio_clip(clip: &AudioClip, out: &mut String) {
    out.push_str(&format!(
        "@audio_clip {} [{} - {}] | track: {}\n",
        format_name(&clip.name),
        format_bar_beat(&clip.time_range.start),
        format_bar_beat(&clip.time_range.end),
        format_name(&clip.track)
    ));
    out.push_str(&format!("  file: \"{}\"\n", clip.file));
    for (field, value) in [
        ("placement_start", clip.placement.start_offset_seconds),
        ("placement_end", clip.placement.end_offset_seconds),
    ] {
        if value != 0.0 {
            out.push_str(&format!("  {field}: {}s\n", format_number(value)));
        }
    }
    if clip.offset_seconds != 0.0 {
        out.push_str(&format!(
            "  offset: {}s\n",
            format_number(clip.offset_seconds)
        ));
    }
    if clip.loop_seconds != 0.0 {
        out.push_str(&format!("  loop: {}s\n", format_number(clip.loop_seconds)));
    }
    if clip.gain_db != 0.0 {
        out.push_str(&format!("  gain: {}dB\n", format_signed(clip.gain_db)));
    }
    if clip.fade_in_seconds != 0.0 {
        out.push_str(&format!(
            "  fade_in: {}s\n",
            format_number(clip.fade_in_seconds)
        ));
    }
    if clip.fade_out_seconds != 0.0 {
        out.push_str(&format!(
            "  fade_out: {}s\n",
            format_number(clip.fade_out_seconds)
        ));
    }
    if clip.invert {
        out.push_str("  invert: true\n");
    }
    if clip.crossfade_seconds != 0.0 {
        out.push_str(&format!(
            "  crossfade: {}s\n",
            format_number(clip.crossfade_seconds)
        ));
    }
    if !clip.gain_envelope.is_empty() {
        out.push_str("  gain_envelope:");
        for (index, point) in clip.gain_envelope.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                " {}:{}dB",
                format_number(point.position),
                format_signed(point.gain_db)
            ));
        }
        out.push('\n');
    }
    if clip.stretch != 1.0 {
        out.push_str(&format!("  stretch: {}\n", format_number(clip.stretch)));
    }
    if clip.stretch_mode != StretchMode::Musical {
        out.push_str(&format!("  mode: {}\n", clip.stretch_mode.as_str()));
    }
    if clip.bypass_processing {
        out.push_str("  bypass_processing: true\n");
    }
    if clip.tempo_follow {
        out.push_str("  tempo_follow: true\n");
    }
    if clip.pitch != 0.0 {
        out.push_str(&format!("  pitch: {}\n", format_signed(clip.pitch)));
    }
    if clip.formant != 0.0 {
        out.push_str(&format!("  formant: {}\n", format_signed(clip.formant)));
    }
    if clip.preserve_formants {
        out.push_str("  preserve_formants: true\n");
    }
    for marker in &clip.warp_markers {
        out.push_str(&format!(
            "  warp: {}s -> {}s\n",
            marker.source_seconds, marker.target_seconds
        ));
    }
    if let Some(group) = &clip.take_group {
        out.push_str(&format!("  take_group: \"{group}\"\n"));
    }
    if let Some(take) = clip.take {
        out.push_str(&format!("  take: {take}\n"));
    }
    if clip.muted {
        out.push_str("  muted: true\n");
    }
}

fn serialize_vocal_tune(tune: &VocalTune, out: &mut String) {
    out.push_str(&format!("@vocal_tune {}", format_name(&tune.name)));
    let mut selector = Vec::new();
    if let Some(clip) = &tune.clip {
        selector.push(format!("clip: {}", format_name(clip)));
    }
    if let Some(track) = &tune.track {
        selector.push(format!("track: {}", format_name(track)));
    }
    if !selector.is_empty() {
        out.push_str(&format!(" | {}", selector.join(", ")));
    }
    out.push('\n');
    if let Some(scale) = &tune.scale {
        out.push_str(&format!("  scale: {}\n", format_key(scale)));
    }
    if tune.snap != VocalSnapPolicy::Auto {
        out.push_str(&format!("  snap: {}\n", tune.snap.as_str()));
    }
    if let Some(reference) = &tune.reference {
        out.push_str(&format!("  reference: {}\n", format_name(reference)));
    }
    if tune.snap_cap_cents != 100.0 {
        out.push_str(&format!(
            "  cap: {}ct\n",
            format_number(tune.snap_cap_cents)
        ));
    }
    if tune.snap_window_cents != 35.0 {
        out.push_str(&format!(
            "  window: {}ct\n",
            format_number(tune.snap_window_cents)
        ));
    }
    if tune.max_move_seconds != 0.120 {
        out.push_str(&format!(
            "  max_move: {}\n",
            format_seconds(tune.max_move_seconds)
        ));
    }
    out.push_str(&format!("  amount: {}%\n", format_number(tune.amount)));
    if tune.retune_seconds != 0.025 {
        out.push_str(&format!(
            "  retune: {}\n",
            format_seconds(tune.retune_seconds)
        ));
    }
    if tune.formant_cents != 0.0 {
        out.push_str(&format!(
            "  formant: {}ct\n",
            format_signed(tune.formant_cents)
        ));
    }
    if tune.vibrato != VocalVibratoMode::Preserve {
        out.push_str(&format!("  vibrato: {}\n", tune.vibrato.as_str()));
    }
    if !tune.notes.is_empty() {
        out.push_str("  notes:\n");
        for note in &tune.notes {
            out.push_str("    - ");
            let mut items = vec![format!("at={}", format_seconds(note.at))];
            if let Some(target) = &note.target {
                items.push(format!("tgt={}", format_vocal_target(target)));
            }
            if let Some(amount) = note.amount {
                items.push(format!("amount={}%", format_number(amount)));
            }
            if let Some(retune) = note.retune_seconds {
                items.push(format!("retune={}", format_seconds(retune)));
            }
            if let Some(formant) = note.formant_cents {
                items.push(format!("formant={}ct", format_signed(formant)));
            }
            if let Some(start) = note.start_seconds {
                items.push(format!("start={}", format_seconds(start)));
            }
            if let Some(length) = note.length {
                items.push(format!("length={}", format_number(length)));
            }
            if let Some(vibrato) = note.vibrato {
                items.push(format!("vibrato={}", vibrato.as_str()));
            }
            if note.bypass {
                items.push("bypass=true".to_string());
            }
            out.push_str(&items.join(" "));
            out.push('\n');
        }
    }
}

fn serialize_vocal_harmony(harmony: &VocalHarmony, out: &mut String) {
    out.push_str(&format!("@vocal_harmony {}", format_name(&harmony.name)));
    let mut selector = Vec::new();
    if let Some(clip) = &harmony.clip {
        selector.push(format!("clip: {}", format_name(clip)));
    }
    if let Some(track) = &harmony.track {
        selector.push(format!("track: {}", format_name(track)));
    }
    if !selector.is_empty() {
        out.push_str(&format!(" | {}", selector.join(", ")));
    }
    out.push('\n');
    out.push_str(&format!("  interval: {}\n", harmony.interval));
    out.push_str(&format!("  level: {}dB\n", format_signed(harmony.level_db)));
    if harmony.pan != 0.0 {
        out.push_str(&format!("  pan: {}\n", format_signed(harmony.pan)));
    }
}

fn format_vocal_target(target: &VocalTarget) -> String {
    match target {
        VocalTarget::Pitch(midi) => format_midi(*midi),
        VocalTarget::Cents(cents) => format!("{}ct", format_number(*cents)),
        VocalTarget::Semitones(semitones) => {
            format!("{}st", format_signed_always(*semitones))
        }
        VocalTarget::OffsetCents(cents) => format!("{}ct", format_signed_always(*cents)),
        VocalTarget::Degree { degree, octave } => format!("{degree}@{octave}"),
    }
}

/// Milliseconds when the conversion is lossless, otherwise plain seconds.
fn format_seconds(seconds: f64) -> String {
    let millis = format_number(seconds * 1000.0);
    if millis
        .parse::<f64>()
        .ok()
        .map(|value| (value / 1000.0).to_bits())
        == Some(seconds.to_bits())
    {
        format!("{millis}ms")
    } else {
        format!("{}s", format_number(seconds))
    }
}

fn format_signed_always(value: f64) -> String {
    if value >= 0.0 {
        format!("+{}", format_number(value))
    } else {
        format!("-{}", format_number(value.abs()))
    }
}

fn format_midi(midi: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[(midi % 12) as usize],
        i32::from(midi) / 12 - 1
    )
}

/// The engine's follower is the only control law; this writes the settings.
fn serialize_control_source(source: &ControlSource, out: &mut String) {
    let mut props = vec![format!(
        "from: track({})",
        format_name(&source.source_track)
    )];
    props.push(format!("attack: {}", format_seconds(source.attack)));
    props.push(format!("release: {}", format_seconds(source.release)));
    props.push(format!(
        "lfo: {}",
        match &source.lfo {
            None => "none".to_string(),
            Some(lfo) => format!(
                "{}@{}Hz",
                lfo_shape_name(lfo.shape),
                format_number(lfo.rate_hz)
            ),
        }
    ));
    if source.lfo.is_some() && source.lfo_depth != 1.0 {
        props.push(format!("lfo_depth: {}", format_number(source.lfo_depth)));
    }
    props.push(format!(
        "formula: {}",
        source.formula.as_deref().unwrap_or("none")
    ));
    if source.scale != 1.0 {
        props.push(format!("scale: {}", format_number(source.scale)));
    }
    if source.offset != 0.0 {
        props.push(format!("offset: {}", format_number(source.offset)));
    }
    if source.curve != ControlCurve::Linear {
        props.push(format!("curve: {}", control_curve_name(source.curve)));
    }
    out.push_str(&format!(
        "@control_source {} | {}\n",
        format_name(&source.name),
        props.join(", ")
    ));
}

fn lfo_shape_name(shape: ControlLfoShape) -> &'static str {
    match shape {
        ControlLfoShape::Sine => "sine",
        ControlLfoShape::Triangle => "triangle",
        ControlLfoShape::Square => "square",
        ControlLfoShape::Saw => "saw",
    }
}

fn control_curve_name(curve: ControlCurve) -> &'static str {
    match curve {
        ControlCurve::Linear => "linear",
        ControlCurve::Squared => "squared",
        ControlCurve::Sqrt => "sqrt",
        ControlCurve::Log => "log",
    }
}

fn serialize_control_link(link: &ControlLink, out: &mut String) {
    out.push_str(&format!("@control_link {}\n", format_name(&link.name)));
    out.push_str(&format!(
        "  from: {}\n",
        control_link_source_name(&link.source)
    ));
    for target in &link.targets {
        out.push_str(&format!(
            "  -> track: {} param: {} formula: {}",
            format_name(&target.track),
            format_param(&target.param),
            target.formula
        ));
        if target.smoothing_ms > 0 {
            out.push_str(&format!(" smoothing: {}ms", target.smoothing_ms));
        }
        out.push('\n');
    }
}

fn control_link_source_name(source: &ControlLinkSource) -> String {
    match source {
        ControlLinkSource::MidiCc {
            channel,
            controller,
        } => {
            if *channel == 1 {
                format!("cc({controller})")
            } else {
                format!("cc({controller}, {channel})")
            }
        }
        ControlLinkSource::Macro { index } => format!("macro({index})"),
        ControlLinkSource::Lane { track, param } => {
            format!("lane({}, {})", format_name(track), format_param(param))
        }
    }
}

/// Quote a parameter key only when it is not a bare token the grammar accepts.
fn format_param(param: &str) -> String {
    let mut chars = param.chars();
    let bare = chars
        .next()
        .map(|c| c.is_ascii_alphanumeric() || c == '_')
        .unwrap_or(false)
        && param
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if bare {
        param.to_string()
    } else {
        format!("\"{}\"", param.replace('"', "'"))
    }
}

fn serialize_preset_def(preset: &PresetDef, out: &mut String) {
    out.push_str(&format!("@preset {}\n", format_name(&preset.name)));
    if let Some(source) = &preset.source {
        out.push_str(&format!("  source: {}\n", source));
    }
    if let Some(plugin) = &preset.plugin {
        out.push_str(&format!("  plugin: \"{}\"\n", plugin));
    }
    if let Some(program) = preset.program {
        out.push_str(&format!("  program: {}\n", program));
    }
    if let Some(patch) = &preset.patch {
        out.push_str(&format!("  patch: \"{}\"\n", patch));
    }
    if let Some(file) = &preset.file {
        out.push_str(&format!("  file: \"{}\"\n", file));
    }
    if let Some(root) = &preset.root_note {
        out.push_str(&format!("  root_note: {}\n", format_pitch(root)));
    }
    if let Some(ext) = &preset.extends {
        out.push_str(&format!("  extends: {}\n", format_name(ext)));
    }
    if !preset.params.is_empty() {
        out.push_str("  params: {");
        for (i, (k, v)) in preset.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}: {}", k, format_map_value(v)));
        }
        out.push_str("}\n");
    }
    if !preset.effects.is_empty() {
        serialize_effects(&preset.effects, out);
    }
}

/// True when a preset document is a drum kit lowered from `@kit`.
pub fn is_kit_preset(preset: &PresetDef) -> bool {
    preset.source.as_deref() == Some("builtin")
        && preset.plugin.as_deref() == Some(crate::DRUMKIT_PLUGIN_ID)
}

fn kit_field<'a>(params: &'a [(String, ParamValue)], key: &str) -> Option<&'a ParamValue> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn serialize_kit(preset: &PresetDef, out: &mut String) {
    out.push_str(&format!("@kit {}\n", format_name(&preset.name)));
    if let Some(value) = kit_field(&preset.params, "kit.max_voices")
        && let Some(voices) = value.as_f64()
    {
        out.push_str(&format!("  max_voices: {}\n", format_number(voices)));
    }

    let mut pads: Vec<&str> = Vec::new();
    for (key, _) in &preset.params {
        let pad = key
            .strip_prefix("pad.")
            .or_else(|| key.strip_prefix("layer."))
            .and_then(|rest| rest.split_once('.').map(|(pad, _)| pad));
        if let Some(pad) = pad
            && !pads.contains(&pad)
        {
            pads.push(pad);
        }
    }

    for pad in pads {
        let mut fields = Vec::new();
        for field in [
            "note", "gain", "pan", "tune", "cents", "decay", "release", "reverse", "one_shot",
            "choke", "voice",
        ] {
            if let Some(value) = kit_field(&preset.params, &format!("pad.{pad}.{field}")) {
                fields.push(format!("{field}: {}", format_param_value(value)));
            }
        }
        out.push_str(&format!("  pad {pad}"));
        if !fields.is_empty() {
            out.push_str(" | ");
            out.push_str(&fields.join(", "));
        }
        out.push('\n');

        let mut indices: Vec<u32> = Vec::new();
        for (key, _) in &preset.params {
            if let Some(rest) = key.strip_prefix(&format!("layer.{pad}."))
                && let Some((index, _)) = rest.split_once('.')
                && let Ok(index) = index.parse::<u32>()
                && !indices.contains(&index)
            {
                indices.push(index);
            }
        }
        indices.sort_unstable();
        for index in indices {
            let prefix = format!("layer.{pad}.{index}.");
            let mut fields = Vec::new();
            for field in ["low", "high", "sample", "gain"] {
                if let Some(value) = kit_field(&preset.params, &format!("{prefix}{field}")) {
                    fields.push(format!("{field}: {}", format_map_value(value)));
                }
            }
            out.push_str(&format!("  layer {pad} | {}\n", fields.join(", ")));
        }
    }
}

fn serialize_track(track: &Track, properties: Option<&ProjectProperties>, out: &mut String) {
    // Header — include both instrument and preset if set
    let mut inline_props = Vec::new();
    if let Some(preset) = &track.preset {
        inline_props.push(format!("preset: {}", format_name(preset)));
    }
    // Always include instrument (even when preset is set)
    inline_props.push(format!("instrument: {}", track.instrument.as_path()));
    if let Some(engine) = &track.engine {
        inline_props.push(format!("engine: {}", engine));
    }
    if let Some(channel) = track.channel {
        inline_props.push(format!("channel: {channel}"));
    }
    if let Some(port) = track.output_port {
        inline_props.push(format!("port: {port}"));
    }
    if let Some(pair) = track.output_pair {
        inline_props.push(format!("pair: {pair}"));
    }
    out.push_str(&format!(
        "@track {} | {}\n",
        format_name(&track.name),
        inline_props.join(", ")
    ));

    // Mix
    if track.mix != MixState::default() {
        out.push_str("  mix: ");
        let mut props = Vec::new();
        if let Some(output) = &track.mix.output {
            props.push(format!("output={}", format_name(output)));
        }
        if track.mix.volume_db != 0.0 {
            props.push(format!("vol={}dB", format_signed(track.mix.volume_db)));
        }
        if track.mix.pan != 0.0 {
            props.push(format!("pan={}", format_number_float(track.mix.pan)));
        }
        if track.mix.stereo_balance {
            props.push("pan_law=balance".to_string());
        }
        for send in &track.mix.sends {
            // Percent conversion is not exactly invertible for every f64.
            // Preserve the legacy spelling where lossless, otherwise write
            // the linear gain directly to retain exact editable state.
            let percent = format_number(send.amount * 100.0);
            let value = if percent.parse::<f64>().ok().map(|v| (v / 100.0).to_bits())
                == Some(send.amount.to_bits())
            {
                format!("{percent}%")
            } else {
                format_number_float(send.amount)
            };
            let tap = if send.pre_fader { ", pre" } else { "" };
            let pan = if send.pan != 0.0 {
                format!(", pan={}", format_number_float(send.pan))
            } else {
                String::new()
            };
            props.push(format!(
                "send({}{}={value}{tap}{pan})",
                send.source
                    .as_ref()
                    .map(|source| format!("{}.out{}->", format_name(&source.owner), source.output))
                    .unwrap_or_default(),
                format_name(&send.target)
            ));
        }
        if track.mix.mute {
            props.push("mute=true".to_string());
        }
        if track.mix.solo {
            props.push("solo=true".to_string());
        }
        if track.mix.polarity_inverted {
            props.push("polarity=invert".to_string());
        }
        if !track.mix.latency_compensation.is_on() {
            props.push(format!("pdc={}", track.mix.latency_compensation.as_str()));
        }
        if track.mix.latency_offset != 0 {
            props.push(format!("latency={}", track.mix.latency_offset));
        }
        if !track.mix.smart_disable {
            props.push("smart_disable=false".to_string());
        }
        if let Some(gain) = &track.mix.control_gain {
            props.push(format!(
                "control({}={}dB)",
                format_name(&gain.source),
                format_signed(gain.depth_db)
            ));
        }
        if let Some(display_name) = &track.mix.overrides.display_name {
            props.push(format!("display_name=\"{}\"", escape_quotes(display_name)));
        }
        if let Some(color) = &track.mix.overrides.color {
            props.push(format!("color=\"{}\"", escape_quotes(color)));
        }
        if track.mix.overrides.solo_safe {
            props.push("solo_safe=true".to_string());
        }
        // If no props were different, at least show vol and pan
        if props.is_empty() {
            props.push("vol=0dB".to_string());
            props.push("pan=0".to_string());
        }
        out.push_str(&props.join(", "));
        out.push('\n');
    }

    // Effects
    if !track.effects.is_empty() {
        serialize_effects(&track.effects, out);
    }

    serialize_input_routes(&track.input_routes, out);

    // Engine params
    if let Some(ref engine_params) = track.engine_params {
        out.push_str("  engine_params: {");
        for (i, (k, v)) in engine_params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}: {}", k, format_map_value(v)));
        }
        out.push_str("}\n");
    }

    // Inline patterns (new model)
    if let Some(timeline) = &track.performance {
        out.push_str("  performance: {\n");
        for event in &timeline.events {
            out.push_str(&format!(
                "    @{} ch{} {};\n",
                event.quarter,
                event.channel,
                crate::performance::format_command(&event.message)
            ));
        }
        out.push_str("  }\n");
    }
    for pattern in &track.inline_patterns {
        serialize_pattern_def(pattern, out);
    }

    // Clips
    for clip in track.pattern_clips() {
        out.push_str(&format!("  clip: {} [", format_name(&clip.pattern)));
        out.push_str(&format_bar_beat(&clip.time_range.start).to_string());
        out.push_str(&format!(" - {}]", format_bar_beat(&clip.time_range.end)));
        if clip.offset_quarters != 0.0 {
            out.push_str(&format!(
                " offset: {}q",
                format_number(clip.offset_quarters)
            ));
        }
        if clip.loop_quarters != 0.0 {
            out.push_str(&format!(" loop: {}q", format_number(clip.loop_quarters)));
        }
        if let Some(id) = &clip.id {
            out.push_str(&format!(" id: {}", format_name(id)));
        }
        out.push('\n');
    }

    // Phrases
    for phrase in &track.phrases {
        serialize_phrase(phrase, properties, out);
    }

    // Patterns
    for pattern in &track.patterns {
        serialize_pattern(pattern, out);
    }

    // Automation
    for lane in track.automation() {
        serialize_automation(lane, out);
    }

    // Post-insert gain clips, including the ones a hairpin expands to
    serialize_gain_automation(&track.gain_automation, out);
}

fn serialize_phrase(phrase: &Phrase, properties: Option<&ProjectProperties>, out: &mut String) {
    out.push_str(&format!("  @phrase {}", format_name(&phrase.name)));
    if let Some(tr) = &phrase.time_range {
        out.push_str(&format!(
            " [{} - {}]",
            format_bar_beat(&tr.start),
            format_bar_beat(&tr.end)
        ));
    }
    if let Some(channel) = phrase.channel {
        out.push_str(&format!(" | channel: {channel}"));
    }
    out.push('\n');

    serialize_pedal_events(&phrase.pedals, out);
    serialize_hairpins(phrase, properties, out);
    serialize_note_events(&phrase.events, out);
}

/// Hairpin marks carry their own position, like pedals. The expansion is shown
/// as a comment because a line the parser would read back is a second ramp on
/// the same span.
fn serialize_hairpins(phrase: &Phrase, properties: Option<&ProjectProperties>, out: &mut String) {
    let expansions = properties
        .map(|properties| crate::dynamics::expand_phrase(phrase, properties).unwrap_or_default())
        .unwrap_or_default();
    for (index, mark) in phrase.hairpins.iter().enumerate() {
        serialize_hairpin_mark(mark, out);
        if let Some(expansion) = expansions.iter().find(|expansion| expansion.mark == index) {
            let mut line = String::new();
            serialize_gain_automation(std::slice::from_ref(&expansion.clip), &mut line);
            out.push_str(&format!("    // {}\n", line.trim()));
        }
    }
}

/// Pattern hairpins keep the phrase's spelling but show no expansion comment:
/// a mark expands once per placement, so there is no single clip to name.
fn serialize_pattern_hairpins(hairpins: &[HairpinMark], out: &mut String) {
    for mark in hairpins {
        serialize_hairpin_mark(mark, out);
    }
}

fn serialize_hairpin_mark(mark: &HairpinMark, out: &mut String) {
    out.push_str(&format!(
        "    @{} {}\n",
        format_number_float(mark.beat),
        mark.kind.keyword()
    ));
}

/// Pedal marks carry their own position, so they keep the order they were
/// written in and never disturb the sequential note lines below them.
fn serialize_pedal_events(pedals: &[PedalEvent], out: &mut String) {
    for pedal in pedals {
        out.push_str(&format!(
            "    @{} pedal {}\n",
            format_number_float(pedal.beat),
            if pedal.down { "down" } else { "up" }
        ));
    }
}

fn serialize_note_events(events: &[NoteEvent], out: &mut String) {
    let mut sequential_beat = 0.0;
    let mut index = 0;
    while index < events.len() {
        let event = &events[index];
        // Check if rest
        let is_rest = event.modifiers.iter().any(|(k, _)| k == "rest");
        if is_rest {
            out.push_str(&format!("    rest {}\n", format_duration(&event.duration)));
            sequential_beat += event.duration.to_beats();
            index += 1;
            continue;
        }

        // Notes that share a start and every other property are one comma list.
        let group_end = comma_group_end(events, index);

        // Output @beat prefix when position differs from sequential expectation
        out.push_str("    ");
        if event.grace {
            out.push_str("@grace ");
        }
        if event.explicit_position || (event.start_beat - sequential_beat).abs() > 0.001 {
            out.push_str(&format!("@{} ", format_number_float(event.start_beat)));
        }

        out.push_str(&format_pitch(&event.pitch));
        out.push(' ');
        push_note_tail(event, out);
        for member in &events[index + 1..group_end] {
            out.push_str(", ");
            out.push_str(&format_pitch(&member.pitch));
            out.push(' ');
            push_note_tail(member, out);
        }
        out.push('\n');

        // Advance sequential position
        sequential_beat = sequential_beat.max(event.start_beat + event.duration.to_beats());
        index = group_end;
    }
}

/// End (exclusive) of the comma list starting at `index`; the members differ
/// only in pitch.
fn comma_group_end(events: &[NoteEvent], index: usize) -> usize {
    let first = &events[index];
    let mut end = index + 1;
    while end < events.len()
        && shares_comma_event(first, &events[end])
        // A comma list refuses a repeated pitch, so a legacy duplicate stays its
        // own event instead of becoming `C4 q, C4 q`.
        && !events[index..end]
            .iter()
            .any(|member| member.pitch == events[end].pitch)
    {
        end += 1;
    }
    end
}

fn shares_comma_event(left: &NoteEvent, right: &NoteEvent) -> bool {
    !right.modifiers.iter().any(|(key, _)| key == "rest")
        && (left.start_beat - right.start_beat).abs() <= 1e-6
        && left.explicit_position == right.explicit_position
        && left.duration == right.duration
        && left.dynamics == right.dynamics
        && left.velocity == right.velocity
        && left.articulation == right.articulation
        && left.vibrato == right.vibrato
        && left.expression == right.expression
        && left.pitch_bend == right.pitch_bend
        && left.muted == right.muted
        && left.grace == right.grace
        && left.text == right.text
        && left.modifiers == right.modifiers
}

/// Everything after the pitch: duration, dynamics and modifiers.
fn push_note_tail(event: &NoteEvent, out: &mut String) {
    out.push_str(&format_duration(&event.duration));

    if let Some(dyn_val) = &event.dynamics {
        out.push_str(&format!(" {}", format_dynamics(dyn_val)));
    }
    if let Some(vel) = event.velocity {
        out.push_str(&format!(" v={}", vel));
    }
    if event.muted {
        out.push_str(" muted=true");
    }
    if let Some(ref txt) = event.text {
        out.push_str(&format!(" text=\"{}\"", txt));
    }

    if let Some(art) = &event.articulation {
        out.push_str(&format!(" articulation={}", format_articulation(art)));
    }

    if let Some(vib) = &event.vibrato {
        out.push_str(" vibrato={");
        let mut params = Vec::new();
        if let Some(rate) = vib.rate_hz {
            params.push(format!("rate: {}Hz", format_number(rate)));
        }
        if let Some(depth) = vib.depth_cents {
            params.push(format!("depth: {}ct", format_number(depth)));
        }
        if let Some(delay) = vib.delay_s {
            params.push(format!("delay: {}s", format_number(delay)));
        }
        out.push_str(&params.join(", "));
        out.push('}');
    }

    if let Some(curves) = &event.expression {
        for curve in curves {
            out.push_str(&format!(
                " expression={{{}: {}",
                curve.param,
                format_param_value(&curve.start)
            ));
            if let Some(end) = &curve.end {
                out.push_str(&format!("->{}", format_param_value(end)));
            }
            out.push('}');
        }
    }

    if let Some(bend) = event.pitch_bend {
        out.push_str(&format!(" bend={}ct", format_number(bend)));
    }

    if event.muted {
        out.push_str(" muted=true");
    }

    // Generic modifiers (skip "rest" which we already handled)
    for (key, val) in &event.modifiers {
        if key == "rest" {
            continue;
        }
        if key == crate::gliss::MODIFIER {
            out.push_str(&format_gliss(val));
            continue;
        }
        let value = if key == "pitch_curve" {
            format_map_value(val)
        } else {
            format_param_value(val)
        };
        out.push_str(&format!(" {}={}", key, value));
    }
}

/// A mark prints as it was written: bare `gliss` for the next note, then the
/// value in its own spelling.
fn format_gliss(value: &ParamValue) -> String {
    match value {
        ParamValue::String(text) if text == crate::gliss::NEXT => " gliss".to_string(),
        _ => format!(" gliss={}", format_param_value(value)),
    }
}

fn serialize_pattern(pattern: &Pattern, out: &mut String) {
    out.push_str(&format!("  @pattern {}", format_name(&pattern.name)));
    if let Some(tr) = &pattern.time_range {
        out.push_str(&format!(
            " [{} - {}]",
            format_bar_beat(&tr.start),
            format_bar_beat(&tr.end)
        ));
    }
    if pattern.repeat > 1 {
        out.push_str(&format!(" x{}", pattern.repeat));
    }
    out.push('\n');

    for hit in &pattern.hits {
        out.push_str(&format!(
            "    {} {}:{}",
            hit.drum, hit.position.beat, hit.position.subdivision
        ));
        if let Some(dyn_val) = &hit.dynamics {
            out.push_str(&format!(" {}", format_dynamics(dyn_val)));
        }
        if let Some(velocity) = hit.velocity {
            out.push_str(&format!(" v={velocity}"));
        }
        if let Some(art) = &hit.articulation {
            out.push_str(&format!(" articulation={}", format_drum_articulation(art)));
        }
        serialize_hit_modifiers(hit, out);
        out.push('\n');
    }
}

fn serialize_hit_modifiers(hit: &HitEvent, out: &mut String) {
    for (key, value) in &hit.modifiers {
        let value = match value {
            // The generic number formatter rounds to four decimal places.
            ParamValue::Number(offset) if key == "grid_offset_beats" => offset.to_string(),
            _ => format_param_value(value),
        };
        out.push_str(&format!(" {key}={value}"));
    }
}

fn serialize_automation(lane: &AutomationLane, out: &mut String) {
    out.push_str(&format!("  automation: {}", lane.param));
    if let Some(range) = &lane.time_range {
        out.push_str(&format!(
            " [{} - {}]",
            format_bar_beat(&range.start),
            format_bar_beat(&range.end)
        ));
    }
    if lane.muted {
        out.push_str(" muted");
    }
    if lane.mode != LaneMode::Envelope {
        out.push_str(&format!(" mode={}", lane.mode.keyword()));
        serialize_lane_lfo(&lane.lfo, out);
    }
    if lane.smoothing_ms != 0.0 {
        out.push_str(&format!(
            " smoothing={}",
            format_param_value(&ParamValue::Time(lane.smoothing_ms / 1000.0))
        ));
    }
    if lane.ppq != 0 {
        out.push_str(&format!(" [ppq={}]", lane.ppq));
    }
    out.push_str(" {");
    let points: Vec<String> = lane
        .points
        .iter()
        .map(|p| {
            let mut text = format!(
                "{}={}",
                format_bar_beat(&p.position),
                format_param_value(&p.value)
            );
            if p.curve != Curve::Linear {
                text.push_str(&format!(" curve={}", p.curve.keyword()));
            }
            if let Some(tension) = p.tension {
                text.push_str(&format!(" tension={}", format_number(tension)));
            }
            text
        })
        .collect();
    out.push_str(&points.join(", "));
    out.push_str("}\n");
}

/// Only non-default cycle controls are written, so a lane that never used them
/// keeps its old text byte for byte.
fn serialize_lane_lfo(lfo: &LaneLfo, out: &mut String) {
    match lfo.speed {
        LfoSpeed::Hz { hz } => {
            if hz != 1.0 {
                out.push_str(&format!(" speed={}Hz", format_number(hz)));
            }
        }
        LfoSpeed::Note {
            numerator,
            denominator,
        } => {
            out.push_str(&format!(" speed={numerator}/{denominator}"));
        }
    }
    if lfo.mult != 1.0 {
        out.push_str(&format!(" mult={}", format_number(lfo.mult)));
    }
    if lfo.pulse_width != 0.5 {
        out.push_str(&format!(" pw={}", format_number(lfo.pulse_width)));
    }
    if lfo.skew != 0.0 {
        out.push_str(&format!(" skew={}", format_number(lfo.skew)));
    }
    if lfo.tension != 0.0 {
        out.push_str(&format!(" tension={}", format_number(lfo.tension)));
    }
    if lfo.amount != 1.0 {
        out.push_str(&format!(" amount={}", format_number(lfo.amount)));
    }
}

fn serialize_effects(effects: &[Effect], out: &mut String) {
    out.push_str("  effects:\n");
    for effect in effects {
        out.push_str(&format!(
            "    - {}",
            format_effect_type(&effect.effect_type)
        ));
        let mut params = effect.params.clone();
        if effect.bypass {
            params.push(("bypass".to_string(), ParamValue::Bool(true)));
        }
        if let Some(ref sc) = effect.sidechain_source {
            params.push(("sidechain".to_string(), ParamValue::String(sc.clone())));
        }
        if !params.is_empty() {
            let param_list: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{}={}", k, format_param_value(v)))
                .collect();
            out.push_str(&format!(": {}", param_list.join(", ")));
        }
        out.push('\n');
    }
}

/// One `route:` line per device-input route. The audio kind is the default and
/// is left implicit, so a project without control routes writes nothing new.
fn serialize_input_routes(routes: &[InputRoute], out: &mut String) {
    for route in routes {
        let source = match route.source.as_str() {
            // A source whose name is also a kind keyword needs quotes, or the
            // parser reads it as the kind and loses the name.
            "audio" | "control" => format!("\"{}\"", route.source.replace('"', "'")),
            _ => format_name(&route.source),
        };
        let kind = match route.kind {
            RouteKind::Audio => String::new(),
            RouteKind::Control => "control ".to_string(),
        };
        out.push_str(&format!(
            "  route: {kind}{source} -> {}\n",
            format_name(&route.target)
        ));
    }
}

fn serialize_bus(bus: &Bus, out: &mut String) {
    out.push_str(&format!("@bus {}\n", format_name(&bus.name)));
    if !bus.routes.is_empty() {
        let routes: Vec<String> = bus.routes.iter().map(|r| format_name(r)).collect();
        out.push_str(&format!("  routes: [{}]\n", routes.join(", ")));
    }
    let mut mix = Vec::new();
    if bus.volume_db != 0.0 {
        mix.push(format!("vol={}dB", format_signed(bus.volume_db)));
    }
    if bus.mute {
        mix.push("mute=true".to_string());
    }
    if bus.solo {
        mix.push("solo=true".to_string());
    }
    if !mix.is_empty() {
        out.push_str(&format!("  mix: {}\n", mix.join(", ")));
    }
    if !bus.effects.is_empty() {
        serialize_effects(&bus.effects, out);
    }
    serialize_input_routes(&bus.input_routes, out);
    for lane in &bus.automation {
        serialize_automation(lane, out);
    }
    serialize_gain_automation(&bus.gain_automation, out);
}

fn serialize_gain_automation(clips: &[GainAutomation], out: &mut String) {
    for clip in clips {
        let mode = match clip.interpolation {
            GainInterpolation::Step => "step",
            GainInterpolation::Linear => "linear",
        };
        let points = clip
            .points
            .iter()
            .map(|p| {
                format!(
                    "{}={}",
                    format_bar_beat(&p.position),
                    format_param_value(&p.value)
                )
            })
            .collect::<Vec<_>>();
        out.push_str(&format!(
            "  gain_automation: {}{} [{} - {}] {{{}}}\n",
            mode,
            if clip.ppq == 4 {
                String::new()
            } else {
                format!(" ppq={}", clip.ppq)
            },
            format_bar_beat(&clip.time_range.start),
            format_bar_beat(&clip.time_range.end),
            points.join(", ")
        ));
    }
}

fn serialize_master(master: &Master, out: &mut String) {
    out.push_str("@master\n");
    if let Some(loudness) = master.loudness_target {
        out.push_str(&format!("  loudness: {}dB\n", format_signed(loudness)));
    }
    if master.volume_db != 0.0 {
        out.push_str(&format!(
            "  volume: {}dB\n",
            format_signed(master.volume_db)
        ));
    }
    if !master.effects.is_empty() {
        serialize_effects(&master.effects, out);
    }
    serialize_input_routes(&master.input_routes, out);
    for lane in &master.automation {
        serialize_automation(lane, out);
    }
    serialize_gain_automation(&master.gain_automation, out);
}

fn serialize_section_inner(section: &Section, out: &mut String) {
    out.push_str(&format!(
        "@section {} [{} - {}]\n",
        format_name(&section.name),
        format_bar_beat(&section.time_range.start),
        format_bar_beat(&section.time_range.end)
    ));
    if let Some(rep) = section.repeat {
        out.push_str(&format!("  repeat: {}\n", rep));
    }
    if let Some(label) = &section.label {
        out.push_str(&format!("  label: \"{}\"\n", label));
    }
    if let Some(flag) = &section.flags {
        out.push_str(&format!("  flags: {}\n", flag.as_token()));
    }
}

// ============================================================================
// Formatting helpers
// ============================================================================

/// Bare when the grammar accepts it, otherwise a quoted literal.
fn format_name(name: &str) -> String {
    let mut chars = name.chars();
    let bare = chars
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '/')
        .unwrap_or(false)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '/');
    if bare {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "'"))
    }
}

pub(crate) fn picture_clock_property(clock: &crate::timecode::PictureClock) -> String {
    format!(
        "picture_clock: {{rate: \"{}\", origin: \"{}\"}}",
        clock.rate.as_str(),
        escape_quotes(&clock.origin)
    )
}

fn escape_quotes(value: &str) -> String {
    value.replace('"', "'")
}

fn format_pitch(p: &Pitch) -> String {
    let note = match p.note {
        NoteName::C => "C",
        NoteName::D => "D",
        NoteName::E => "E",
        NoteName::F => "F",
        NoteName::G => "G",
        NoteName::A => "A",
        NoteName::B => "B",
    };
    let acc = match p.accidental {
        Accidental::Natural => "",
        Accidental::Sharp => "#",
        Accidental::DoubleSharp => "##",
        Accidental::Flat => "b",
        Accidental::DoubleFlat => "bb",
    };
    format!("{}{}{}", note, acc, p.octave)
}

fn format_duration(d: &Duration) -> String {
    let mut s = match d.base {
        BaseDuration::Whole => "w".to_string(),
        BaseDuration::Half => "h".to_string(),
        BaseDuration::Quarter => "q".to_string(),
        BaseDuration::Eighth => "8th".to_string(),
        BaseDuration::Sixteenth => "16th".to_string(),
        BaseDuration::ThirtySecond => "32nd".to_string(),
        BaseDuration::SixtyFourth => "64th".to_string(),
    };
    match d.dots {
        1 => s.push('.'),
        2 => s.push_str(".."),
        _ => {}
    }
    if d.triplet {
        s.push('t');
    }
    if let Some((n, m)) = d.tuplet {
        s.push_str(&format!("{}:{}", n, m));
    }
    for tie in &d.ties {
        s.push('~');
        s.push_str(match tie {
            BaseDuration::Whole => "w",
            BaseDuration::Half => "h",
            BaseDuration::Quarter => "q",
            BaseDuration::Eighth => "8th",
            BaseDuration::Sixteenth => "16th",
            BaseDuration::ThirtySecond => "32nd",
            BaseDuration::SixtyFourth => "64th",
        });
    }
    s
}

fn format_dynamics(d: &Dynamics) -> &'static str {
    match d {
        Dynamics::PPP => "ppp",
        Dynamics::PP => "pp",
        Dynamics::P => "p",
        Dynamics::MP => "mp",
        Dynamics::MF => "mf",
        Dynamics::F => "f",
        Dynamics::FF => "ff",
        Dynamics::FFF => "fff",
    }
}

fn format_articulation(a: &Articulation) -> &'static str {
    a.as_token()
}

fn format_drum_articulation(a: &DrumArticulation) -> &'static str {
    match a {
        DrumArticulation::Closed => "closed",
        DrumArticulation::Open => "open",
        DrumArticulation::HalfOpen => "half_open",
        DrumArticulation::GhostNote => "ghost_note",
        DrumArticulation::Flam => "flam",
        DrumArticulation::Roll => "roll",
        DrumArticulation::RimShot => "rim_shot",
        DrumArticulation::CrossStick => "cross_stick",
    }
}

fn format_effect_type(e: &EffectType) -> String {
    match e {
        EffectType::Plugin(id) => {
            let bare = !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            format!(
                "plugin:{}",
                if bare {
                    id.clone()
                } else {
                    serde_json::to_string(id).expect("string serialization")
                }
            )
        }
        _ => e.lane_name().into(),
    }
}

/// Canonical `bar:beat[:subdivision]` text, shared with validation messages.
pub(crate) fn format_bar_beat(bb: &BarBeat) -> String {
    if bb.subdivision == 0 {
        format!("{}:{}", bb.bar, bb.beat)
    } else {
        format!("{}:{}:{}", bb.bar, bb.beat, bb.subdivision)
    }
}

fn format_key(k: &KeySignature) -> String {
    let note = match k.root {
        NoteName::C => "C",
        NoteName::D => "D",
        NoteName::E => "E",
        NoteName::F => "F",
        NoteName::G => "G",
        NoteName::A => "A",
        NoteName::B => "B",
    };
    let acc = match k.accidental {
        Accidental::Natural => "",
        Accidental::Sharp => "#",
        Accidental::DoubleSharp => "##",
        Accidental::Flat => "b",
        Accidental::DoubleFlat => "bb",
    };
    let mode = match k.mode {
        KeyMode::Major => "major",
        KeyMode::Minor => "minor",
    };
    format!("{}{} {}", note, acc, mode)
}

fn format_param_value(v: &ParamValue) -> String {
    match v {
        ParamValue::Number(n) => format_number(*n),
        ParamValue::Db(n) => format!("{}dB", format_signed(*n)),
        ParamValue::Percentage(n) => format!("{}%", format_number(*n)),
        ParamValue::Frequency(n) => format!("{}Hz", format_number(*n)),
        ParamValue::Cents(n) => format!("{}ct", format_number(*n)),
        ParamValue::Time(n) => {
            if *n < 1.0 {
                format!("{}ms", format_number(*n * 1000.0))
            } else {
                format!("{}s", format_number(*n))
            }
        }
        ParamValue::String(s) => s.clone(),
        ParamValue::Bool(b) => b.to_string(),
        ParamValue::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|value| format_number(*value))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn format_map_value(value: &ParamValue) -> String {
    match value {
        ParamValue::String(text) => format!("\"{}\"", text),
        _ => format_param_value(value),
    }
}

fn format_number(n: f64) -> String {
    n.to_string()
}

fn format_number_float(n: f64) -> String {
    // Display uses the shortest decimal that roundtrips to the original f64.
    // Fixed two-decimal formatting moves fractional beat positions on save.
    n.to_string()
}

fn format_signed(n: f64) -> String {
    if n >= 0.0 {
        format_number(n)
    } else {
        format!("-{}", format_number(n.abs()))
    }
}

fn serialize_pattern_def(pattern: &PatternDef, out: &mut String) {
    out.push_str(&format!("@pattern {}", format_name(&pattern.name)));
    let mut inline_props = Vec::new();
    if let Some(preset) = &pattern.preset {
        inline_props.push(format!("preset: {}", format_name(preset)));
    }
    inline_props.push(format!("instrument: {}", pattern.instrument.as_path()));
    if let Some(ref color) = pattern.color {
        inline_props.push(format!("color: \"{}\"", color));
    }
    if let Some(t) = pattern.transpose {
        inline_props.push(format!("transpose: {}", t));
    }
    if let Some(s) = pattern.swing {
        inline_props.push(format!("swing: {}%", format_number(s)));
    }
    if let Some(bars) = pattern.bars {
        inline_props.push(format!("bars: {}", format_number(bars)));
    }
    if let Some(engine) = &pattern.engine {
        inline_props.push(format!("engine: {}", engine));
    }
    out.push_str(&format!(" | {}\n", inline_props.join(", ")));

    if !pattern.effects.is_empty() {
        serialize_effects(&pattern.effects, out);
    }

    if !pattern.events.is_empty() || !pattern.pedals.is_empty() || !pattern.hairpins.is_empty() {
        out.push_str("  events:\n");
        serialize_pedal_events(&pattern.pedals, out);
        serialize_pattern_hairpins(&pattern.hairpins, out);
        serialize_note_events(&pattern.events, out);
    }

    if !pattern.hits.is_empty() {
        out.push_str("  hits:\n");
        for hit in &pattern.hits {
            out.push_str(&format!(
                "    {} {}:{}",
                hit.drum, hit.position.beat, hit.position.subdivision
            ));
            if let Some(dyn_val) = &hit.dynamics {
                out.push_str(&format!(" {}", format_dynamics(dyn_val)));
            }
            if let Some(vel) = hit.velocity {
                out.push_str(&format!(" v={}", vel));
            }
            if let Some(art) = &hit.articulation {
                out.push_str(&format!(" articulation={}", format_drum_articulation(art)));
            }
            // A hit may carry an `articulation=` that names no known token; it
            // stays a generic modifier, so it must round-trip like any other.
            serialize_hit_modifiers(hit, out);
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn track_local_hit_roundtrip_retains_user_authored_typed_velocity_without_fallback() {
        let source = "@project\n  tempo: 120\n@track drums | instrument: drums/acoustic_kit\n  @pattern groove [1:1 - 2:1]\n    snare 1:0 mf v=37\n";
        let project = crate::parse(source).unwrap();
        let hit = &project.tracks[0].patterns[0].hits[0];
        assert_eq!(hit.velocity, Some(37));
        assert!(hit.modifiers.iter().all(|(key, _)| key != "velocity"));
        let serialized = super::serialize(&project);
        assert!(serialized.contains("v=37"), "{serialized}");
        let reparsed = crate::parse(&serialized).unwrap();
        assert_eq!(reparsed.tracks[0].patterns[0].hits[0], *hit);
    }

    use super::*;
    use crate::parser;

    const PITCHED_CLIP: &str = "@ocp \"0.2.0\"\n@project \"Pitch\"\n  tempo: 120\n@audio_clip hit [1:1 - 3:1] | track: tape\n  file: \"hit.wav\"\n  pitch: -7.5\n@track tape | instrument: audio/clip\n";

    #[test]
    fn audio_clip_pitch_roundtrips_and_zero_is_omitted() {
        let project = parser::parse(PITCHED_CLIP).unwrap();
        assert_eq!(project.audio_clip(0).unwrap().pitch, -7.5);
        let serialized = serialize(&project);
        assert!(serialized.contains("  pitch: -7.5"), "{serialized}");
        assert_eq!(parser::parse(&serialized).unwrap(), project);

        let signed = PITCHED_CLIP.replace("pitch: -7.5", "pitch: +5");
        let raised = parser::parse(&signed).unwrap();
        assert_eq!(raised.audio_clip(0).unwrap().pitch, 5.0);
        assert!(serialize(&raised).contains("  pitch: 5"), "{signed}");

        let mut unity = project;
        unity.audio_clip_mut(0).unwrap().pitch = 0.0;
        assert!(!serialize(&unity).contains("pitch:"));
    }

    #[test]
    fn audio_clip_polarity_and_crossfade_round_trip() {
        let source = "@project\n  tempo: 120\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n  invert: true\n  crossfade: 10ms\n@track tape | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        let clip = project.audio_clip(0).unwrap();
        assert!(clip.invert);
        assert_eq!(clip.crossfade_seconds, 0.010);
        let output = serialize(&project);
        assert!(output.contains("  invert: true"), "{output}");
        assert!(output.contains("  crossfade: 0.01s"), "{output}");
        assert_eq!(parser::parse(&output).unwrap(), project);

        let plain = parser::parse(
            "@project\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n@track tape | instrument: audio/clip\n",
        )
        .unwrap();
        let plain_output = serialize(&plain);
        assert!(!plain_output.contains("invert:"), "{plain_output}");
        assert!(!plain_output.contains("crossfade:"), "{plain_output}");
    }

    #[test]
    fn audio_clip_gain_envelope_round_trips_and_empty_is_omitted() {
        let source = "@project\n  tempo: 120\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n  gain_envelope: 0:0dB, 0.5:-6dB, 1:0dB\n@track tape | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        let clip = project.audio_clip(0).unwrap();
        assert_eq!(
            clip.gain_envelope,
            vec![
                GainEnvelopePoint {
                    position: 0.0,
                    gain_db: 0.0,
                },
                GainEnvelopePoint {
                    position: 0.5,
                    gain_db: -6.0,
                },
                GainEnvelopePoint {
                    position: 1.0,
                    gain_db: 0.0,
                },
            ]
        );
        let output = serialize(&project);
        assert!(
            output.contains("  gain_envelope: 0:0dB, 0.5:-6dB, 1:0dB"),
            "{output}"
        );
        assert_eq!(parser::parse(&output).unwrap(), project);

        let plain = parser::parse(
            "@project\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n@track tape | instrument: audio/clip\n",
        )
        .unwrap();
        assert!(plain.audio_clip(0).unwrap().gain_envelope.is_empty());
        assert!(!serialize(&plain).contains("gain_envelope"), "{plain:?}");
    }

    #[test]
    fn audio_clip_gain_envelope_accepts_signed_and_whole_positions() {
        let source = "@project\n  tempo: 120\n@audio_clip take [1:1 - 2:1] | track: tape\n  file: \"take.wav\"\n  gain_envelope: 0:+3dB, 1:-12.5dB\n@track tape | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        let clip = project.audio_clip(0).unwrap();
        assert_eq!(clip.gain_envelope.len(), 2);
        assert_eq!(clip.gain_envelope[0].gain_db, 3.0);
        assert_eq!(clip.gain_envelope[1].position, 1.0);
        assert_eq!(clip.gain_envelope[1].gain_db, -12.5);
        let output = serialize(&project);
        assert!(
            output.contains("  gain_envelope: 0:3dB, 1:-12.5dB"),
            "{output}"
        );
        assert_eq!(parser::parse(&output).unwrap(), project);
    }

    #[test]
    fn send_linear_gain_and_legacy_percent_are_exact() {
        let source = "@project\n  tempo: 120\n@track keys | instrument: piano/acoustic_grand\n  mix: send(space=25%)\n@bus space\n  routes: [master]\n";
        let mut project = parser::parse(source).unwrap();
        assert_eq!(project.tracks[0].mix.sends[0].amount, 0.25);
        assert!(serialize(&project).contains("send(space=25%)"));
        for value in [0.0, 1.0, 0.16326715976401, 0.1, 0.9999999999999999] {
            project.tracks[0].mix.sends[0].amount = value;
            let restored = parser::parse(&serialize(&project)).unwrap();
            assert_eq!(project, restored);
        }
        let linear = parser::parse(&source.replace("25%", "0.16326715976401")).unwrap();
        assert_eq!(linear.tracks[0].mix.sends[0].amount, 0.16326715976401);
        assert!(serialize(&linear).contains("send(space=0.16326715976401)"));
    }

    #[test]
    fn master_volume_roundtrips_and_unity_is_omitted() {
        let text = "@project\n@master\n  volume: -6dB\n  loudness: -14dB\n";
        let original = parser::parse(text).unwrap();
        let serialized = serialize(&original);
        assert!(serialized.contains("volume: -6dB"), "{serialized}");
        let reopened = parser::parse(&serialized).unwrap();
        assert_eq!(original, reopened);
        let mut unity = original;
        unity.master.as_mut().unwrap().volume_db = 0.0;
        assert!(!serialize(&unity).contains("volume:"));
    }

    #[test]
    fn array_param_values_roundtrip_through_text() {
        let text = "@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: { lfo.shape.x: [0, 0.5, 1], lfo.shape.y: [0.25, 1, 0.25], lfo.shape.curve: [0.5, 0.5, 0.5] }\n";
        let original = parser::parse(text).unwrap();
        assert_eq!(
            original.presets[0].params[0].1,
            crate::ast::ParamValue::Array(vec![0.0, 0.5, 1.0])
        );
        let serialized = serialize(&original);
        assert!(
            serialized.contains("lfo.shape.x: [0, 0.5, 1]"),
            "{serialized}"
        );
        let reopened = parser::parse(&serialized).unwrap();
        assert_eq!(original, reopened);
    }

    #[test]
    fn parameter_map_state_paths_roundtrip() {
        let text = "@project\n  tempo: 120\n@preset p\n  source: builtin\n  plugin: \"builtin.open_synth\"\n  params: {open_synth_state_file: \"states/Custom Voice.json\"}\n@track t | preset: p\n  engine_params: {open_synth_state_file: \"states/Alternate.json\"}\n";
        let original = parser::parse(text).unwrap();
        let reopened = parser::parse(&serialize(&original)).unwrap();
        assert_eq!(
            serde_json::to_value(original).unwrap(),
            serde_json::to_value(reopened).unwrap()
        );
    }

    #[test]
    fn full_featured_project_state_roundtrip() {
        let original =
            parser::parse(include_str!("../../../examples/demo_full_featured.ocp")).unwrap();
        crate::validate(&original).unwrap();
        let reopened = parser::parse(&serialize(&original)).unwrap();
        crate::validate(&reopened).unwrap();
        for (left, right) in original.tracks.iter().zip(&reopened.tracks) {
            for (a, b) in left.phrases.iter().zip(&right.phrases) {
                for (index, (x, y)) in a.events.iter().zip(&b.events).enumerate() {
                    assert_eq!(
                        serde_json::to_value(x).unwrap(),
                        serde_json::to_value(y).unwrap(),
                        "{} / {} / {}",
                        left.name,
                        a.name,
                        index
                    );
                }
            }
        }
        fn compare(a: &serde_json::Value, b: &serde_json::Value, path: String) {
            match (a, b) {
                (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
                    assert_eq!(x.len(), y.len(), "{path}");
                    for (key, value) in x {
                        compare(value, &y[key], format!("{path}/{key}"));
                    }
                }
                (serde_json::Value::Array(x), serde_json::Value::Array(y)) => {
                    assert_eq!(x.len(), y.len(), "{path}");
                    for (i, (x, y)) in x.iter().zip(y).enumerate() {
                        compare(x, y, format!("{path}/{i}"));
                    }
                }
                _ => assert_eq!(a, b, "{path}"),
            }
        }
        compare(
            &serde_json::to_value(original).unwrap(),
            &serde_json::to_value(reopened).unwrap(),
            String::new(),
        );
    }

    #[test]
    fn test_round_trip_minimal() {
        let source = r#"@project "Test"
  tempo: 120
  time_signature: 4/4
  key: C major

@track piano | instrument: piano/acoustic_grand
  @phrase intro [1:1 - 4:4]
    C4 q mf
    E4 q mf articulation=legato
    G4 h f
"#;
        let ast = parser::parse(source).unwrap();
        let output = serialize(&ast);
        // Parse the output again
        let ast2 = parser::parse(&output)
            .unwrap_or_else(|_| panic!("Failed to re-parse serialized output:\n{}", output));
        assert_eq!(ast.name, ast2.name);
        assert_eq!(ast.properties.tempo, ast2.properties.tempo);
        assert_eq!(ast.tracks.len(), ast2.tracks.len());
        assert_eq!(
            ast.tracks[0].phrases[0].events.len(),
            ast2.tracks[0].phrases[0].events.len()
        );
    }

    #[test]
    fn test_round_trip_chord_explicit_positions() {
        // Chords with @start_beat should survive round-trip
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase chord [1:1:0 - 1:4:0]\n    @0.0 C4 q mf\n    @0.0 E4 q mf\n    @0.0 G4 q mf\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        let ast2 = parser::parse(&output).expect("Failed to re-parse serialized output");
        let events = &ast2.tracks[0].phrases[0].events;
        assert_eq!(events.len(), 3, "should have 3 chord notes");
        for e in events {
            assert!(
                (e.start_beat - 0.0).abs() < 0.001,
                "chord notes should all be at 0.0"
            );
        }
    }

    #[test]
    fn test_round_trip_sequential_stays_sequential() {
        // Sequential notes should NOT get @beat prefix on round-trip
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase seq [1:1:0 - 2:4:0]\n    C4 q mf\n    D4 q mf\n    E4 h f\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        // Output should be sequential (no @beat prefixes)
        assert!(
            !output.contains("@0"),
            "sequential output should not have @beat prefix, got:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        assert_eq!(ast2.tracks[0].phrases[0].events.len(), 3);
    }

    #[test]
    fn pedal_marks_are_visible_and_round_trip() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase ped [1:1:0 - 3:1:0]\n    @0 pedal down\n    C4 h mf\n    @2 pedal up\n    E4 h mf\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        assert!(
            output.contains("    @0 pedal down\n") && output.contains("    @2 pedal up\n"),
            "the formatter dropped the pedal marks:\n{output}"
        );
        let again = parser::parse(&output).expect("Failed to re-parse serialized output");
        assert_eq!(
            again.tracks[0].phrases[0].pedals,
            ast.tracks[0].phrases[0].pedals
        );
        assert_eq!(serialize(&again), output, "the formatter is not idempotent");
    }

    #[test]
    fn a_pattern_keeps_its_pedal_marks_in_its_events_block() {
        let source = "@project\n@pattern riff | instrument: piano/acoustic_grand\n  events:\n    @0 pedal down\n    C4 q mf\n    @2 pedal up\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        assert!(
            output.contains("  events:\n    @0 pedal down\n"),
            "the pattern lost its pedal:\n{output}"
        );
        let again = parser::parse(&output).expect("Failed to re-parse");
        assert_eq!(again.patterns[0].pedals.len(), 2);
    }

    #[test]
    fn test_round_trip_engine_params() {
        // engine_params should survive round-trip
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  engine_params: {frequency: 440, amplitude: 0.3}\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        assert!(
            output.contains("engine_params"),
            "output should contain engine_params:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        let ep = ast2.tracks[0]
            .engine_params
            .as_ref()
            .expect("engine_params should survive round-trip");
        assert_eq!(ep.len(), 2);
    }

    #[test]
    fn plugin_identity_with_format_colon_or_path_round_trips_exactly() {
        for id in [
            "vst3.vst3:valhallasupermassive",
            "clap.Plugin With Spaces",
            "vst3.C:\\Plugins\\Voice\"A\".vst3",
        ] {
            let mut project =
                parser::parse("@project\n@track t | instrument: piano/acoustic_grand\n").unwrap();
            project.tracks[0].effects.push(Effect {
                effect_type: EffectType::Plugin(id.into()),
                params: vec![],
                bypass: false,
                sidechain_source: None,
            });
            let output = serialize(&project);
            let parsed = parser::parse(&output).unwrap();
            assert_eq!(
                parsed.tracks[0].effects[0].effect_type,
                EffectType::Plugin(id.into())
            );
        }
    }

    #[test]
    fn test_round_trip_plugin_effects() {
        // Plugin effects should survive round-trip
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - plugin:\"org.chowdsp.ChowMultiTool\"\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        assert!(
            output.contains("plugin:org.chowdsp.ChowMultiTool"),
            "output should contain plugin effect:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        let effects = &ast2.tracks[0].effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0].effect_type,
            crate::ast::EffectType::Plugin("org.chowdsp.ChowMultiTool".into())
        );
    }

    #[test]
    fn test_round_trip_track_with_instrument_and_preset() {
        // Track with both instrument AND preset should serialize both
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | preset: my_preset, instrument: piano/acoustic_grand\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        assert!(
            output.contains("preset: my_preset"),
            "output should contain preset:\n{}",
            output
        );
        assert!(
            output.contains("instrument: piano/acoustic_grand"),
            "output should contain instrument:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        let track = &ast2.tracks[0];
        assert_eq!(track.preset, Some("my_preset".into()));
        assert_eq!(track.instrument.as_path(), "piano/acoustic_grand");
    }

    #[test]
    fn test_round_trip_pattern_full_time_range() {
        // Pattern time_range should serialize both start and end
        let source = "@project\n  tempo: 128\n  time_signature: 4/4\n\n@track drums | instrument: drums/acoustic_kit\n  @pattern beat [1:1:0 - 4:4:0] x4\n    kick 1:0 ff\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let output = serialize(&ast);
        // Note: format_bar_beat drops ":0" when subdivision is 0
        assert!(
            output.contains("[1:1 - 4:4]"),
            "pattern time_range should have both start and end:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        let pattern = &ast2.tracks[0].patterns[0];
        assert!(pattern.time_range.is_some());
        let tr = pattern.time_range.unwrap();
        assert_eq!(tr.start.bar, 1);
        assert_eq!(tr.end.bar, 4);
    }

    #[test]
    fn test_round_trip_version_header() {
        // Version header should survive round-trip
        let source = "@ocp \"0.2.0\"\n\n@project \"Test\"\n  tempo: 120\n  time_signature: 4/4\n";
        let ast = parser::parse(source).expect("Failed to parse");
        assert_eq!(ast.ocp_version, Some("0.2.0".to_string()));
        let output = serialize(&ast);
        assert!(
            output.starts_with("@ocp \"0.2.0\""),
            "output should start with version header:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        assert_eq!(ast2.ocp_version, Some("0.2.0".to_string()));
    }

    #[test]
    fn test_round_trip_pan_fractional() {
        // Fractional pan should survive serialization (not truncated to i32)
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  mix: vol=-3dB, pan=0.75\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let pan_before = ast.tracks[0].mix.pan;
        assert!(
            (pan_before - 0.75).abs() < 0.01,
            "pan before should be ~0.75"
        );
        let output = serialize(&ast);
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        let pan_after = ast2.tracks[0].mix.pan;
        assert!(
            (pan_after - 0.75).abs() < 0.01,
            "pan should survive as ~0.75, got {}",
            pan_after
        );
    }

    #[test]
    fn explicit_track_output_roundtrips_and_legacy_defaults() {
        let source = "@project\n  tempo: 120\n@track keys | instrument: piano/acoustic_grand\n  mix: output=fx\n@bus fx\n  routes: [master]\n";
        let ast = parser::parse(source).unwrap();
        assert_eq!(ast.tracks[0].mix.output.as_deref(), Some("fx"));
        let restored = parser::parse(&serialize(&ast)).unwrap();
        assert_eq!(ast, restored);
        let mut value = serde_json::to_value(&ast).unwrap();
        value["tracks"][0]["mix"]
            .as_object_mut()
            .unwrap()
            .remove("output");
        let legacy: Project = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.tracks[0].mix.output, None);
    }

    #[test]
    fn test_round_trip_tempo_map() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n  tempo_map:\n    1:1 = 120 bpm\n    17:1 = 140 bpm\n";
        let ast = parser::parse(source).expect("Failed to parse");
        assert_eq!(ast.properties.tempo_map.len(), 2);
        assert_eq!(ast.properties.tempo_map[0].bpm, 120.0);
        assert_eq!(ast.properties.tempo_map[1].bpm, 140.0);
        let output = serialize(&ast);
        assert!(
            output.contains("tempo_map:"),
            "output should contain tempo_map:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        assert_eq!(ast2.properties.tempo_map.len(), 2);
    }

    #[test]
    fn test_round_trip_time_sig_map() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n  time_sig_map:\n    9:1 = 3/4\n    17:1 = 4/4\n";
        let ast = parser::parse(source).expect("Failed to parse");
        assert_eq!(ast.properties.time_sig_map.len(), 2);
        assert_eq!(ast.properties.time_sig_map[0].numerator, 3);
        let output = serialize(&ast);
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        assert_eq!(ast2.properties.time_sig_map.len(), 2);
    }

    #[test]
    fn test_round_trip_preset_extends() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@preset bright_pad\n  source: soundfont\n  program: 89\n  extends: warm_pad\n  effects:\n    - eq: high_shelf=+3dB@10kHz\n";
        let ast = parser::parse(source).expect("Failed to parse");
        assert_eq!(ast.presets[0].extends, Some("warm_pad".to_string()));
        let output = serialize(&ast);
        assert!(
            output.contains("extends: warm_pad"),
            "output should contain extends:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        assert_eq!(ast2.presets[0].extends, Some("warm_pad".to_string()));
    }

    #[test]
    fn test_round_trip_muted_note() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1 - 2:4]\n    C4 q mf\n    E4 q mf muted=true\n    F4 q mf\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let events = &ast.tracks[0].phrases[0].events;
        assert_eq!(events.len(), 3);
        assert!(!events[0].muted);
        assert!(events[1].muted);
        assert!(!events[2].muted);
        let output = serialize(&ast);
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        assert!(ast2.tracks[0].phrases[0].events[1].muted);
    }

    #[test]
    fn gain_ppq_roundtrip_and_cross_resolution_overlap() {
        let source = "@project\n@master\n  gain_automation: linear ppq=48 [1:1 - 1:1:24] {1:1:0=1, 1:1:3=0}\n";
        let ast = parser::parse(source).unwrap();
        assert!(crate::validator::validate(&ast).is_ok());
        assert_eq!(parser::parse(&serialize(&ast)).unwrap(), ast);
        let overlap = format!("{source}  gain_automation: step [1:1:1 - 1:2] {{1:1:1=1}}\n");
        assert!(crate::validator::validate(&parser::parse(&overlap).unwrap()).is_err());
        for resolution in ["0", "96001", "99999999999999999999999"] {
            assert!(
                crate::validator::validate(
                    &parser::parse(&source.replace("ppq=48", &format!("ppq={resolution}")))
                        .unwrap()
                )
                .is_err()
            );
        }
        let mut old =
            serde_json::to_value(&ast.master.as_ref().unwrap().gain_automation[0]).unwrap();
        old.as_object_mut().unwrap().remove("ppq");
        let restored: GainAutomation = serde_json::from_value(old).unwrap();
        assert_eq!(restored.ppq, 4);
    }

    #[test]
    fn gain_clips_roundtrip_validate_and_default_for_old_json() {
        let source = "@project\n  tempo: 120\n@bus group\n  routes: [master]\n  gain_automation: linear [1:1 - 2:1] {1:1=1, 1:4=0}\n@master\n  gain_automation: step [1:1 - 2:1] {1:1=0.5}\n";
        let ast = parser::parse(source).unwrap();
        assert!(crate::validator::validate(&ast).is_ok());
        assert_eq!(parser::parse(&serialize(&ast)).unwrap(), ast);
        let old_bus: Bus =
            serde_json::from_str(r#"{"name":"old","routes":[],"effects":[]}"#).unwrap();
        assert!(old_bus.gain_automation.is_empty());
        for (from, to) in [
            ("1:4=0", "1:4=99"),
            ("1:4=0", "1:1=0"),
            // A point on the range end is legal; one past it is not.
            ("1:4=0", "2:2=0"),
            ("1:4=0", "1:4=true"),
        ] {
            let bad = parser::parse(&source.replace(from, to)).unwrap();
            assert!(crate::validator::validate(&bad).is_err());
        }
        let mut bad = ast.clone();
        bad.properties.time_signature.numerator = 3;
        assert!(crate::validator::validate(&bad).is_err());
        let mut bad = ast.clone();
        let duplicate = bad.buses[0].gain_automation[0].clone();
        bad.buses[0].gain_automation.push(duplicate);
        assert!(crate::validator::validate(&bad).is_err());
    }

    #[test]
    fn names_with_spaces_round_trip() {
        let source = r#"@project "Spaces"
  tempo: 120
  time_signature: 4/4

@preset "Piano 1"
  source: builtin
  plugin: "builtin.open_synth"

@pattern "Beat 1" | instrument: drums/acoustic_kit
  hits:
    kick 1:0 mf

@track "Track 2" | instrument: piano/acoustic_grand, preset: "Piano 1"
  mix: send("Reverb Bus"=25%)
  @phrase "melody 2" [1:1 - 5:1]
    C4 q mf
  clip: "Beat 1" [1:1 - 3:1]

@bus "Reverb Bus"
  routes: ["Track 2"]

@section "Main A" [1:1 - 5:1]
  label: "A"
"#;
        let ast = parser::parse(source).expect("quoted names must parse");
        assert_eq!(ast.tracks[0].name, "Track 2");
        assert_eq!(ast.tracks[0].patterns, vec![]);
        assert_eq!(ast.tracks[0].phrases[0].name, "melody 2");
        assert_eq!(ast.tracks[0].pattern_clip(0).unwrap().pattern, "Beat 1");
        assert_eq!(ast.presets[0].name, "Piano 1");
        assert_eq!(ast.buses[0].name, "Reverb Bus");
        assert_eq!(ast.buses[0].routes, vec!["Track 2".to_string()]);
        assert_eq!(ast.sections[0].name, "Main A");
        assert_eq!(ast.tracks[0].mix.sends[0].target, "Reverb Bus");

        let output = serialize(&ast);
        assert!(
            output.contains("@track \"Track 2\""),
            "spaced names must be quoted:\n{output}"
        );
        let reopened = parser::parse(&output).expect("serialized names must re-parse");
        assert_eq!(reopened, ast);
    }

    #[test]
    fn test_round_trip_sidechain() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: sidechain=kick, ratio=4:1\n";
        let ast = parser::parse(source).expect("Failed to parse");
        let effect = &ast.tracks[0].effects[0];
        assert_eq!(effect.sidechain_source, Some("kick".to_string()));
        let output = serialize(&ast);
        assert!(
            output.contains("sidechain"),
            "output should contain sidechain:\n{}",
            output
        );
        let ast2 = parser::parse(&output).expect("Failed to re-parse");
        let effect2 = &ast2.tracks[0].effects[0];
        assert_eq!(effect2.sidechain_source, Some("kick".to_string()));
    }

    #[test]
    fn bypass_round_trips_on_a_builtin_device() {
        // `compression` has no bypass parameter; the field is the only control.
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: threshold=-20dB, bypass=true\n";
        let ast = parser::parse(source).expect("bypass parses on any device");
        let effect = &ast.tracks[0].effects[0];
        assert!(effect.bypass);
        assert!(
            effect.params.iter().all(|(name, _)| name != "bypass"),
            "bypass must not stay in the parameter list"
        );
        let output = serialize(&ast);
        assert_eq!(
            output, source,
            "bypass must keep its place among the inline attributes"
        );
        let reopened = parser::parse(&output).expect("serialized bypass re-parses");
        assert_eq!(ast, reopened);
        assert!(reopened.tracks[0].effects[0].bypass);
    }

    #[test]
    fn a_project_without_bypass_serialises_byte_identically() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - eq: low_freq=150Hz, low_gain=-2dB\n    - compression: ratio=4:1, threshold=-12dB\n    - reverb: decay=1.6s, mix=18%\n";
        let ast = parser::parse(source).expect("project parses");
        assert!(ast.tracks[0].effects.iter().all(|effect| !effect.bypass));
        let output = serialize(&ast);
        assert_eq!(
            output, source,
            "an unbypassed chain must write the same bytes as before"
        );
        assert!(!output.contains("bypass"), "{output}");
    }

    #[test]
    fn an_explicit_bypass_false_is_not_written() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - reverb: bypass=false, mix=18%\n";
        let ast = parser::parse(source).expect("bypass=false parses");
        assert!(!ast.tracks[0].effects[0].bypass);
        let output = serialize(&ast);
        assert!(!output.contains("bypass"), "{output}");
        assert!(output.contains("- reverb: mix=18%"), "{output}");
    }

    #[test]
    fn a_legacy_dynamic_eq_bypass_moves_to_the_effect_field() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - dynamic_eq: bypass=true, band1_enabled=true\n";
        let ast = parser::parse(source).expect("legacy dynamic_eq bypass parses");
        let effect = &ast.tracks[0].effects[0];
        assert!(
            effect.bypass,
            "the legacy parameter means the effect's own bypass"
        );
        assert!(
            effect.params.iter().all(|(name, _)| name != "bypass"),
            "one meaning: the parameter list keeps no bypass copy"
        );
        let output = serialize(&ast);
        assert!(
            output.contains("- dynamic_eq: band1_enabled=true, bypass=true"),
            "{output}"
        );
        let reopened = parser::parse(&output).expect("re-parse");
        assert_eq!(ast, reopened);
    }

    #[test]
    fn bypass_must_be_boolean() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - compression: bypass=1\n";
        let error = parser::parse(source).expect_err("a number is not a bypass");
        assert!(
            error.to_string().contains("bypass expects true or false"),
            "{error}"
        );
    }

    #[test]
    fn a_control_route_round_trips_byte_identically() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@control_source kick_env | from: track(kick), attack: 0ms, release: 100ms, lfo: none, formula: none\n\n@track kick | instrument: drums/kick\n\n@track bass | instrument: synth/bass\n  effects:\n    - compression: threshold=-24dB, ratio=4:1\n  route: control kick_env -> compression\n";
        let ast = parser::parse(source).expect("control route parses");
        let route = &ast.tracks[1].input_routes[0];
        assert_eq!(route.kind, crate::ast::RouteKind::Control);
        assert_eq!(route.source, "kick_env");
        assert_eq!(route.target, "compression");
        let output = serialize(&ast);
        assert_eq!(output, source, "control route text must be stable");
        let reopened = parser::parse(&output).expect("re-parse");
        assert_eq!(ast, reopened);
        assert_eq!(output, serialize(&reopened));
    }

    #[test]
    fn a_route_without_a_kind_is_an_audio_route_and_writes_no_word() {
        let source = "@project\n@track kick | instrument: drums/kick\n\n@track bass | instrument: synth/bass\n  effects:\n    - compression: ratio=4:1\n  route: kick -> compression\n";
        let ast = parser::parse(source).expect("audio route parses");
        let route = &ast.tracks[1].input_routes[0];
        assert_eq!(route.kind, crate::ast::RouteKind::Audio);
        assert_eq!(route.source, "kick");
        let output = serialize(&ast);
        assert!(
            output.contains("route: kick -> compression"),
            "the default kind stays implicit:\n{output}"
        );
        assert!(!output.contains("route: audio"), "{output}");
        assert_eq!(ast, parser::parse(&output).unwrap());
    }

    #[test]
    fn a_source_named_like_a_kind_stays_unambiguous() {
        let source = "@project\n@track audio | instrument: drums/kick\n@track control | instrument: drums/kick\n@track bass | instrument: synth/bass\n  route: \"audio\" -> compression\n";
        // `audio`/`control` sources need quotes when written before an effect
        // target; the serializer must not emit an ambiguous bare word.
        let ast = parser::parse(source).unwrap();
        assert_eq!(ast.tracks[2].input_routes[0].source, "audio");
        let output = serialize(&ast);
        assert!(
            output.contains("route: \"audio\" -> compression"),
            "a name that is also a kind keyword is quoted:\n{output}"
        );
        assert_eq!(ast, parser::parse(&output).unwrap(), "{output}");
    }

    const KIT_SOURCE: &str = "@project\n  tempo: 120\n  time_signature: 4/4\n\
@kit basic\n\
  max_voices: 32\n\
  pad kick | note: C2, gain: -2dB, pan: 0, tune: 0, cents: 0, decay: 0.4s, release: 0.02s, reverse: false, one_shot: true, choke: 1\n\
  pad snare | note: 38\n\
  pad hat_closed | note: 42, choke: 2\n\
  layer kick | low: 1, high: 127, sample: \"samples/kick.wav\", gain: 0dB\n\
  layer snare | low: 1, high: 63, sample: \"samples/snare_soft_a.wav\", gain: 0dB\n\
  layer snare | low: 1, high: 63, sample: \"samples/snare_soft_b.wav\", gain: 0dB\n\
  layer snare | low: 64, high: 127, sample: \"samples/snare_hard.wav\", gain: -1dB\n\
@track drums | instrument: drumkit, preset: basic\n\
  @pattern beat [1:1 - 2:1]\n\
    kick 1:0 mf\n\
    snare 2:0 mf\n";

    #[test]
    fn kit_block_lowers_to_a_builtin_preset_and_round_trips() {
        let project = parser::parse(KIT_SOURCE).expect("kit source must parse");
        assert_eq!(project.presets.len(), 1);
        let kit = &project.presets[0];
        assert_eq!(kit.source.as_deref(), Some("builtin"));
        assert_eq!(kit.plugin.as_deref(), Some(crate::DRUMKIT_PLUGIN_ID));
        assert_eq!(
            project.tracks[0].instrument,
            crate::ast::InstrumentPath::new("drums", "drumkit")
        );
        crate::validate(&project).expect("kit must validate");

        let text = serialize(&project);
        assert!(text.contains("@kit basic"), "{text}");
        assert!(text.contains("pad kick | note: 36"), "{text}");
        assert!(
            text.contains("layer snare | low: 1, high: 63, sample: \"samples/snare_soft_b.wav\""),
            "{text}"
        );
        let reopened = parser::parse(&text).expect("formatted kit must re-parse");
        assert_eq!(
            serde_json::to_value(&project).unwrap(),
            serde_json::to_value(&reopened).unwrap()
        );
        // JSON export carries the kit as a builtin preset with pad params.
        let json = serde_json::to_value(&project).unwrap();
        let params = json["presets"][0]["params"].as_array().unwrap();
        assert!(params.iter().any(|entry| entry[0] == "pad.kick.note"));
        assert!(
            params
                .iter()
                .any(|entry| entry[0] == "layer.snare.2.sample")
        );
    }

    #[test]
    fn kit_layer_for_an_undeclared_pad_is_a_validation_error() {
        let source = "@project\n@kit broken\n  pad kick | note: 36\n  layer ghost | low: 1, high: 127, sample: \"ghost.wav\"\n";
        let project = parser::parse(source).unwrap();
        assert!(crate::validate(&project).is_err());
    }

    #[test]
    fn kit_inverted_velocity_range_is_a_validation_error() {
        let source = "@project\n@kit broken\n  pad snare | note: 38\n  layer snare | low: 90, high: 20, sample: \"snare.wav\"\n";
        let project = parser::parse(source).unwrap();
        assert!(crate::validate(&project).is_err());
    }

    fn vocal_tune_project() -> &'static str {
        r#"@project "Vocal tune"
  tempo: 120
  key: C major
@audio_clip lead [1:1 - 2:1] | track: vocal
  file: "lead.wav"
@audio_clip guide [1:1 - 2:1] | track: guide
  file: "guide.wav"
@vocal_tune lead_tune | clip: lead
  scale: A major
  snap: reference
  reference: guide
  cap: 60ct
  window: 40ct
  amount: 60%
  retune: 25ms
  formant: -150ct
  vibrato: scale
  notes:
    - at=0.5s tgt=A4 amount=100% retune=10ms formant=-1ct vibrato=flatten
    - at=1.25s tgt=+2st amount=40% bypass=true
    - at=2s tgt=5700ct vibrato=preserve
    - at=3s tgt=+25ct
    - at=4s tgt=5@4
@track vocal | instrument: audio/clip
@track guide | instrument: audio/clip
"#
    }

    #[test]
    fn vocal_tune_parses_every_target_kind() {
        let project = parser::parse(vocal_tune_project()).unwrap();
        crate::validate(&project).unwrap();
        let tune = &project.properties.vocal_tunes[0];
        assert_eq!(tune.name, "lead_tune");
        assert_eq!(tune.clip.as_deref(), Some("lead"));
        assert!(tune.track.is_none());
        let scale = tune.scale.expect("scale");
        assert_eq!(scale.root, NoteName::A);
        assert_eq!(scale.mode, KeyMode::Major);
        assert_eq!(tune.snap, VocalSnapPolicy::Reference);
        assert_eq!(tune.reference.as_deref(), Some("guide"));
        assert_eq!(tune.snap_cap_cents, 60.0);
        assert_eq!(tune.snap_window_cents, 40.0);
        assert_eq!(tune.amount, 60.0);
        assert_eq!(tune.retune_seconds, 0.025);
        assert_eq!(tune.formant_cents, -150.0);
        assert_eq!(tune.vibrato, VocalVibratoMode::Scale);
        assert_eq!(tune.notes[0].at, 0.5);
        assert_eq!(tune.notes[0].target, Some(VocalTarget::Pitch(69)));
        assert_eq!(tune.notes[0].amount, Some(100.0));
        assert_eq!(tune.notes[0].retune_seconds, Some(0.01));
        assert_eq!(tune.notes[0].formant_cents, Some(-1.0));
        assert_eq!(tune.notes[0].vibrato, Some(VocalVibratoMode::Flatten));
        assert_eq!(tune.notes[1].target, Some(VocalTarget::Semitones(2.0)));
        assert!(tune.notes[1].bypass);
        assert_eq!(tune.notes[2].target, Some(VocalTarget::Cents(5_700.0)));
        assert_eq!(tune.notes[2].vibrato, Some(VocalVibratoMode::Preserve));
        assert_eq!(tune.notes[3].target, Some(VocalTarget::OffsetCents(25.0)));
        assert_eq!(
            tune.notes[4].target,
            Some(VocalTarget::Degree {
                degree: 5,
                octave: 4
            })
        );
    }

    #[test]
    fn vocal_tune_round_trips_through_text() {
        let project = parser::parse(vocal_tune_project()).unwrap();
        let text = serialize(&project);
        let reopened = parser::parse(&text).unwrap();
        assert_eq!(project, reopened);
        assert_eq!(text, serialize(&reopened), "fmt is not idempotent");
        assert!(
            text.contains("@vocal_tune lead_tune | clip: lead"),
            "{text}"
        );
        assert!(text.contains("  scale: A major\n"), "{text}");
        assert!(text.contains("  snap: reference\n"), "{text}");
        assert!(text.contains("  reference: guide\n"), "{text}");
        assert!(text.contains("  cap: 60ct\n"), "{text}");
        assert!(text.contains("  window: 40ct\n"), "{text}");
        assert!(text.contains("  amount: 60%\n"), "{text}");
        assert!(text.contains("  formant: -150ct\n"), "{text}");
        assert!(text.contains("  vibrato: scale\n"), "{text}");
        assert!(text.contains("tgt=+25ct"), "{text}");
        assert!(text.contains("tgt=5@4"), "{text}");
        assert!(text.contains("vibrato=flatten"), "{text}");
        assert!(text.contains("vibrato=preserve"), "{text}");
        assert!(text.contains("bypass=true"), "{text}");
    }

    #[test]
    fn vocal_tune_timing_fields_round_trip() {
        let source = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  amount: 100%\n  max_move: 150ms\n  notes:\n    - at=0.5s start=-30ms length=1.08\n    - at=1.5s start=12ms tgt=+25ct\n@track vocal | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        crate::validate(&project).unwrap();
        let tune = &project.properties.vocal_tunes[0];
        assert_eq!(tune.max_move_seconds, 0.150);
        assert_eq!(tune.notes[0].start_seconds, Some(-0.030));
        assert_eq!(tune.notes[0].length, Some(1.08));
        assert_eq!(tune.notes[1].start_seconds, Some(0.012));
        assert_eq!(tune.notes[1].length, None);
        let text = serialize(&project);
        let reopened = parser::parse(&text).unwrap();
        assert_eq!(project, reopened);
        assert_eq!(text, serialize(&reopened), "fmt is not idempotent");
        assert!(text.contains("  max_move: 150ms\n"), "{text}");
        assert!(text.contains("start=-30ms length=1.08"), "{text}");
        assert!(text.contains("start=12ms"), "{text}");
        // The default cap and missing fields are not written.
        let plain = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  amount: 100%\n@track vocal | instrument: audio/clip\n";
        let plain = serialize(&parser::parse(plain).unwrap());
        assert!(!plain.contains("max_move"), "{plain}");
        assert!(!plain.contains("start="), "{plain}");
        assert!(!plain.contains("length="), "{plain}");
    }

    #[test]
    fn vocal_tune_can_select_a_single_clip_track() {
        let source = "@project\n  tempo: 120\n  key: A major\n@audio_clip take [1:1 - 2:1] | track: lead\n  file: \"take.wav\"\n@vocal_tune tune | track: lead\n  notes:\n    - at=0s tgt=1@4\n@track lead | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        crate::validate(&project).unwrap();
        let tune = &project.properties.vocal_tunes[0];
        assert_eq!(tune.track.as_deref(), Some("lead"));
        // `amount` defaults to 100% so a targeted note tunes fully.
        assert_eq!(tune.amount, 100.0);
        assert_eq!(tune.retune_seconds, 0.025);
        assert_eq!(tune.vibrato, VocalVibratoMode::Preserve);
        assert_eq!(tune.notes[0].at, 0.0);
        let reopened = parser::parse(&serialize(&project)).unwrap();
        assert_eq!(project, reopened);
    }

    #[test]
    fn vocal_snap_defaults_stay_out_of_the_text() {
        let source = "@project\n  tempo: 120\n  key: C major\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  amount: 0%\n@track vocal | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        let tune = &project.properties.vocal_tunes[0];
        assert_eq!(tune.snap, VocalSnapPolicy::Auto);
        assert_eq!(tune.snap.resolved(false), VocalSnapPolicy::Scale);
        assert_eq!(tune.snap.resolved(true), VocalSnapPolicy::Reference);
        assert!(tune.reference.is_none());
        assert_eq!(tune.snap_cap_cents, 100.0);
        assert_eq!(tune.snap_window_cents, 35.0);
        let text = serialize(&project);
        for absent in ["snap:", "reference:", "cap:", "window:"] {
            assert!(!text.contains(absent), "{absent} in {text}");
        }
        assert_eq!(parser::parse(&text).unwrap(), project);

        // A non-default policy round-trips and is written out.
        let off = parser::parse(&source.replace(
            "@vocal_tune t | clip: lead\n",
            "@vocal_tune t | clip: lead\n  snap: off\n",
        ))
        .unwrap();
        assert_eq!(off.properties.vocal_tunes[0].snap, VocalSnapPolicy::Off);
        let text = serialize(&off);
        assert!(text.contains("  snap: off\n"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), off);
    }

    #[test]
    fn vocal_tune_unknown_fields_name_the_accepted_set() {
        let field = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  swing: 50%\n@track vocal | instrument: audio/clip\n";
        let error = parser::parse(field).unwrap_err().to_string();
        assert!(
            error.contains("unknown @vocal_tune field 'swing'"),
            "{error}"
        );
        assert!(
            error.contains(
                "scale, snap, reference, cap, window, max_move, amount, retune, formant, vibrato or notes"
            ),
            "{error}"
        );

        let note = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  notes:\n    - at=0s tgt=A3 nudge=+12ms\n@track vocal | instrument: audio/clip\n";
        let error = parser::parse(note).unwrap_err().to_string();
        assert!(error.contains("unknown note field 'nudge'"), "{error}");
        assert!(
            error.contains("at, tgt, amount, retune, formant, start, length, vibrato or bypass"),
            "{error}"
        );

        let value = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  notes:\n    - at=0s tgt=A9\n@track vocal | instrument: audio/clip\n";
        let error = parser::parse(value).unwrap_err().to_string();
        assert!(error.contains("invalid note field 'tgt=A9'"), "{error}");
        assert!(error.contains("a pitch like A3"), "{error}");

        let vibrato = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_tune t | clip: lead\n  notes:\n    - at=0s vibrato=wobble\n@track vocal | instrument: audio/clip\n";
        let error = parser::parse(vibrato).unwrap_err().to_string();
        assert!(
            error.contains("invalid note field 'vibrato=wobble'"),
            "{error}"
        );
        assert!(error.contains("preserve, flatten or scale"), "{error}");
    }

    fn validation_text(project: &crate::ast::Project) -> String {
        crate::validate(project)
            .unwrap_err()
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn vocal_harmony_project() -> &'static str {
        r#"@project "Vocal harmony"
  tempo: 120
  key: C major
@audio_clip lead [1:1 - 2:1] | track: vocal
  file: "lead.wav"
@vocal_tune lead_tune | clip: lead
  scale: C major
@vocal_harmony third | clip: lead
  interval: 2
  level: -6dB
  pan: -0.3
@track vocal | instrument: audio/clip
"#
    }

    #[test]
    fn vocal_harmony_parses_and_round_trips() {
        let project = parser::parse(vocal_harmony_project()).unwrap();
        crate::validate(&project).unwrap();
        let harmony = &project.properties.vocal_harmonies[0];
        assert_eq!(harmony.name, "third");
        assert_eq!(harmony.clip.as_deref(), Some("lead"));
        assert!(harmony.track.is_none());
        assert_eq!(harmony.interval, 2);
        assert_eq!(harmony.level_db, -6.0);
        assert_eq!(harmony.pan, -0.3);
        let text = serialize(&project);
        let reopened = parser::parse(&text).unwrap();
        assert_eq!(project, reopened);
        assert_eq!(text, serialize(&reopened), "fmt is not idempotent");
        assert!(
            text.contains("@vocal_harmony third | clip: lead\n"),
            "{text}"
        );
        assert!(text.contains("  interval: 2\n"), "{text}");
        assert!(text.contains("  level: -6dB\n"), "{text}");
        assert!(text.contains("  pan: -0.3\n"), "{text}");
    }

    #[test]
    fn vocal_harmony_defaults_stay_out_of_the_text() {
        let source = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_harmony h | clip: lead\n@track vocal | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        crate::validate(&project).unwrap();
        let harmony = &project.properties.vocal_harmonies[0];
        assert_eq!(harmony.interval, 2, "a third is the default interval");
        assert_eq!(harmony.level_db, -6.0);
        assert_eq!(harmony.pan, 0.0);
        let text = serialize(&project);
        // The level is always written; the neutral pan is not.
        assert!(!text.contains("pan:"), "{text}");
        assert!(text.contains("  interval: 2\n"), "{text}");
        assert!(text.contains("  level: -6dB\n"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), project);
    }

    #[test]
    fn vocal_harmony_unknown_fields_name_the_accepted_set() {
        let field = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_harmony h | clip: lead\n  formant: -150ct\n@track vocal | instrument: audio/clip\n";
        let error = parser::parse(field).unwrap_err().to_string();
        assert!(
            error.contains("unknown @vocal_harmony field 'formant'"),
            "{error}"
        );
        assert!(error.contains("interval, level or pan"), "{error}");
    }

    #[test]
    fn vocal_harmony_needs_a_real_clip() {
        let missing = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_harmony h | clip: ghost\n@track vocal | instrument: audio/clip\n";
        let text = validation_text(&parser::parse(missing).unwrap());
        assert!(
            text.contains("Unknown audio clip reference: 'ghost' (in vocal harmony 'h')"),
            "{text}"
        );
    }

    #[test]
    fn vocal_harmony_interval_is_bounded_and_not_unison() {
        for interval in ["13", "-13", "0"] {
            let source = format!(
                "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_harmony h | clip: lead\n  interval: {interval}\n@track vocal | instrument: audio/clip\n"
            );
            let text = validation_text(&parser::parse(&source).unwrap());
            assert!(text.contains("vocal harmony 'h'.interval"), "{text}");
        }
        let wide = "@project\n  tempo: 120\n@audio_clip lead [1:1 - 2:1] | track: vocal\n  file: \"lead.wav\"\n@vocal_harmony h | clip: lead\n  interval: -12\n@track vocal | instrument: audio/clip\n";
        crate::validate(&parser::parse(wide).unwrap()).unwrap();
    }

    #[test]
    fn vocal_harmony_can_select_a_single_clip_track_and_is_unique_per_clip() {
        let source = "@project\n  tempo: 120\n  key: A major\n@audio_clip take [1:1 - 2:1] | track: lead\n  file: \"take.wav\"\n@vocal_harmony h | track: lead\n@track lead | instrument: audio/clip\n";
        let project = parser::parse(source).unwrap();
        crate::validate(&project).unwrap();
        let harmony = &project.properties.vocal_harmonies[0];
        assert_eq!(harmony.track.as_deref(), Some("lead"));
        let reopened = parser::parse(&serialize(&project)).unwrap();
        assert_eq!(project, reopened);

        let twice = source.replace(
            "@track lead",
            "@vocal_harmony second | clip: take\n@track lead",
        );
        let text = validation_text(&parser::parse(&twice).unwrap());
        assert!(text.contains("already has a vocal harmony"), "{text}");
    }

    #[test]
    fn v02_projects_without_the_block_round_trip_unchanged() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut paths = std::fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("ocp"))
            .collect::<Vec<_>>();
        paths.sort();
        assert!(paths.len() >= 10, "expected the shipped examples");
        for path in paths {
            let source = std::fs::read_to_string(&path).unwrap();
            let project = parser::parse(&source)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert!(
                project.properties.vocal_tunes.is_empty(),
                "{} gained a vocal tune",
                path.display()
            );
            assert!(
                project.properties.vocal_harmonies.is_empty(),
                "{} gained a vocal harmony",
                path.display()
            );
            let text = serialize(&project);
            assert!(
                !text.contains("@vocal_tune"),
                "{} formatted into a vocal tune",
                path.display()
            );
            assert!(
                !text.contains("@vocal_harmony"),
                "{} formatted into a vocal harmony",
                path.display()
            );
            let reopened = parser::parse(&text).unwrap();
            assert_eq!(project, reopened, "{}", path.display());
        }
    }

    #[test]
    fn export_settings_roundtrip_and_defaults_are_omitted() {
        let source = "@project \"exports\"\n  tempo: 120\n  time_signature: 4/4\n  export: {container: wav, wav: {depth: i24, dither: true}}\n";
        let project = parser::parse(source).unwrap();
        let export = project.properties.export.expect("export settings parsed");
        assert_eq!(export.container, ExportContainer::Wav);
        assert_eq!(export.wav.depth, SampleFormat::I24);
        assert!(export.wav.dither);
        let text = serialize(&project);
        assert!(
            text.contains("export: {container: wav, wav: {depth: i24, dither: true}}"),
            "{text}"
        );
        assert_eq!(parser::parse(&text).unwrap(), project);

        let plain = parser::parse("@project \"plain\"\n  tempo: 120\n").unwrap();
        assert!(plain.properties.export.is_none());
        assert!(!serialize(&plain).contains("export:"));
    }

    #[test]
    fn flac_export_settings_roundtrip_with_depth_and_level() {
        let source = "@project \"flac\"\n  tempo: 120\n  export: {container: flac, wav: {depth: i16, dither: false}, flac: {depth: i24, level: 8}}\n";
        let project = parser::parse(source).unwrap();
        let export = project.properties.export.expect("export settings parsed");
        assert_eq!(export.container, ExportContainer::Flac);
        assert_eq!(export.flac.depth, SampleFormat::I24);
        assert_eq!(export.flac.level, 8);
        let text = serialize(&project);
        assert!(text.contains("container: flac"), "{text}");
        assert!(text.contains("flac: {depth: i24, level: 8}"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), project);

        // A default FLAC block is omitted, so WAV projects keep their spelling.
        let wav = parser::parse("@project\n  export: {container: wav}\n").unwrap();
        assert!(!serialize(&wav).contains("flac:"));
    }

    #[test]
    fn mp3_export_settings_roundtrip_with_bitrate_and_vbr_quality() {
        let cbr =
            "@project \"mp3\"\n  tempo: 120\n  export: {container: mp3, mp3: {bitrate: 256}}\n";
        let project = parser::parse(cbr).unwrap();
        let export = project.properties.export.expect("export settings parsed");
        assert_eq!(export.container, ExportContainer::Mp3);
        assert_eq!(export.mp3.bitrate, 256);
        assert_eq!(export.mp3.vbr_quality, None);
        let text = serialize(&project);
        assert!(text.contains("container: mp3"), "{text}");
        assert!(text.contains("mp3: {bitrate: 256}"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), project);

        let vbr = "@project\n  export: {container: mp3, mp3: {bitrate: 192, vbr_quality: 2}}\n";
        let project = parser::parse(vbr).unwrap();
        let export = project.properties.export.expect("export settings parsed");
        assert_eq!(export.mp3.bitrate, 192);
        assert_eq!(export.mp3.vbr_quality, Some(2));
        let text = serialize(&project);
        assert!(
            text.contains("mp3: {bitrate: 192, vbr_quality: 2}"),
            "{text}"
        );
        assert_eq!(parser::parse(&text).unwrap(), project);

        // A default MP3 block never appears, so WAV and FLAC projects keep their
        // historical spelling.
        let wav = parser::parse("@project\n  export: {container: wav}\n").unwrap();
        assert!(!serialize(&wav).contains("mp3:"));
        let defaults = parser::parse("@project\n  export: {container: mp3}\n").unwrap();
        let export = defaults.properties.export.unwrap();
        assert_eq!(export.mp3.bitrate, DEFAULT_MP3_BITRATE_KBPS);
        assert_eq!(export.mp3.vbr_quality, None);
        assert!(!serialize(&defaults).contains("mp3:"));
    }

    #[test]
    fn export_sample_rate_roundtrips_and_is_omitted_when_unset() {
        let source = "@project \"rate\"\n  tempo: 120\n  export: {container: wav, wav: {depth: i16, dither: false}, sample_rate: 48000}\n";
        let project = parser::parse(source).unwrap();
        let export = project.properties.export.expect("export settings parsed");
        assert_eq!(export.sample_rate, Some(48_000));
        let text = serialize(&project);
        assert!(text.contains("sample_rate: 48000"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), project);

        // A project that declares no rate keeps the caller's choice, and
        // serialisation does not invent one.
        let plain = parser::parse("@project \"plain\"\n  export: {container: wav}\n").unwrap();
        assert_eq!(plain.properties.export.unwrap().sample_rate, None);
        assert!(!serialize(&plain).contains("sample_rate"));
    }

    #[test]
    fn the_flat_export_block_still_parses_and_serialises_as_the_container_form() {
        let source = "@project \"legacy\"\n  export: {format: i24, dither: true}\n";
        let project = parser::parse(source).unwrap();
        let export = project.properties.export.expect("legacy export parsed");
        assert_eq!(export.container, ExportContainer::Wav);
        assert_eq!(export.wav.depth, SampleFormat::I24);
        assert!(export.wav.dither);
        let text = serialize(&project);
        assert!(
            text.contains("export: {container: wav, wav: {depth: i24, dither: true}}"),
            "{text}"
        );
        assert_eq!(parser::parse(&text).unwrap(), project);

        // Each flat key also stands alone, as before.
        let only_format = parser::parse("@project\n  export: {format: f32}\n").unwrap();
        let export = only_format.properties.export.unwrap();
        assert_eq!(export.wav.depth, SampleFormat::F32);
        assert!(!export.wav.dither);
        let only_dither = parser::parse("@project\n  export: {dither: true}\n").unwrap();
        let export = only_dither.properties.export.unwrap();
        assert_eq!(export.wav.depth, SampleFormat::I16);
        assert!(export.wav.dither);
    }

    #[test]
    fn export_settings_default_to_sixteen_bit_without_dither() {
        let project = parser::parse("@project\n  export: {}\n").unwrap();
        let export = project.properties.export.expect("empty block is explicit");
        assert_eq!(export.container, ExportContainer::Wav);
        assert_eq!(export.wav.depth, SampleFormat::I16);
        assert!(!export.wav.dither);
    }

    #[test]
    fn solo_mode_roundtrips_and_the_default_is_omitted() {
        let source = "@project \"solo\"\n  tempo: 120\n  solo_mode: upstream\n";
        let project = parser::parse(source).unwrap();
        assert_eq!(project.properties.solo_mode, SoloMode::Upstream);
        let text = serialize(&project);
        assert!(text.contains("solo_mode: upstream"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), project);

        let plain = parser::parse("@project \"plain\"\n  tempo: 120\n").unwrap();
        assert_eq!(plain.properties.solo_mode, SoloMode::Both);
        assert!(!serialize(&plain).contains("solo_mode:"));
        assert_eq!(
            parser::parse(&serialize(&plain))
                .unwrap()
                .properties
                .solo_mode,
            SoloMode::Both
        );
    }

    #[test]
    fn resample_steps_roundtrip_for_render_and_live() {
        let source = "@project\n  export: {resample: 6}\n  live_resample: 3\n";
        let project = parser::parse(source).unwrap();
        assert_eq!(project.properties.export.unwrap().resample, Some(6));
        assert_eq!(project.properties.live_resample, Some(3));
        let text = serialize(&project);
        assert!(
            text.contains(
                "export: {container: wav, wav: {depth: i16, dither: false}, resample: 6}"
            )
        );
        assert!(text.contains("live_resample: 3"));
        assert_eq!(parser::parse(&text).unwrap(), project);
        let plain = parser::parse("@project\n").unwrap();
        assert!(plain.properties.live_resample.is_none());
        assert!(!serialize(&plain).contains("live_resample"));
    }

    #[test]
    fn automation_normalization_version_roundtrips_and_defaults_to_legacy() {
        let source = "@project \"Wide\"\n  automation_normalization: 2\n";
        let project = parser::parse(source).unwrap();
        assert_eq!(project.properties.automation_normalization, Some(2));
        assert!(project.properties.uses_wide_automation_normalization());
        let text = serialize(&project);
        assert!(text.contains("automation_normalization: 2"));
        assert_eq!(parser::parse(&text).unwrap(), project);

        let plain = parser::parse("@project\n").unwrap();
        assert_eq!(plain.properties.automation_normalization, None);
        assert_eq!(plain.properties.automation_normalization_version(), 1);
        assert!(!plain.properties.uses_wide_automation_normalization());
        assert!(!serialize(&plain).contains("automation_normalization"));
    }

    // ---- Chord tokens ([G4 A4] q mf) and comma lists ----

    const CHORD_BODY: &str = "    [G4 A4] q mf\n    [C4 E4 G4] q. f articulation=legato\n    rest q\n    [B4 C5] h mp v=100\n";

    #[test]
    fn chord_tokens_serialize_as_the_canonical_comma_form() {
        let source = format!(
            "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n{CHORD_BODY}"
        );
        let ast = parser::parse(&source).unwrap();
        let text = serialize(&ast);
        assert!(text.contains("    G4 q mf, A4 q mf\n"), "{text}");
        assert!(
            text.contains(
                "    C4 q. f articulation=legato, E4 q. f articulation=legato, G4 q. f articulation=legato\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("    B4 h mp v=100, C5 h mp v=100\n"),
            "{text}"
        );
        assert!(
            !text.lines().any(|line| line.trim_start().starts_with('[')),
            "{text}"
        );
        let reparsed = parser::parse(&text).unwrap();
        assert_eq!(reparsed, ast);
        assert_eq!(serialize(&reparsed), text);
    }

    #[test]
    fn chord_at_an_explicit_beat_keeps_the_beat_prefix() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @2 [C4 E4] q mf\n    D4 q mf\n";
        let ast = parser::parse(source).unwrap();
        let text = serialize(&ast);
        assert!(text.contains("    @2 C4 q mf, E4 q mf\n"), "{text}");
        assert!(text.contains("    D4 q mf\n"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), ast);
    }

    #[test]
    fn legacy_simultaneous_notes_serialize_as_the_canonical_comma_form() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @0.0 C4 q mf\n    @0.0 E4 q mf\n    @0.0 G4 q mf\n    D4 q mf\n";
        let ast = parser::parse(source).unwrap();
        let text = serialize(&ast);
        assert!(
            text.contains("    @0 C4 q mf, E4 q mf, G4 q mf\n"),
            "{text}"
        );
        assert!(text.contains("    D4 q mf\n"), "{text}");
        let reparsed = parser::parse(&text).unwrap();
        let starts: Vec<_> = reparsed.tracks[0].phrases[0]
            .events
            .iter()
            .map(|event| event.start_beat)
            .collect();
        assert_eq!(starts, vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(serialize(&reparsed), text);
    }

    #[test]
    fn simultaneous_notes_with_different_dynamics_stay_separate() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @0.0 C4 q mf\n    @0.0 E4 q f\n";
        let ast = parser::parse(source).unwrap();
        let text = serialize(&ast);
        assert!(text.contains("    @0 C4 q mf\n"), "{text}");
        assert!(text.contains("    @0 E4 q f\n"), "{text}");
        assert!(!text.contains("C4 q mf, E4"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), ast);
    }

    #[test]
    fn simultaneous_duplicate_pitches_never_serialize_as_one_comma_list() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @0.0 C4 q mf\n    @0.0 C4 q mf\n    @0.0 E4 q mf\n";
        let ast = parser::parse(source).unwrap();
        let text = serialize(&ast);
        assert!(!text.contains("C4 q mf, C4"), "{text}");
        let reparsed = parser::parse(&text).unwrap();
        assert_eq!(reparsed, ast);
        assert_eq!(serialize(&reparsed), text);
    }

    #[test]
    fn comma_list_stays_a_comma_list() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @0.0 G4 q mf, G5 q mf\n    A4 q mf, B4 q mf\n";
        let ast = parser::parse(source).unwrap();
        let text = serialize(&ast);
        assert!(text.contains("    @0 G4 q mf, G5 q mf\n"), "{text}");
        assert!(text.contains("    A4 q mf, B4 q mf\n"), "{text}");
        assert_eq!(parser::parse(&text).unwrap(), ast);
        assert_eq!(serialize(&parser::parse(&text).unwrap()), text);
    }

    #[test]
    fn comma_members_that_differ_beyond_pitch_become_separate_lines() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @0.0 G4 q mf, G5 h mp articulation=legato\n";
        let ast = parser::parse(source).unwrap();
        let text = serialize(&ast);
        assert!(!text.contains("G4 q mf, G5"), "{text}");
        assert!(text.contains("    @0 G4 q mf\n"), "{text}");
        assert!(
            text.contains("    @0 G5 h mp articulation=legato\n"),
            "{text}"
        );
        assert_eq!(parser::parse(&text).unwrap(), ast);
        assert_eq!(serialize(&parser::parse(&text).unwrap()), text);
    }

    #[test]
    fn mixed_line_with_a_chord_member_expands_to_one_comma_list() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @0.0 [G4 G5] q mf, C4 q mf\n";
        let ast = parser::parse(source).unwrap();
        let text = serialize(&ast);
        assert!(
            text.contains("    @0 G4 q mf, G5 q mf, C4 q mf\n"),
            "{text}"
        );
        assert_eq!(parser::parse(&text).unwrap(), ast);
    }

    #[test]
    fn comma_formatting_is_stable_when_run_twice() {
        let source = "@project\n  tempo: 120\n  time_signature: 4/4\n\n@track t | instrument: piano/acoustic_grand\n  @phrase p [1:1:0 - 3:1:0]\n    @0.0 [G4 G5] q mf, C4 q mf\n    @2 A4 h mp, B4 h mp\n";
        let once = serialize(&parser::parse(source).unwrap());
        let twice = serialize(&parser::parse(&once).unwrap());
        assert_eq!(once, twice);
    }

    #[test]
    fn a_lane_carries_an_optional_span_and_a_mute_flag() {
        let source = "@project\n  tempo: 120\n@track t1 | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor [3:1 - 5:1] {4:1=0.5}\n@track t2 | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor muted {1:1=0.25}\n@bus b\n  routes: [master]\n  effects:\n    - gain: factor=1\n  automation: gain.factor [2:1 - 4:1] muted {3:1=0.75}\n@master\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1=0.5}\n";
        let project = parser::parse(source).unwrap();
        let ranged = &project.tracks[0].automation_lane(0).unwrap();
        let muted = &project.tracks[1].automation_lane(0).unwrap();
        let both = &project.buses[0].automation[0];
        assert_eq!(
            ranged.time_range,
            Some(TimeRange {
                start: BarBeat::new(3, 1, 0),
                end: BarBeat::new(5, 1, 0),
            })
        );
        assert!(!ranged.muted);
        assert_eq!(muted.time_range, None);
        assert!(muted.muted);
        assert!(both.muted);
        assert_eq!(
            both.time_range,
            Some(TimeRange {
                start: BarBeat::new(2, 1, 0),
                end: BarBeat::new(4, 1, 0),
            })
        );
        assert!(!project.tracks[0].automation_lane(0).unwrap().muted);
        let legacy = &project.master.as_ref().unwrap().automation[0];
        assert_eq!(legacy.time_range, None);
        assert!(!legacy.muted);

        let text = serialize(&project);
        assert!(
            text.contains("  automation: gain.factor [3:1 - 5:1] {4:1=0.5}\n"),
            "{text}"
        );
        assert!(
            text.contains("  automation: gain.factor muted {1:1=0.25}\n"),
            "{text}"
        );
        assert!(
            text.contains("  automation: gain.factor [2:1 - 4:1] muted {3:1=0.75}\n"),
            "{text}"
        );
        // A lane without a span and without a mute keeps its old text byte for
        // byte, including the point list that follows the header.
        assert!(
            text.contains("  automation: gain.factor {1:1=0.5}\n"),
            "{text}"
        );
        assert_eq!(parser::parse(&text).unwrap(), project);
        assert_eq!(serialize(&parser::parse(&text).unwrap()), text);
    }

    #[test]
    fn a_legacy_lane_serializes_to_the_same_bytes_and_json() {
        let source = "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor mode=envelope smoothing=0s {1:1=0.5}\n";
        // `mode=envelope` and `smoothing=0s` are the defaults, so a legacy lane
        // written with them is the same lane as one without them.
        let project = parser::parse(source).unwrap();
        let lane = &project.tracks[0].automation_lane(0).unwrap();
        assert_eq!(
            serialize(&project),
            serialize(&parser::parse(
                "@project\n  tempo: 120\n@track t | instrument: piano/acoustic_grand\n  effects:\n    - gain: factor=1\n  automation: gain.factor {1:1=0.5}\n"
            )
            .unwrap())
        );
        // `None` and `false` are absent from the wire form, exactly as before
        // the fields existed.
        assert_eq!(
            serde_json::to_string(lane).unwrap(),
            r#"{"param":"gain.factor","ppq":0,"points":[{"position":{"bar":1,"beat":1,"subdivision":0},"value":{"Number":0.5}}]}"#
        );
        let old: AutomationLane =
            serde_json::from_str(r#"{"param":"gain.factor","ppq":0,"points":[]}"#).unwrap();
        assert_eq!(old.time_range, None);
        assert!(!old.muted);
    }
}
