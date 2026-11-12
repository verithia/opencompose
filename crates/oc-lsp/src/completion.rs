//! Completion — context-aware autocomplete suggestions.

use oc_dsl::ast::*;
use tower_lsp::lsp_types::*;

/// Provide completions based on cursor position.
pub fn complete(source: &str, project: &Option<Project>, pos: Position) -> Vec<CompletionItem> {
    let line_idx = pos.line as usize;
    let Some(line) = crate::position::line_at(source, pos.line) else {
        return Vec::new();
    };
    let Some(col) = crate::position::byte_column(line, pos.character) else {
        return Vec::new();
    };
    let prefix = line[..col].trim_start();

    let mut items = Vec::new();

    // Top-level keywords
    if prefix.starts_with('@') || prefix.is_empty() {
        items.extend(keyword_completions());
    }

    // After "instrument:" — suggest instrument paths
    if prefix.contains("instrument:") {
        items.extend(instrument_completions());
    }

    // After "preset:" — suggest project preset names
    if prefix.contains("preset:")
        && let Some(proj) = project
    {
        for preset in &proj.presets {
            items.push(CompletionItem {
                label: preset.name.clone(),
                kind: Some(CompletionItemKind::VALUE),
                detail: Some(format!(
                    "Preset: {}",
                    preset.source.as_deref().unwrap_or("?")
                )),
                ..Default::default()
            });
        }
    }

    // After "source:" — suggest preset sources
    if prefix.contains("source:") {
        for s in &["soundfont", "clap", "vst3", "lv2", "builtin", "sample"] {
            items.push(CompletionItem {
                label: s.to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                ..Default::default()
            });
        }
    }

    // After "articulation=" — suggest articulations
    if prefix.contains("articulation=") {
        items.extend(articulation_completions());
    }

    // Dynamics after pitch+duration
    if looks_like_after_duration(prefix) {
        items.extend(dynamics_completions());
    }

    // Effect types after "- "
    if prefix.trim_start().starts_with('-') {
        items.extend(effect_type_completions());
    }

    // Drum names in patterns
    if is_in_pattern_context(source, line_idx) {
        items.extend(drum_completions());
    }

    items
}

