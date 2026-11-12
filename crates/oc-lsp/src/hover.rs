//! Hover — show info when hovering over DSL elements.

use oc_dsl::ast::*;
use tower_lsp::lsp_types::*;

/// Provide hover info for the token under the cursor.
pub fn hover_info(source: &str, project: &Option<Project>, pos: Position) -> Option<Hover> {
    let line = crate::position::line_at(source, pos.line)?;
    let col = crate::position::byte_column(line, pos.character)?;

    // Get the word under cursor
    let word = word_at(line, col)?;

    // Check what the word is
    let info = if let Some(proj) = project {
        hover_word(word, proj)
    } else {
        hover_keyword(word)
    };

    info.map(|content| Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: content,
        }),
        range: None,
    })
}

fn hover_word(word: &str, project: &Project) -> Option<String> {
    // Check if it's a track name
    if let Some(track) = project.tracks.iter().find(|t| t.name == word) {
        let sound = match &track.preset {
            Some(p) => p.clone(),
            None => track.instrument.as_path(),
        };
        return Some(format!(
            "**Track:** `{}`\n\n- Sound: `{}`\n- Volume: {}dB\n- Pan: {}\n- Phrases: {}\n- Patterns: {}\n- Effects: {}",
            track.name,
            sound,
            track.mix.volume_db,
            track.mix.pan,
            track.phrases.len(),
            track.patterns.len(),
            track.effects.len()
        ));
    }

    // Check if it's a preset name
    if let Some(preset) = project.presets.iter().find(|p| p.name == word) {
        let source = preset.source.as_deref().unwrap_or("?");
        let used_by: Vec<&str> = project
            .tracks
            .iter()
            .filter(|t| t.preset.as_deref() == Some(&preset.name))
            .map(|t| t.name.as_str())
            .collect();
        return Some(format!(
            "**Preset:** `{}`\n\n- Source: `{}`\n- Program: {}\n- Effects: {}\n- Used by: {}",
            preset.name,
            source,
            preset.program.map(|p| p.to_string()).unwrap_or("—".into()),
            preset.effects.len(),
            if used_by.is_empty() {
                "none".into()
            } else {
                used_by.join(", ")
            }
        ));
    }

    // Check if it's a bus name
    if let Some(bus) = project.buses.iter().find(|b| b.name == word) {
        return Some(format!(
            "**Bus:** `{}`\n\n- Routes: {}\n- Effects: {}",
            bus.name,
            bus.routes.join(", "),
            bus.effects.len()
        ));
    }

    // Check for pitch notation
    if let Some(midi) = Pitch::from_text(word).and_then(|pitch| pitch.checked_to_midi()) {
        let freq = 440.0 * 2.0_f64.powf((midi as f64 - 69.0) / 12.0);
        return Some(format!(
            "**Pitch:** `{}` — MIDI {} — {:.1} Hz",
            word, midi, freq
        ));
    }

    // Check for dynamics
    if let Some(vel) = dynamics_velocity(word) {
        return Some(format!("**Dynamics:** `{}` — velocity {} / 127", word, vel));
    }

    // Fall back to keyword hover
    hover_keyword(word)
}

fn hover_keyword(word: &str) -> Option<String> {
    match word {
        "tempo" => Some("**tempo:** Project tempo in BPM (beats per minute)".into()),
        "time_signature" => {
            Some("**time_signature:** Beats per bar / beat unit (e.g. 4/4, 3/4, 6/8)".into())
        }
        "key" => Some("**key:** Musical key (e.g. C major, Bb minor)".into()),
        "instrument" => Some(
            "**instrument:** Sound source as `category/name` (e.g. piano/acoustic_grand)".into(),
        ),
        "preset" => {
            Some("**preset:** Reference to a @preset block defining the sound source".into())
        }
        "mix" => Some("**mix:** Track mix settings: vol (dB), pan (-100 to +100), sends".into()),
        "effects" => {
            Some("**effects:** Insert effect chain (eq, compression, reverb, delay, etc.)".into())
        }
        "articulation" => {
            Some("**articulation:** Playing technique (legato, staccato, pizzicato, etc.)".into())
        }
        "vibrato" => {
            Some("**vibrato:** Pitch oscillation: rate (Hz), depth (cents), delay (seconds)".into())
        }
        "expression" => {
            Some("**expression:** Continuous parameter change over a note's duration".into())
        }
        "source" => {
            Some("**source:** Preset backend: soundfont, clap, vst3, lv2, builtin, sample".into())
        }
        _ => None,
    }
}

fn word_at(line: &str, col: usize) -> Option<&str> {
    if col >= line.len() || !line.is_char_boundary(col) {
        return None;
    }
    let is_word = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '/' | '#' | '-');
    let mut start = col;
    for (byte, ch) in line[..col].char_indices().rev() {
        if !is_word(ch) {
            break;
        }
        start = byte;
    }
    let mut end = col;
    for (byte, ch) in line[col..].char_indices() {
        if !is_word(ch) {
            break;
        }
        end = col + byte + ch.len_utf8();
    }
    if start == end {
        return None;
    }
    Some(&line[start..end])
}

fn dynamics_velocity(word: &str) -> Option<u8> {
    Dynamics::from_token(word).map(|dynamics| dynamics.to_velocity())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_hover_uses_checked_model_conversion_including_octave_minus_one() {
        let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        assert!(hover_word("C-1", &project).unwrap().contains("MIDI 0"));
        assert!(hover_word("Bb-1", &project).unwrap().contains("MIDI 10"));
        assert!(hover_word("G9", &project).unwrap().contains("MIDI 127"));
        for text in ["A9", "Cb-1", "C-2"] {
            assert!(hover_word(text, &project).is_none(), "{text}");
        }
        assert_eq!(word_at("C-1 q mf", 2), Some("C-1"));
    }

    #[test]
    fn dynamics_hover_uses_the_dsl_velocity() {
        let project = oc_dsl::parse("@project\n  tempo: 120\n").unwrap();
        for token in ["ppp", "pp", "p", "mp", "mf", "f", "ff", "fff"] {
            let velocity = Dynamics::from_token(token).unwrap().to_velocity();
            let hover = hover_word(token, &project).unwrap();
            assert!(
                hover.contains(&format!("velocity {velocity} / 127")),
                "{hover}"
            );
        }
    }
}
