use chrono::{DateTime, Duration, Utc};
use rand::Rng;

/// Characters used for pairing codes.
/// Excludes ambiguous characters: I, O, L, 0, 1.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// A generated pairing code with expiration.
#[derive(Debug, Clone)]
pub struct PairingCode {
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

/// Generate a pairing code of the given length with a TTL in seconds.
pub fn generate_code(length: usize, ttl_secs: u64) -> PairingCode {
    let mut rng = rand::thread_rng();
    let code: String = (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CODE_ALPHABET.len());
            CODE_ALPHABET[idx] as char
        })
        .collect();
    let expires_at = Utc::now() + Duration::seconds(ttl_secs as i64);
    PairingCode { code, expires_at }
}

/// Constant-time comparison of two byte slices.
/// Prevents timing attacks by always comparing every byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify a provided code against a stored code using constant-time comparison.
/// The provided code is normalized to uppercase and trimmed before comparison.
pub fn verify_code(provided: &str, stored: &str) -> bool {
    let normalized = provided.trim().to_uppercase();
    constant_time_eq(normalized.as_bytes(), stored.as_bytes())
}

/// Check whether a code has expired.
pub fn is_code_expired(expires_at: &DateTime<Utc>) -> bool {
    Utc::now() > *expires_at
}

/// Result of a pairing code verification attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    /// Code was correct; sender is now approved.
    Approved,
    /// Code was wrong.
    InvalidCode,
    /// Code has expired; a new code should be generated.
    Expired,
    /// Too many failed attempts; sender is temporarily locked out.
    LockedOut,
}

/// Check whether a trimmed message looks like a pairing code.
/// Returns true if the message has the expected length and contains
/// only characters from the code alphabet (case-insensitive).
pub fn looks_like_pairing_code(message: &str, expected_length: usize) -> bool {
    let trimmed = message.trim();
    trimmed.len() == expected_length
        && trimmed
            .bytes()
            .all(|b| CODE_ALPHABET.contains(&b.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    // ---- Code generation ----

    #[test]
    fn generated_code_has_correct_length() {
        let pc = generate_code(8, 3600);
        assert_eq!(pc.code.len(), 8);
    }

    #[test]
    fn generated_code_respects_custom_length() {
        let pc = generate_code(12, 3600);
        assert_eq!(pc.code.len(), 12);
    }

    #[test]
    fn generated_code_contains_only_valid_chars() {
        let alphabet: &str = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
        for _ in 0..200 {
            let pc = generate_code(8, 3600);
            for ch in pc.code.chars() {
                assert!(alphabet.contains(ch), "invalid char in code: {ch}");
            }
        }
    }

    #[test]
    fn generated_code_excludes_ambiguous_chars() {
        for _ in 0..500 {
            let pc = generate_code(8, 3600);
            for ch in pc.code.chars() {
                assert_ne!(ch, 'I', "ambiguous char I found");
                assert_ne!(ch, 'O', "ambiguous char O found");
                assert_ne!(ch, 'L', "ambiguous char L found");
                assert_ne!(ch, '0', "ambiguous char 0 found");
                assert_ne!(ch, '1', "ambiguous char 1 found");
            }
        }
    }

    #[test]
    fn generated_codes_are_unique() {
        let a = generate_code(8, 3600);
        let b = generate_code(8, 3600);
        assert_ne!(a.code, b.code);
    }

    #[test]
    fn generated_code_has_future_expiration() {
        let pc = generate_code(8, 3600);
        assert!(pc.expires_at > Utc::now());
    }

    #[test]
    fn generated_code_expiration_matches_ttl() {
        let before = Utc::now();
        let pc = generate_code(8, 60);
        let after = Utc::now();
        let expected_min = before + Duration::seconds(60);
        let expected_max = after + Duration::seconds(60);
        assert!(pc.expires_at >= expected_min);
        assert!(pc.expires_at <= expected_max);
    }

    // ---- Code verification ----

    #[test]
    fn verify_matching_code_returns_true() {
        let pc = generate_code(8, 3600);
        assert!(verify_code(&pc.code, &pc.code));
    }

    #[test]
    fn verify_wrong_code_returns_false() {
        let pc = generate_code(8, 3600);
        assert!(!verify_code("WRONGCDE", &pc.code));
    }

    #[test]
    fn verify_case_insensitive_lowercase_input() {
        let pc = generate_code(8, 3600);
        let lower = pc.code.to_lowercase();
        assert!(verify_code(&lower, &pc.code));
    }

    #[test]
    fn verify_trims_whitespace() {
        let pc = generate_code(8, 3600);
        let padded = format!("  {}  ", pc.code);
        assert!(verify_code(&padded, &pc.code));
    }

    #[test]
    fn verify_empty_strings_match() {
        assert!(verify_code("", ""));
    }

    #[test]
    fn verify_different_lengths_returns_false() {
        assert!(!verify_code("ABC", "ABCD"));
    }

    // ---- Code expiration ----

    #[test]
    fn unexpired_code_is_not_expired() {
        let expires_at = Utc::now() + Duration::seconds(3600);
        assert!(!is_code_expired(&expires_at));
    }

    #[test]
    fn expired_code_is_expired() {
        let expires_at = Utc::now() - Duration::seconds(1);
        assert!(is_code_expired(&expires_at));
    }

    // ---- Constant-time comparison ----

    #[test]
    fn constant_time_eq_identical_values() {
        assert!(constant_time_eq(b"ABCDEFGH", b"ABCDEFGH"));
    }

    #[test]
    fn constant_time_eq_different_values() {
        assert!(!constant_time_eq(b"ABCDEFGH", b"ABCDEFGX"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"ABC", b"ABCD"));
    }

    #[test]
    fn constant_time_eq_empty_slices() {
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_single_bit_difference() {
        assert!(!constant_time_eq(b"A", b"B"));
    }

    // ---- looks_like_pairing_code ----

    #[test]
    fn looks_like_code_accepts_valid_uppercase() {
        assert!(looks_like_pairing_code("ABCD2345", 8));
    }

    #[test]
    fn looks_like_code_accepts_valid_lowercase() {
        assert!(looks_like_pairing_code("abcd2345", 8));
    }

    #[test]
    fn looks_like_code_trims_whitespace() {
        assert!(looks_like_pairing_code("  ABCD2345  ", 8));
    }

    #[test]
    fn looks_like_code_rejects_wrong_length() {
        assert!(!looks_like_pairing_code("ABC", 8));
    }

    #[test]
    fn looks_like_code_rejects_ambiguous_chars() {
        // '0' and '1' are ambiguous and not in the alphabet
        assert!(!looks_like_pairing_code("ABCD0001", 8));
    }

    #[test]
    fn looks_like_code_rejects_regular_message() {
        assert!(!looks_like_pairing_code("hello, how are you?", 8));
    }

    #[test]
    fn looks_like_code_rejects_spaces_in_middle() {
        assert!(!looks_like_pairing_code("ABCD 234", 8));
    }

    // ---- VerifyResult ----

    #[test]
    fn verify_result_equality() {
        assert_eq!(VerifyResult::Approved, VerifyResult::Approved);
        assert_ne!(VerifyResult::Approved, VerifyResult::InvalidCode);
        assert_ne!(VerifyResult::Expired, VerifyResult::LockedOut);
    }
}
