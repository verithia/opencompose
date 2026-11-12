//! The skill that explains a refusal, as data next to the skill loader.
//!
//! The catalog is large and the model reads it once, so a refusal names the
//! remedy: the payload gains a `skill` id and the prose call that reads it when
//! one of the bundled skills already explains the message. The `error` text is
//! never rewritten.
use serde_json::{Value, json};

/// Where the class text sits in the message the code produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// The message starts with the text.
    Prefix,
    /// The text follows a name the refusal cannot predict.
    Contains,
}

/// One refusal class and the skill that explains it.
#[derive(Debug)]
pub struct Hint {
    /// Text the code already produces, verbatim.
    pub class: &'static str,
    pub anchor: Anchor,
    /// A skill id; a test proves the loader can read one for every row.
    pub skill: &'static str,
}

/// Every refusal a bundled skill explains. The first match wins.
pub const HINTS: &[Hint] = &[
    // Subtree path grammar, from oc_dsl::ops.
    Hint {
        class: "Unknown path segment: ",
        anchor: Anchor::Prefix,
        skill: "dsl-subtree-paths",
    },
    Hint {
        class: "Unsupported replace path",
        anchor: Anchor::Prefix,
        skill: "dsl-subtree-paths",
    },
    Hint {
        class: "Invalid track child for replace",
        anchor: Anchor::Prefix,
        skill: "dsl-subtree-paths",
    },
    // The section surface, from the section tools. The manual names the tools;
    // the message already quotes the offending name and range.
    Hint {
        class: "section '",
        anchor: Anchor::Prefix,
        skill: "dsl-subtree-paths",
    },
    Hint {
        class: "cannot rename section '",
        anchor: Anchor::Prefix,
        skill: "dsl-subtree-paths",
    },
    // Which engine plays a track, from the engine's render cache.
    Hint {
        class: "needs a SoundFont",
        anchor: Anchor::Contains,
        skill: "playable-instruments",
    },
    // A preset names media that is not on disk; the edit layer reports it as
    // `preset '<name>': file '<path>' not found (project directory '<dir>')`.
    Hint {
        class: "preset '",
        anchor: Anchor::Prefix,
        skill: "playable-instruments",
    },
    // An agent looping over narrow windows: the remedy is a listening clip.
    Hint {
        class: "narrow-window loop: ",
        anchor: Anchor::Prefix,
        skill: "audition-and-listen",
    },
    // Render errors and their fixes.
    Hint {
        class: "output directory ",
        anchor: Anchor::Prefix,
        skill: "fix-a-render-error",
    },
    Hint {
        class: "cannot be used as an effect",
        anchor: Anchor::Contains,
        skill: "fix-a-render-error",
    },
    // The panel's own refusals: the tool is not offered in this conversation.
    Hint {
        class: "refused: ",
        anchor: Anchor::Prefix,
        skill: "fix-a-render-error",
    },
    Hint {
        class: "unknown tool '",
        anchor: Anchor::Prefix,
        skill: "fix-a-render-error",
    },
    // Specific effect syntax refusals precede the generic parse-error fallback.
    Hint {
        class: "expected effect_entry",
        anchor: Anchor::Contains,
        skill: "effects-and-devices",
    },
    Hint {
        class: "unknown effect type:",
        anchor: Anchor::Contains,
        skill: "effects-and-devices",
    },
    Hint {
        class: "expected effect_type",
        anchor: Anchor::Contains,
        skill: "effects-and-devices",
    },
    // The entry parsed and its parameter list did not: a `name: value` line
    // under it. Without this row the class falls through to the generic row and
    // sends the model to `writing-notes`, which does not teach effect parameters.
    Hint {
        class: "expected effect_param",
        anchor: Anchor::Contains,
        skill: "effects-and-devices",
    },
    // The same refusal after the entry itself parsed: the parser wanted another
    // entry and got a parameter list on its own line.
    Hint {
        class: "or effect_entry",
        anchor: Anchor::Contains,
        skill: "effects-and-devices",
    },
    // A value the DSP factory cannot type (the validation preflight and the
    // render both refuse it); the skill lists the accepted names and units.
    Hint {
        class: "invalid typed effect parameter",
        anchor: Anchor::Contains,
        skill: "effects-and-devices",
    },
    // An automation line that did not parse: the parser wanted one of the
    // lane's own attributes or its `{...}` point block. This class used to
    // fall through to the generic row and send the model to `writing-notes`,
    // which does not teach lanes.
    Hint {
        class: "expected automation_",
        anchor: Anchor::Contains,
        skill: "write-mix-automation",
    },
    Hint {
        class: "expected lane_mode_name",
        anchor: Anchor::Contains,
        skill: "write-mix-automation",
    },
    // Note syntax, from oc_dsl::parser. `oc_dsl::ops` and the panel's track
    // tools wrap every ParseError as "Parse error: ", which is the class that
    // actually reaches the model; the rows below name each refusal the manual
    // answers, so the table stays complete for a caller that reports the
    // ParseError without the wrapper.
    Hint {
        class: "Parse error: ",
        anchor: Anchor::Prefix,
        skill: "writing-notes",
    },
    Hint {
        class: "Syntax error at line ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "trailing comma: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "comma list starts with ',': ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "has no note event: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "comma list member ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "rest is not a comma list member: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "a comma list member shares the line's position: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "expected a note event after ',': ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "comma list cannot span a line break: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "comma list repeats pitch ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "rest is not a chord member: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "empty chord: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "unclosed chord: ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "chord repeats pitch ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "invalid chord member ",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    // A glissando mark the score cannot carry: no destination, a chord in the
    // way, a pitch control it would fight, or a source whose pitch is fixed.
    // The engine and the parser both start the sentence with the mark.
    Hint {
        class: "gliss",
        anchor: Anchor::Contains,
        skill: "writing-notes",
    },
    Hint {
        class: "Invalid duration: ",
        anchor: Anchor::Prefix,
        skill: "writing-notes",
    },
    Hint {
        class: "Invalid pitch: ",
        anchor: Anchor::Prefix,
        skill: "writing-notes",
    },
];

/// The skill id that explains this refusal, or `None` when no skill does.
pub fn for_message(message: &str) -> Option<&'static str> {
    let message = message_without_location(message);
    HINTS
        .iter()
        .find(|hint| match hint.anchor {
            Anchor::Prefix => message.starts_with(hint.class),
            Anchor::Contains => message.contains(hint.class),
        })
        .map(|hint| hint.skill)
}

