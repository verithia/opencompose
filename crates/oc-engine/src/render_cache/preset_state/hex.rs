//! State hex is bounded before allocation and decoded by bytes so malformed
//! non-ASCII text cannot panic at a UTF-8 character boundary.

use super::MAX_STATE_BYTES;
const MAX_TEXT_BYTES: usize = MAX_STATE_BYTES * 4;

pub(super) fn decode(format: &str, text: &str) -> Result<Vec<u8>, String> {
    decode_limited(format, text, MAX_TEXT_BYTES, MAX_STATE_BYTES)
}
fn decode_limited(
    format: &str,
    text: &str,
    text_limit: usize,
    decoded_limit: usize,
) -> Result<Vec<u8>, String> {
    if text.len() > text_limit {
        return Err(format!(
            "{format} state hex exceeds the {text_limit}-byte encoded limit"
        ));
    }
    let count = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .count();
    if !count.is_multiple_of(2) {
        return Err(format!("invalid {format} state hex: odd number of digits"));
    }
    if count / 2 > decoded_limit {
        return Err(format!(
            "{format} state hex exceeds the {decoded_limit}-byte decoded limit"
        ));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count / 2)
        .map_err(|error| format!("could not allocate bounded {format} state: {error}"))?;
    let mut digits = text.bytes().filter(|byte| !byte.is_ascii_whitespace());
    while let Some(high) = digits.next() {
        let low = digits.next().expect("the bounded digit count is even");
        let value = digit(high)
            .zip(digit(low))
            .map(|(high, low)| (high << 4) | low)
            .ok_or_else(|| {
                format!(
                    "invalid {format} state hex at byte {}: invalid digit",
                    output.len()
                )
            })?;
        output.push(value);
    }
    Ok(output)
}
fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_hex_enforces_encoded_and_decoded_budgets_and_unicode_safety() {
        assert_eq!(decode_limited("VST3", "0a FF\n", 6, 2).unwrap(), [10, 255]);
        assert!(
            decode_limited("VST3", "     ", 4, 2)
                .unwrap_err()
                .contains("encoded limit")
        );
        assert!(
            decode_limited("VST3", "000102", 8, 2)
                .unwrap_err()
                .contains("decoded limit")
        );
        assert!(
            decode_limited("VST3", "é", 8, 2)
                .unwrap_err()
                .contains("at byte 0")
        );
        assert!(
            decode_limited("VST3", "0", 8, 2)
                .unwrap_err()
                .contains("odd number")
        );
    }
}
