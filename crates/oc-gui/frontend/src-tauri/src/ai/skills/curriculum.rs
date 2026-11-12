//! Exercises returned on demand, keeping individual topics out of the skill index.

use super::{Skill, read};

pub(super) const ID: &str = "production-curriculum";
const DATA: &str = include_str!("production-curriculum.tsv");
const MAX_SUGGESTIONS: usize = 12;

struct Exercise {
    topic: &'static str,
    level: &'static str,
    fields: &'static str,
    workflow: &'static str,
    session: &'static str,
    check: &'static str,
}

fn exercises() -> impl Iterator<Item = Exercise> {
    DATA.lines().skip(1).map(|line| {
        let mut columns = line.split('\t');
        Exercise {
            topic: columns.next().expect("curriculum topic"),
            level: columns.next().expect("curriculum level"),
            fields: columns.next().expect("curriculum fields"),
            workflow: columns.next().expect("curriculum workflow"),
            session: columns.next().expect("curriculum session"),
            check: columns.next().expect("curriculum check"),
        }
    })
}

fn topic_key(topic: &str) -> String {
    topic
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_exercise(exercise: Exercise) -> Result<Skill, String> {
    let mut skill = read(ID)?;
    skill.title = exercise.topic.to_owned();
    skill.body = format!(
        "## Session\n{}\n\n## Check\n{}\n\nFor the workflow, call read_skill with id `{}`.",
        exercise.session, exercise.check, exercise.workflow,
    );
    if exercise.level == "field" {
        let topics = exercises()
            .filter(|child| {
                child.level != "field" && child.fields.split(',').any(|f| f == exercise.fields)
            })
            .map(|child| format!("- {}", child.topic))
            .collect::<Vec<_>>()
            .join("\n");
        skill.body.push_str("\n\n## Topics in this field\n");
        skill.body.push_str(&topics);
    }
    Ok(skill)
}

pub(super) fn read_topic(topic: &str) -> Result<Skill, String> {
    if topic.len() > 200 {
        return Err("curriculum topic must be at most 200 bytes".to_owned());
    }
    let topic = topic.trim();
    let key = topic_key(topic);
    if key.is_empty() {
        return Err("curriculum topic must contain a field, genre or technique name".to_owned());
    }
    let exact = topic.to_lowercase();
    if let Some(exercise) = exercises().find(|entry| entry.topic.to_lowercase() == exact) {
        return format_exercise(exercise);
    }
    let key = match key.as_str() {
        "dnb" | "d n b" | "d b" | "drum n bass" => "drum and bass",
        "ukg" => "uk garage",
        _ => &key,
    };
    let mut equivalent = exercises().filter(|entry| topic_key(entry.topic) == key);
    if let Some(exercise) = equivalent.next()
        && equivalent.next().is_none()
    {
        return format_exercise(exercise);
    }

    let mut suggestions = exercises()
        .filter(|entry| {
            let candidate = topic_key(entry.topic);
            key.split_whitespace().all(|word| {
                candidate
                    .split_whitespace()
                    .any(|candidate| candidate.starts_with(word))
            })
        })
        .map(|entry| entry.topic)
        .take(MAX_SUGGESTIONS + 1)
        .collect::<Vec<_>>();
    let mut skill = read(ID)?;
    if suggestions.is_empty() {
        skill.body.insert_str(
            0,
            "No matching topic. Try a more specific label or one of these fields.\n\n",
        );
    } else {
        let more = suggestions.len() > MAX_SUGGESTIONS;
        suggestions.truncate(MAX_SUGGESTIONS);
        skill.body = format!(
            "Choose an exact topic and call read_skill again with it.{}\n\n{}",
            if more {
                " Narrow your search to see more specific topics."
            } else {
                ""
            },
            suggestions
                .iter()
                .map(|name| format!("- {name}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(skill)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn all_bundled_exercises_resolve_through_the_topic_lookup() {
        let mut topics = BTreeSet::new();
        let mut fields = BTreeSet::new();
        for exercise in exercises() {
            assert!(
                topics.insert(exercise.topic.to_lowercase()),
                "{}",
                exercise.topic
            );
            if exercise.level == "field" {
                assert!(fields.insert(exercise.fields));
            }
            assert!(read(exercise.workflow).is_ok(), "{}", exercise.workflow);
            let result = read_topic(exercise.topic).unwrap();
            assert_eq!(result.title, exercise.topic);
            assert!(result.body.contains(exercise.session));
            assert!(result.body.contains(exercise.check));
        }
        assert_eq!(topics.len(), 221);
        assert_eq!(fields.len(), 13);
        for exercise in exercises().filter(|entry| entry.level != "field") {
            for field in exercise.fields.split(',') {
                let parent = exercises()
                    .find(|entry| entry.level == "field" && entry.fields == field)
                    .expect("every exercise belongs to a browsable field");
                let result = read_topic(parent.topic).unwrap();
                let label = format!("- {}", exercise.topic);
                assert!(result.body.lines().any(|line| line == label));
            }
        }
    }

    #[test]
    fn discovery_resolves_common_genre_shorthand() {
        for alias in ["DnB", "D&B", "D'n'B", "drum 'n' bass"] {
            assert_eq!(read_topic(alias).unwrap().title, "Drum and bass");
        }
        assert_eq!(read_topic("UKG").unwrap().title, "UK garage");
    }

    #[test]
    fn discovery_returns_bounded_labels_and_fields_return_their_children() {
        let field = read_topic("Electronic club music").unwrap();
        assert!(field.body.contains("- Goa trance"));
        assert!(!field.body.contains("- Singeli"));
        let topic = read_topic(" UK-GARAGE ").unwrap();
        assert_eq!(topic.title, "UK garage");
        let suggestions = read_topic("techno").unwrap();
        assert!(suggestions.body.contains("- Acid techno"));
        assert!(!suggestions.body.contains("## Session"));
        assert!(
            read_topic("unlisted genre")
                .unwrap()
                .body
                .contains("No matching topic")
        );
        assert!(read_topic(" ").is_err());
    }
}