/// The validator prefixes some messages with `line:column: `; skill matching
/// keeps using the underlying prose while the payload stays located.
fn message_without_location(message: &str) -> &str {
    let mut parts = message.splitn(3, ':');
    let (Some(line), Some(column), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return message;
    };
    if line.parse::<u32>().is_ok() && column.parse::<u32>().is_ok() {
        rest.trim_start()
    } else {
        message
    }
}

/// Name the loader call explicitly so the remedy is directly actionable.
pub fn hint_text(skill: &str) -> String {
    format!("read_skill('{skill}') teaches this")
}

/// The `{"ok": false, ...}` payload a JSON surface answers with: `error`
/// verbatim, plus `skill` and the prose `hint` only when a skill explains it.
pub fn refusal(message: &str) -> Value {
    let mut payload = json!({ "ok": false, "error": message });
    if let Some(skill) = for_message(message) {
        payload["skill"] = json!(skill);
        payload["hint"] = json!(hint_text(skill));
    }
    payload
}

/// A parser refusal reproduced by the fixture below.
#[cfg(test)]
const PARSE_REFUSAL_FIXTURE: &str = "Parse error: Syntax error at line 8, column 5: \
     expected EOI, top_level_block, track_content, or phrase_event";

/// Invalid effect syntax fixtures, checked against the current parser.
#[cfg(test)]
const EFFECT_REFUSAL_FIXTURES: &[&str] = &[
    "Parse error: Syntax error at line 8, column 5: expected effect_entry",
    "Parse error: Syntax error at line 8, column 5: expected effect_entry",
    "Parse error: Syntax error at line 9, column 7: \
     expected EOI, top_level_block, track_content, or effect_entry",
    "Parse error: Syntax error at line 7, column 12: expected effect_entry",
    "Parse error: Syntax error at line 7, column 7: unknown effect type: filter",
    "Parse error: Syntax error at line 7, column 7: expected effect_type",
];

