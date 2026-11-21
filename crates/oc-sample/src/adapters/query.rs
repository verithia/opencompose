//! Query syntax shared by the adapters whose search speaks a Lucene dialect.
//!
//! User text and tags are data, not query syntax: a `:` in a search box must
//! not select a field, and a `"` in a tag must not close the phrase around it.
//! These helpers make that hold before the adapter percent-encodes the whole
//! value once.

/// Characters a Lucene query parser treats as syntax.
const LUCENE_SPECIALS: &[char] = &[
    '+', '-', '&', '|', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\', '/',
];

/// Escape every Lucene special character so `text` searches as plain words.
/// The bare operators `AND`, `OR` and `NOT` are lowercased, which makes them
/// ordinary terms.
pub(crate) fn lucene_terms(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            if matches!(word, "AND" | "OR" | "NOT") {
                return word.to_ascii_lowercase();
            }
            let mut out = String::with_capacity(word.len());
            for character in word.chars() {
                if LUCENE_SPECIALS.contains(&character) {
                    out.push('\\');
                }
                out.push(character);
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wrap `text` in double quotes as one phrase, escaping the backslashes and
/// quotes inside it.
pub(crate) fn quoted_phrase(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        if matches!(character, '"' | '\\') {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lucene_syntax_in_user_text_becomes_plain_words() {
        assert_eq!(lucene_terms("snare drum"), "snare drum");
        assert_eq!(
            lucene_terms("title:x (a OR b) \"c\" \\ -d"),
            "title\\:x \\(a or b\\) \\\"c\\\" \\\\ \\-d"
        );
        assert_eq!(lucene_terms("NOT café*"), "not café\\*");
    }

    #[test]
    fn a_phrase_escapes_its_quotes_and_backslashes() {
        assert_eq!(quoted_phrase("hip hop"), "\"hip hop\"");
        assert_eq!(quoted_phrase("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
