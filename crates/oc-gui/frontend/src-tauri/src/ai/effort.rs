//! What a saved thinking level means on each wire.
//!
//! The catalog decides which models declare levels; this module decides how a
//! level becomes a request field, so the adapters cannot drift apart. Every
//! adapter sends the saved level and never a literal.

use super::AiProviderEntry;

/// Levels a reasoning model accepts, in increasing effort. The catalog lists
/// the subset a model supports; an unknown level falls back to the middle one
/// rather than failing a request.
pub const LEVELS: &[&str] = &["minimal", "low", "medium", "high"];

/// The effort string a wire takes: the level itself when it is one we know,
/// otherwise `medium`.
pub fn effort(level: &str) -> &str {
    LEVELS
        .iter()
        .find(|known| **known == level)
        .copied()
        .unwrap_or("medium")
}

/// Anthropic's thinking budget for a level, in tokens.
///
/// The API requires `budget_tokens < max_tokens`, so the budget never takes
/// more than half the output allowance: a small allowance still gets a thinking
/// block that fits, and an allowance below [`MIN_MAX_TOKENS`] gets none at all
/// rather than a rejected request.
pub fn anthropic_budget(level: &str, max_tokens: u64) -> Option<u64> {
    if max_tokens < MIN_MAX_TOKENS {
        return None;
    }
    let wanted = match effort(level) {
        "minimal" | "low" => 1_024,
        "high" => 8_192,
        _ => 4_096,
    };
    Some(wanted.min(max_tokens / 2).max(1_024))
}

/// Output allowance below which no thinking budget is requested.
pub const MIN_MAX_TOKENS: u64 = 2_048;

/// True when the entry's model declares thinking levels, which is the same
/// question the panel asks before showing the selector.
pub fn supported(entry: &AiProviderEntry) -> bool {
    !super::catalog::thinking_levels(entry.wire(), &entry.model).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_level_is_sent_as_it_is_and_an_unknown_one_falls_back() {
        assert_eq!(effort("high"), "high");
        assert_eq!(effort("low"), "low");
        assert_eq!(effort("maximum"), "medium");
        assert_eq!(effort(""), "medium");
    }

    #[test]
    fn the_anthropic_budget_grows_with_the_level_and_fits_the_reply() {
        assert_eq!(anthropic_budget("low", 32_768), Some(1_024));
        assert_eq!(anthropic_budget("medium", 32_768), Some(4_096));
        assert_eq!(anthropic_budget("high", 32_768), Some(8_192));
        // A smaller allowance halves the budget instead of being rejected.
        assert_eq!(anthropic_budget("high", 8_192), Some(4_096));
        assert_eq!(anthropic_budget("high", 4_096), Some(2_048));
        // Below the floor no thinking block is requested at all.
        assert_eq!(anthropic_budget("low", 2_047), None);
    }

    /// The selector offers exactly what the catalog declares, so every level a
    /// model can be saved with has to survive this table. A new level in the
    /// catalog without a line here would be silently sent as `medium`.
    #[test]
    fn every_level_the_bundled_catalog_declares_is_sent_as_it_is() {
        let mut seen = 0;
        for provider in &super::super::catalog::catalog().providers {
            for model in &provider.models {
                for level in &model.thinking_levels {
                    seen += 1;
                    assert_eq!(
                        effort(level),
                        level,
                        "{} declares \"{level}\", which effort() rewrites",
                        model.id
                    );
                }
            }
        }
        assert!(seen > 0, "the catalog declares no thinking levels");
    }

    #[test]
    fn a_minimal_level_asks_for_the_smallest_thinking_budget() {
        assert_eq!(anthropic_budget("minimal", 32_768), Some(1_024));
    }
}