fn keyword_completions() -> Vec<CompletionItem> {
    vec![
        ("@project", "Project header", "@project \"${1:Name}\"\n  tempo: ${2:120}\n  time_signature: ${3:4/4}\n  key: ${4:C major}"),
        ("@track", "Track definition", "@track ${1:name} | instrument: ${2:piano/acoustic_grand}\n  mix: vol=${3:0}dB, pan=${4:0}"),
        ("@track (preset)", "Track with preset", "@track ${1:name} | preset: ${2:preset_name}\n  mix: vol=${3:0}dB, pan=${4:0}"),
        ("@phrase", "Phrase block", "@phrase ${1:name} [${2:1}:1:0 - ${3:4}:4:0]"),
        ("@pattern", "Drum pattern", "@pattern ${1:name} [${2:1}:1:0] x${3:4}"),
        ("@preset", "Sound preset", "@preset ${1:name}\n  source: ${2:soundfont}\n  program: ${3:0}"),
        ("@bus", "Effect bus", "@bus ${1:name}\n  routes: [${2:track1}]\n  effects:\n    - reverb: decay=${3:2}s, mix=${4:30}%"),
        ("@master", "Master chain", "@master\n  effects:\n    - limiter: ceiling=-0.3dB"),
        ("@section", "Section marker", "@section ${1:name} [${2:1}:1:0 - ${3:8}:4:0]\n  label: \"${4:Description}\""),
    ].into_iter().map(|(label, detail, snippet)| CompletionItem {
        label: label.to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some(detail.to_string()),
        insert_text: Some(snippet.to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    }).collect()
}

fn instrument_completions() -> Vec<CompletionItem> {
    vec![
        "piano/acoustic_grand",
        "piano/electric",
        "organ/drawbar",
        "guitar/acoustic_steel",
        "guitar/electric_clean",
        "guitar/distortion",
        "bass/electric_finger",
        "bass/acoustic",
        "bass/synth",
        "strings/violin",
        "strings/cello",
        "strings/section",
        "brass/trumpet",
        "brass/french_horn",
        "brass/section",
        "woodwinds/flute",
        "woodwinds/clarinet",
        "woodwinds/oboe",
        "synth/lead_saw",
        "synth/pad_warm",
        "synth/bass_sub",
        "drums/acoustic_kit",
        "drums/electronic",
    ]
    .into_iter()
    .map(|name| CompletionItem {
        label: name.to_string(),
        kind: Some(CompletionItemKind::VALUE),
        ..Default::default()
    })
    .collect()
}

fn articulation_completions() -> Vec<CompletionItem> {
    vec![
        "legato",
        "staccato",
        "staccatissimo",
        "tenuto",
        "marcato",
        "accent",
        "sustained",
        "detached",
        "pizzicato",
        "arco",
        "tremolo",
        "trill",
        "harmonics",
        "muted",
        "sforzando",
        "palm_mute",
        "hammer_on",
        "slide",
    ]
    .into_iter()
    .map(|name| CompletionItem {
        label: name.to_string(),
        kind: Some(CompletionItemKind::ENUM_MEMBER),
        ..Default::default()
    })
    .collect()
}

fn dynamics_completions() -> Vec<CompletionItem> {
    vec!["ppp", "pp", "p", "mp", "mf", "f", "ff", "fff"]
        .into_iter()
        .map(|d| CompletionItem {
            label: d.to_string(),
            kind: Some(CompletionItemKind::ENUM_MEMBER),
            detail: Some("Dynamic marking".into()),
            ..Default::default()
        })
        .collect()
}

fn effect_type_completions() -> Vec<CompletionItem> {
    vec![
        ("eq", "eq: ${1:param}=${2:value}"),
        ("compression", "compression: ratio=${1:4}:1, threshold=${2:-12}dB, attack=${3:10}ms, release=${4:100}ms"),
        ("leveler", "leveler: target=${1:-20}dB, speed=${2:3}, window=${3:1000}ms, gate=${4:-45}dB"),
        ("opto", "opto: peak_reduction=${1:6}dB, release=${2:1500}ms"),
        ("fet", "fet: drive=${1:0}dB, attack=${2:0.3}ms, release=${3:120}ms, output=${4:0}dB, bias=${5:25}%"),
        ("iron", "iron: drive=${1:4}dB, bias=${2:0}, low_freq=${3:25}Hz, high_freq=${4:18}kHz"),
        ("passive_eq", "passive_eq: low_freq=${1:100}Hz, low_boost=${2:5}dB, low_cut=${3:5}dB, high_boost=${4:3}dB, tilt=${5:0}dB, air=${6:0}dB"),
        ("doubler", "doubler: voices=${1:2}, delay=${2:15}ms, rate=${3:0.3}, depth=${4:2}ms, detune=${5:0}, focus=${6:300}Hz, mix=${7:50}%"),
        ("exciter", "exciter: amount=${1:50}%, drive=${2:6}dB, character=${3:0}%, low_freq=${4:9.5}kHz, high_freq=${5:16}kHz"),
        ("ducked_send", "ducked_send: predelay=${1:30}ms, depth=${2:9}dB, attack=${3:20}ms, release=${4:320}ms, threshold=${5:-45}dB, width=${6:120}%, sidechain=${7:Vocal}"),
        ("resonance", "resonance: threshold=${1:6}dB, depth=${2:9}dB, selectivity=${3:35}%, attack=${4:10}ms, release=${5:150}ms, mix=${6:100}%"),
        ("tuner", "tuner: amount=${1:100}%, retune=${2:50}ms, key=${3:C}, scale=${4:major}, mode=${5:full_range}"),
        ("reverb", "reverb: decay=${1:2}s, mix=${2:30}%"),
        ("delay", "delay: time=${1:375}ms, feedback=${2:30}%, mix=${3:25}%"),
        ("chorus", "chorus: rate=${1:1.5}, depth=${2:3}, mix=${3:30}%"),
        ("limiter", "limiter: ceiling=${1:-0.3}dB"),
        ("distortion", "distortion: drive=${1:30}%, mix=${2:20}%"),
    ].into_iter().map(|(label, snippet)| CompletionItem {
        label: label.to_string(),
        kind: Some(CompletionItemKind::FUNCTION),
        insert_text: Some(snippet.to_string()),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    }).collect()
}

fn drum_completions() -> Vec<CompletionItem> {
    vec![
        "kick", "snare", "hihat", "crash", "ride", "splash", "tom_high", "tom_mid", "tom_low",
        "cowbell", "clap", "rim",
    ]
    .into_iter()
    .map(|name| CompletionItem {
        label: name.to_string(),
        kind: Some(CompletionItemKind::VALUE),
        ..Default::default()
    })
    .collect()
}

fn looks_like_after_duration(prefix: &str) -> bool {
    let words: Vec<&str> = prefix.split_whitespace().collect();
    // Pattern: "C4 q" or "Bb3 8th" — pitch then duration, cursor after duration
    words.len() >= 2 && {
        let last = words[words.len() - 1];
        matches!(last, "w" | "h" | "q" | "8th" | "16th" | "32nd")
    }
}

fn is_in_pattern_context(source: &str, line_idx: usize) -> bool {
    // Walk backwards to find if we're inside a @pattern block
    let lines: Vec<&str> = source.lines().collect();
    for i in (0..=line_idx).rev() {
        let trimmed = lines.get(i).map(|l| l.trim()).unwrap_or("");
        if trimmed.starts_with("@pattern") {
            return true;
        }
        if trimmed.starts_with("@track")
            || trimmed.starts_with("@phrase")
            || trimmed.starts_with("@bus")
            || trimmed.starts_with("@master")
            || trimmed.starts_with("@project")
            || trimmed.starts_with("@preset")
        {
            return false;
        }
    }
    false
}
