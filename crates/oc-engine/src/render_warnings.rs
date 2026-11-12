//! Non-fatal render diagnostics collected while a render pass runs.
//!
//! A warning means the render continued with a substitute: silence instead of
//! a plugin that could not be hosted, or a default patch instead of preset
//! state. Callers drain the list after rendering and report it to the user.
//!
//! A message that repeats — one silent drum hit per bar, one unsound pad per
//! hit — is recorded once and counted, so a drain reports one line per problem
//! with the number of occurrences instead of one line per hit.

use std::sync::Mutex;

struct Warning {
    message: String,
    count: usize,
}

static WARNINGS: Mutex<Vec<Warning>> = Mutex::new(Vec::new());

/// Serialises the tests that reset the process-wide buffer, so one test's
/// drain cannot empty another test's window.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Count one occurrence of `message`, in first-occurrence order.
fn record(warnings: &mut Vec<Warning>, message: String) {
    if let Some(entry) = warnings.iter_mut().find(|entry| entry.message == message) {
        entry.count += 1;
        return;
    }
    warnings.push(Warning { message, count: 1 });
}

/// One line per warning; a repeated message carries its count.
fn lines(warnings: &[Warning]) -> Vec<String> {
    warnings
        .iter()
        .map(|entry| match entry.count {
            1 => entry.message.clone(),
            count => format!("{} ({count} occurrences)", entry.message),
        })
        .collect()
}

/// Record one occurrence of a warning.
///
/// The same message increments its count. Nothing is logged here: a render
/// reports its warnings once, from [`take`], so a repeated problem costs one
/// line rather than one line per occurrence.
pub fn push(warning: impl Into<String>) {
    let message = warning.into();
    let mut warnings = WARNINGS.lock().unwrap_or_else(|error| error.into_inner());
    record(&mut warnings, message);
}

pub fn clear() {
    WARNINGS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

/// Take every warning recorded since the last call.
///
/// The list holds one line per distinct message, in first-occurrence order. A
/// message that fired more than once carries its count, for example
/// `... the hit is silent (256 occurrences)`.
pub fn take() -> Vec<String> {
    let mut warnings = WARNINGS.lock().unwrap_or_else(|error| error.into_inner());
    lines(&std::mem::take(&mut *warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(messages: &[&str]) -> Vec<String> {
        let mut warnings = Vec::new();
        for message in messages {
            record(&mut warnings, (*message).to_string());
        }
        lines(&warnings)
    }

    #[test]
    fn a_repeated_warning_becomes_one_line_with_its_count() {
        let messages = vec!["drum hit 'crash' is silent"; 100];
        assert_eq!(
            collect(&messages),
            vec!["drum hit 'crash' is silent (100 occurrences)"]
        );
    }

    #[test]
    fn distinct_warnings_keep_separate_lines_in_first_occurrence_order() {
        assert_eq!(
            collect(&[
                "drum hit 'crash' is silent",
                "drum hit 'ride' is silent",
                "drum hit 'crash' is silent",
            ]),
            vec![
                "drum hit 'crash' is silent (2 occurrences)",
                "drum hit 'ride' is silent",
            ]
        );
    }

    #[test]
    fn a_warning_that_fired_once_keeps_its_plain_text() {
        assert_eq!(
            collect(&["AU preset 'keys' cannot render in process"]),
            vec!["AU preset 'keys' cannot render in process"]
        );
    }
}
