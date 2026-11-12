use ring::rand::{SecureRandom, SystemRandom};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenError {
    EntropyUnavailable,
}

pub(super) fn make_token() -> Result<String, TokenError> {
    make_token_with(|bytes| {
        SystemRandom::new()
            .fill(bytes)
            .map_err(|_| TokenError::EntropyUnavailable)
    })
}

fn make_token_with(
    fill: impl FnOnce(&mut [u8; 32]) -> Result<(), TokenError>,
) -> Result<String, TokenError> {
    let mut bytes = [0; 32];
    fill(&mut bytes)?;
    Ok(oc_market::verify::hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_tokens_keep_the_wire_format_and_use_fresh_entropy() {
        let first = make_token().unwrap();
        let second = make_token().unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
        assert_eq!(
            make_token_with(|bytes| {
                bytes.fill(0xab);
                Ok(())
            })
            .unwrap(),
            "ab".repeat(32)
        );
    }

    #[test]
    fn entropy_failure_cannot_publish_a_partial_or_fallback_token() {
        assert_eq!(
            make_token_with(|bytes| {
                bytes[..8].fill(0xab);
                Err(TokenError::EntropyUnavailable)
            }),
            Err(TokenError::EntropyUnavailable)
        );
    }
}