/// (a DSL fragment that refuses, the message the parser must produce for it).
/// Track-edit wrapping keeps parser line and column numbers stable.
#[cfg(test)]
fn effect_refusing_fragments() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "@track reference | instrument: audio/clip\n  mix: vol=-3dB\n  effects:\n    \
             eq: low_freq=500Hz, low_gain=24dB, high_freq=500Hz, high_gain=-24dB\n",
            "Parse error: Syntax error at line 8, column 5: expected effect_entry",
        ),
        (
            "@track reference | instrument: audio/clip\n  mix: vol=-3dB\n  effects:\n    - eq\n      \
             low_freq: 500Hz\n",
            "Parse error: Syntax error at line 9, column 7: \
             expected EOI, top_level_block, track_content, or effect_entry",
        ),
        (
            "@track reference | instrument: audio/clip\n  mix: vol=-3dB\n  \
             effects: eq(low_freq=500Hz, low_gain=24dB)\n",
            "Parse error: Syntax error at line 7, column 12: expected effect_entry",
        ),
        (
            "@track reference | instrument: audio/clip\n  effects:\n    - filter: cutoff=300Hz\n",
            "Parse error: Syntax error at line 7, column 7: unknown effect type: filter",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hint_names_a_skill_the_loader_can_read() {
        for hint in HINTS {
            let skill = crate::ai::skills::read(hint.skill)
                .unwrap_or_else(|error| panic!("{}: {error}", hint.skill));
            assert_eq!(skill.id, hint.skill);
            assert!(!skill.body.is_empty(), "{} has no body", hint.skill);
            // The prose names the same id the field does, so a model that reads
            // only the sentence still calls the loader with a readable id.
            let probe = match hint.anchor {
                Anchor::Prefix => format!("{}probe", hint.class),
                Anchor::Contains => format!("probe {} probe", hint.class),
            };
            let payload = refusal(&probe);
            assert_eq!(payload["skill"], hint.skill, "{probe}");
            assert_eq!(
                payload["hint"],
                format!("read_skill('{}') teaches this", hint.skill),
                "{probe}"
            );
        }
    }

    #[test]
    fn the_narrow_window_refusal_names_the_skill_that_explains_listening() {
        let message = crate::window_guard::refusal_message();
        assert_eq!(for_message(&message), Some("audition-and-listen"));
        let skill = crate::ai::skills::read("audition-and-listen").expect("the skill is bundled");
        assert_eq!(skill.id, "audition-and-listen");
        assert!(!skill.body.is_empty());
    }

    /// The refusal a sampled source produces, measured from the engine rather
    /// than copied: the producer is `oc_engine::interpret`.
    #[test]
    fn a_slide_on_a_sampled_source_names_the_skill_that_teaches_the_run() {
        let project = oc_dsl::parse(
            "@project \"Gliss\"\n  tempo: 60\n@preset piano\n  source: soundfont\n  program: 0\n@track keys | preset: piano, instrument: piano/acoustic_grand\n  @phrase phrase [1:1 - 2:1]\n    C4 q mf gliss\n    G4 q mf\n",
        )
        .unwrap();
        let message = oc_engine::interpret::try_interpret(
            &project,
            &oc_engine::interpret::InterpretSettings::default(),
        )
        .unwrap_err();
        assert_eq!(for_message(&message), Some("writing-notes"), "{message}");
        let payload = refusal(&message);
        assert_eq!(payload["error"], message.as_str());
        assert_eq!(payload["skill"], "writing-notes");
        assert_eq!(payload["hint"], "read_skill('writing-notes') teaches this");
    }

    #[test]
    fn a_refusal_keeps_the_error_verbatim_and_only_gains_a_skill() {
        let hinted = refusal("Unknown path segment: doc");
        let mut keys: Vec<&str> = hinted
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["error", "hint", "ok", "skill"]);
        assert_eq!(hinted["ok"], false);
        assert_eq!(hinted["error"], "Unknown path segment: doc");
        assert_eq!(hinted["skill"], "dsl-subtree-paths");
        assert_eq!(
            hinted["hint"],
            "read_skill('dsl-subtree-paths') teaches this"
        );

        // A refusal no skill explains keeps the two-key payload it always had.
        let plain = refusal("track 4 out of range");
        let mut keys: Vec<&str> = plain
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["error", "ok"]);
        assert_eq!(plain["ok"], false);
    }

    /// Every class the table claims, one message each. The synthesized line and
    /// column are the ones the wrapped path emits; the producer test below
    /// proves each class against the real parser.
    fn grammar_refusals() -> Vec<String> {
        let mut messages = vec![PARSE_REFUSAL_FIXTURE.to_owned()];
        for (_, class) in refusing_fragments() {
            messages.push(format!(
                "Parse error: Syntax error at line 3, column 1: {class}"
            ));
        }
        for wrapper in ["Invalid duration: '4th'", "Invalid pitch: 'H4'"] {
            messages.push(wrapper.to_owned());
        }
        messages
    }

    /// (a phrase body of DSL, the class text the parser must emit for it).
    /// Fragments are wrapped exactly as `oc_dsl::ops` wraps a track edit.
    fn refusing_fragments() -> &'static [(&'static str, &'static str)] {
        &[
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    C4 q mf,\n",
                "trailing comma: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    , C4 q mf\n",
                "comma list starts with ',': ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    @0.0 ,\n",
                "has no note event: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    C4 q mf, E4\n",
                "comma list member ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    C4 q mf, rest q\n",
                "rest is not a comma list member: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    C4 q mf, @2 E4 q mf\n",
                "a comma list member shares the line's position: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    C4 q mf,\n    E4 q mf\n",
                "comma list cannot span a line break: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    C4 q mf, C4 h mf\n",
                "comma list repeats pitch ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    [C4 rest q] q mf\n",
                "rest is not a chord member: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    [] q mf\n",
                "empty chord: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    [C4 E4 q mf\n",
                "unclosed chord: ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    [C4 C4] q mf\n",
                "chord repeats pitch ",
            ),
            (
                "@track lead | instrument: synth/lead\n  @phrase riff [1:1 - 2:1]\n    [H5] q mf\n",
                "invalid chord member ",
            ),
        ]
    }

    /// The live session hit both messages while trying to write a filter lane.
    #[test]
    fn an_automation_lane_refusal_names_the_automation_skill() {
        for message in [
            "Parse error: Syntax error at line 8, column 18: expected automation_ppq, \
             automation_smoothing, automation_mode, automation_speed, automation_mult",
            "Parse error: Syntax error at line 8, column 24: expected lane_mode_name",
        ] {
            assert_eq!(
                for_message(message),
                Some("write-mix-automation"),
                "{message}"
            );
            let payload = refusal(message);
            assert_eq!(payload["error"], message);
            assert_eq!(payload["skill"], "write-mix-automation");
        }
    }

    /// The class is produced by the parser, not only by the literal above.
    #[test]
    fn the_parser_names_the_automation_class() {
        let dsl = "@project\n  tempo: 120\n  time_signature: 4/4\n\n\
                   @track lead | instrument: synth/lead\n  automation:\n    \
                   filter.cutoff: @0 200Hz, @32 8000Hz\n";
        let message = match oc_dsl::parse(dsl) {
            Ok(_) => panic!("the lane-less line parsed, so it proves no class"),
            Err(error) => format!("Parse error: {error}"),
        };
        assert!(message.contains("expected automation_"), "{message}");
        assert_eq!(for_message(&message), Some("write-mix-automation"));
    }

    #[test]
    fn every_grammar_refusal_names_the_writing_notes_skill() {
        for message in grammar_refusals() {
            assert_eq!(for_message(&message), Some("writing-notes"), "{message}");
        }
    }

    #[test]
    fn the_parser_produces_every_class_the_table_claims() {
        for (dsl, class) in refusing_fragments() {
            let wrapper = format!("@project\n  tempo: 120\n  time_signature: 4/4\n\n{dsl}");
            let error = match oc_dsl::parse(&wrapper) {
                Ok(_) => panic!("{dsl:?} parsed, so it proves no class"),
                Err(error) => error,
            };
            let message = format!("Parse error: {error}");
            assert!(message.contains(class), "{message} lacks {class:?}");
            assert_eq!(for_message(&message), Some("writing-notes"), "{message}");
        }
    }

    #[test]
    fn the_parse_refusal_fixture_matches_the_parser() {
        // Same shape and line/column a live model hit: the wrapper adds four
        // lines, so the offending token is line 8, column 5.
        let dsl = "@track reference | instrument: audio/clip\n  mix: vol=-3dB\n  \
                   @phrase test [1:1:0 - 5:1:0]\n    H4 q mf\n";
        let wrapper = format!("@project\n  tempo: 120\n  time_signature: 4/4\n\n{dsl}");
        let error = match oc_dsl::parse(&wrapper) {
            Ok(_) => panic!("the live refusal no longer reproduces"),
            Err(error) => error,
        };
        assert_eq!(format!("Parse error: {error}"), PARSE_REFUSAL_FIXTURE);
        assert_eq!(for_message(PARSE_REFUSAL_FIXTURE), Some("writing-notes"));
    }

    #[test]
    fn the_effect_syntax_refusals_name_the_effects_skill() {
        for message in EFFECT_REFUSAL_FIXTURES {
            assert_eq!(
                for_message(message),
                Some("effects-and-devices"),
                "{message}"
            );
        }
    }

    #[test]
    fn the_parser_produces_the_effect_refusals_the_table_claims() {
        for (dsl, expected) in effect_refusing_fragments() {
            let wrapper = format!("@project\n  tempo: 120\n  time_signature: 4/4\n\n{dsl}");
            let error = match oc_dsl::parse(&wrapper) {
                Ok(_) => panic!("{dsl:?} parsed, so it proves no refusal"),
                Err(error) => error,
            };
            let message = format!("Parse error: {error}");
            assert_eq!(message, *expected, "the parser changed the refusal");
            assert_eq!(
                for_message(&message),
                Some("effects-and-devices"),
                "{message}"
            );
        }
    }

    /// A malformed effect parameter selects the specific syntax remedy.
    #[test]
    fn the_effect_param_refusal_the_parser_produces_names_the_effects_skill() {
        let dsl = "@track reference | instrument: audio/clip\n  mix: vol=-3dB\n  \
                   effects:\n    - eq:\n        low_freq: 300Hz\n";
        let wrapper = format!("@project\n  tempo: 120\n  time_signature: 4/4\n\n{dsl}");
        let error = match oc_dsl::parse(&wrapper) {
            Ok(_) => panic!("the effect_param refusal no longer reproduces"),
            Err(error) => error,
        };
        let message = format!("Parse error: {error}");
        assert_eq!(
            message,
            "Parse error: Syntax error at line 9, column 9: expected effect_param"
        );
        assert_eq!(for_message(&message), Some("effects-and-devices"));
    }

    /// The hint names the loader operation as well as its skill argument.
    #[test]
    fn a_refusal_carries_the_prose_call_not_just_the_id() {
        for (message, skill) in [
            (
                "Parse error: Syntax error at line 8, column 5: expected effect_entry",
                "effects-and-devices",
            ),
            ("Unknown path segment: doc", "dsl-subtree-paths"),
            (
                "Track 'keys' needs a SoundFont: declare preset.file in the project",
                "playable-instruments",
            ),
            (
                "preset 'grand': file 'media/grand.sf2' not found (project directory '/tmp/song')",
                "playable-instruments",
            ),
            (
                "5:1: preset 'grand': file 'media/grand.sf2' not found (project directory '/tmp/song')",
                "playable-instruments",
            ),
            ("narrow-window loop: 9 windows", "audition-and-listen"),
        ] {
            let payload = refusal(message);
            assert_eq!(payload["ok"], false);
            // The error stays byte-identical; the hint is additive.
            assert_eq!(payload["error"], message);
            assert_eq!(payload["skill"], skill);
            assert_eq!(
                payload["hint"],
                format!("read_skill('{skill}') teaches this")
            );
        }
    }

    #[test]
    fn a_generic_parse_refusal_still_names_writing_notes() {
        // The effect rows come first; a message with no effect class must not be
        // shadowed by them. This is the general grammar refusal, not an effect.
        assert_eq!(for_message(PARSE_REFUSAL_FIXTURE), Some("writing-notes"));
        for message in [
            "Parse error: Syntax error at line 4, column 3: \
             expected EOI, project_property, or top_level_block",
            "Parse error: Syntax error at line 3, column 1: \
             expected EOI, top_level_block, track_content, or phrase_event",
            "Parse error: Syntax error at line 5, column 3: expected pitch, rest, or bar_line",
            "Invalid pitch: 'H4'",
            "trailing comma: a comma list cannot end with ','",
        ] {
            assert_eq!(for_message(message), Some("writing-notes"), "{message}");
        }
    }

    #[test]
    fn the_preexisting_refusal_classes_keep_their_skill() {
        for (message, skill) in [
            ("Unknown path segment: doc", "dsl-subtree-paths"),
            ("Unsupported replace path 'project'", "dsl-subtree-paths"),
            (
                "Invalid track child for replace 'automation'",
                "dsl-subtree-paths",
            ),
            (
                "Track 'keys' needs a SoundFont: declare preset.file in the project",
                "playable-instruments",
            ),
            (
                "preset 'grand': file 'media/grand.sf2' not found (project directory '/tmp/song')",
                "playable-instruments",
            ),
            ("narrow-window loop: 9 windows", "audition-and-listen"),
            (
                "output directory '/tmp/renders' does not exist",
                "fix-a-render-error",
            ),
            (
                "Lead: an instrument plugin (0 audio inputs, 1 stereo output) cannot be used as an effect",
                "fix-a-render-error",
            ),
            (
                "refused: tracks_mix is not in the DSL toolset",
                "fix-a-render-error",
            ),
            ("unknown tool 'render_offline'", "fix-a-render-error"),
        ] {
            assert_eq!(for_message(message), Some(skill), "{message}");
        }
    }

    /// The engine's instrument-insert refusal reaches the agent as `ok: false`
    /// plus the render skill the message names, so the model can read the
    /// remedy instead of guessing. `Render error: ` is the CLI's wrapper.
    #[test]
    fn the_instrument_insert_refusal_reaches_the_agent_with_the_render_skill() {
        let engine = "Kontakt 8: an instrument plugin (0 audio inputs, 32 stereo outputs) cannot \
                      be used as an effect on track 'Lead'; put it on an instrument track, or \
                      wrap it in a bus";
        for message in [engine.to_string(), format!("Render error: {engine}")] {
            let payload = refusal(&message);
            assert_eq!(payload["ok"], false);
            assert_eq!(payload["error"], message);
            assert_eq!(payload["skill"], "fix-a-render-error");
            assert_eq!(
                payload["hint"],
                "read_skill('fix-a-render-error') teaches this"
            );
        }
        // The prose is only useful if the skill it names actually teaches the
        // class; a hint to a page that lacks it sends the model in a circle.
        let skill = crate::ai::skills::read("fix-a-render-error").expect("the skill is bundled");
        assert!(
            skill.body.contains("cannot be used as an effect"),
            "fix-a-render-error does not quote the instrument-insert class"
        );
    }
}
