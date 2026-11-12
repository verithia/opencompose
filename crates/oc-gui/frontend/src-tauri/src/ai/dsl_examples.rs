//! Guards the prompt's DSL reference: every example it renders is data, and
//! every fragment here is parsed by the real grammar. A drifted example fails
//! the build instead of a live session.

use serde::Deserialize;

#[derive(Deserialize)]
struct ExampleFile {
    #[allow(dead_code)]
    version: u32,
    examples: Vec<Example>,
}

#[derive(Deserialize)]
struct Example {
    id: String,
    #[serde(default)]
    wrap: String,
    fragment: String,
}

const EXAMPLES: &str = include_str!("../../../src/ai/dsl-examples.json");

#[test]
fn every_example_the_prompt_shows_parses() {
    let file: ExampleFile =
        serde_json::from_str(EXAMPLES).expect("dsl-examples.json is valid JSON");
    assert!(
        !file.examples.is_empty(),
        "the DSL reference lost every example"
    );
    for example in &file.examples {
        let mut source = String::new();
        if !example.wrap.is_empty() {
            source.push_str(&example.wrap);
            source.push('\n');
        }
        source.push_str(&example.fragment);
        source.push('\n');
        let project = oc_dsl::parse(&source).unwrap_or_else(|error| {
            panic!(
                "example '{}' does not parse: {error}\n---\n{source}",
                example.id
            )
        });
        // A fragment the renderer would refuse is a wrong example, not a style
        // choice: the model copies it and the session fails a render later.
        // References to a preset, bus or pattern another example defines are the
        // exception: the reference is one document split into fragments.
        let errors: Vec<String> = oc_dsl::validator::validate(&project)
            .err()
            .unwrap_or_default()
            .iter()
            .map(ToString::to_string)
            .filter(|error| {
                ![
                    "Unknown preset reference",
                    "Unknown bus reference",
                    "Unknown pattern reference",
                    "Unknown route reference",
                ]
                .iter()
                .any(|prefix| error.starts_with(prefix))
            })
            .collect();
        assert!(
            errors.is_empty(),
            "example '{}' does not validate: {errors:?}\n---\n{source}",
            example.id
        );
    }
}
