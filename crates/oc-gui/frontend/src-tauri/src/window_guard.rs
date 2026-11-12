//! An agent that keeps measuring ever-narrower windows is looping, not listening.
//!
//! The guard counts the narrow windows one agent has rendered of one project
//! revision and refuses the next one with a message that names the remedy. It
//! is an agent-session property: the counter lives on the app state, never in
//! the engine, and no render a human drives passes through it.

/// A window under four seconds is a slice of a section, not a section.
pub const NARROW_WINDOW_SECONDS: f64 = 4.0;
/// Eight narrow windows of one revision already cover a short clip; a ninth
/// measures the same thing again.
pub const NARROW_WINDOW_LIMIT: u32 = 8;

/// The refusal a model reads. `ai::skill_hints` maps its class to the
/// `audition-and-listen` skill, so the remedy is one `read_skill` away.
pub fn refusal_message() -> String {
    format!(
        "narrow-window loop: {NARROW_WINDOW_LIMIT} windows under \
         {NARROW_WINDOW_SECONDS:.0} s of the same revision. Measuring more windows will not tell \
         you how it sounds; call audition_range on the section you care about, or analyze_notes to \
         get the notes (or render a section, not a beat), or read the audition-and-listen skill."
    )
}

/// Narrow windows admitted so far for one project revision.
#[derive(Debug, Default)]
pub struct WindowGuard {
    revision: Option<u64>,
    narrow: u32,
}

impl WindowGuard {
    /// Count a window before it renders; `Err` is the refusal. A revision the
    /// guard has not seen starts a fresh count, so opening or editing the
    /// project is always a new budget.
    pub fn admit(
        &mut self,
        revision: u64,
        window_s: f64,
        bar_s: Option<f64>,
    ) -> Result<(), String> {
        if self.revision != Some(revision) {
            self.revision = Some(revision);
            self.narrow = 0;
        }
        if !narrow(window_s, bar_s) {
            return Ok(());
        }
        if self.narrow >= NARROW_WINDOW_LIMIT {
            return Err(refusal_message());
        }
        self.narrow += 1;
        Ok(())
    }

    /// A listening clip clears the loop: the model has heard the audio.
    pub fn listened(&mut self, revision: u64) {
        self.revision = Some(revision);
        self.narrow = 0;
    }

    /// Narrow windows admitted at `revision`; zero once the revision moved on.
    #[cfg(test)]
    pub fn narrow_count(&self, revision: u64) -> u32 {
        if self.revision == Some(revision) {
            self.narrow
        } else {
            0
        }
    }
}

/// Narrow means under four seconds *and* under one bar, so a bar-by-bar pass
/// over a long song is never counted whatever the tempo; without a clock the
/// four-second ceiling stands alone.
fn narrow(window_s: f64, bar_s: Option<f64>) -> bool {
    window_s < NARROW_WINDOW_SECONDS && bar_s.is_none_or(|bar| window_s < bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_is_never_narrow_whatever_the_tempo() {
        let mut guard = WindowGuard::default();
        // A bar at 120 bpm is 2 s: under four seconds, but a whole bar.
        for _ in 0..20 {
            guard
                .admit(7, 2.0, Some(2.0))
                .expect("a bar-granularity pass is not a loop");
        }
        assert_eq!(guard.narrow_count(7), 0);
        // Half a bar is a slice of it.
        guard.admit(7, 1.0, Some(2.0)).unwrap();
        assert_eq!(guard.narrow_count(7), 1);
        // Without a clock, four seconds is the whole rule.
        guard.admit(8, 4.0, None).unwrap();
        assert_eq!(guard.narrow_count(8), 0);
        guard.admit(8, 3.9, None).unwrap();
        assert_eq!(guard.narrow_count(8), 1);
    }

    #[test]
    fn the_message_names_the_count_the_ceiling_and_the_skill() {
        let message = refusal_message();
        assert!(
            message.starts_with("narrow-window loop: 8 windows under 4 s"),
            "{message}"
        );
        assert!(
            message.contains("audition_range on the section"),
            "{message}"
        );
        assert!(
            message.contains("analyze_notes to get the notes"),
            "the refusal names the note-level alternative: {message}"
        );
        assert!(message.contains("audition-and-listen skill"), "{message}");
    }
}
