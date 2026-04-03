/// Result of normalizing a heartbeat response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeResult {
    /// Only HEARTBEAT_OK, no meaningful content.
    AckOnly,
    /// HEARTBEAT_OK was stripped but remaining text is within the ack threshold.
    OkWithText(String),
    /// Non-trivial output: either no HEARTBEAT_OK at boundaries, or remaining
    /// text exceeds `ack_max_chars`.
    Alert(String),
}

const TOKEN: &str = "HEARTBEAT_OK";

/// Strip markdown wrappers (`**`, `` ` ``, `_`, `~`, `*`) from edges.
fn strip_markdown_wrappers(s: &str) -> &str {
    let mut s = s.trim();
    for wrapper in ["```", "**", "__", "~~", "`", "*", "_"] {
        if s.starts_with(wrapper) && s.ends_with(wrapper) && s.len() > 2 * wrapper.len() {
            s = s[wrapper.len()..s.len() - wrapper.len()].trim();
        }
    }
    s
}

/// Normalize a heartbeat response by stripping HEARTBEAT_OK tokens from
/// start/end boundaries and applying `ack_max_chars` suppression.
pub fn normalize_heartbeat_output(output: &str, ack_max_chars: usize) -> NormalizeResult {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return NormalizeResult::AckOnly;
    }

    let unwrapped = strip_markdown_wrappers(trimmed);

    // Check for pure HEARTBEAT_OK (with or without markdown)
    if unwrapped == TOKEN {
        return NormalizeResult::AckOnly;
    }

    // Try stripping at start
    let after_start = if let Some(rest) = unwrapped.strip_prefix(TOKEN) {
        rest.trim_start_matches(['.', '!', ',', ';', ':', '-'])
            .trim()
    } else {
        unwrapped
    };

    // Try stripping at end
    let stripped = if let Some(rest) = after_start.strip_suffix(TOKEN) {
        rest.trim_end_matches(['.', '!', ',', ';', ':', '-']).trim()
    } else {
        after_start
    };

    // If nothing was actually stripped, HEARTBEAT_OK is in the middle only
    if stripped == unwrapped {
        // Check if it contains HEARTBEAT_OK at all — if in middle only, treat as alert
        return NormalizeResult::Alert(trimmed.to_string());
    }

    // Something was stripped
    if stripped.is_empty() {
        return NormalizeResult::AckOnly;
    }

    if stripped.len() <= ack_max_chars {
        NormalizeResult::OkWithText(stripped.to_string())
    } else {
        NormalizeResult::Alert(stripped.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_heartbeat_ok() {
        assert_eq!(
            normalize_heartbeat_output("HEARTBEAT_OK", 300),
            NormalizeResult::AckOnly
        );
    }

    #[test]
    fn heartbeat_ok_with_whitespace() {
        assert_eq!(
            normalize_heartbeat_output("  HEARTBEAT_OK  ", 300),
            NormalizeResult::AckOnly
        );
    }

    #[test]
    fn heartbeat_ok_markdown_wrapped() {
        assert_eq!(
            normalize_heartbeat_output("**HEARTBEAT_OK**", 300),
            NormalizeResult::AckOnly
        );
        assert_eq!(
            normalize_heartbeat_output("`HEARTBEAT_OK`", 300),
            NormalizeResult::AckOnly
        );
        assert_eq!(
            normalize_heartbeat_output("```\nHEARTBEAT_OK\n```", 300),
            NormalizeResult::AckOnly
        );
    }

    #[test]
    fn heartbeat_ok_at_start_with_text() {
        let result = normalize_heartbeat_output("HEARTBEAT_OK\nAll clear, nothing to report.", 300);
        assert_eq!(
            result,
            NormalizeResult::OkWithText("All clear, nothing to report.".to_string())
        );
    }

    #[test]
    fn heartbeat_ok_at_end_with_text() {
        let result = normalize_heartbeat_output("Alert: disk full\nHEARTBEAT_OK", 300);
        assert_eq!(
            result,
            NormalizeResult::OkWithText("Alert: disk full".to_string())
        );
    }

    #[test]
    fn heartbeat_ok_in_middle_is_alert() {
        let result =
            normalize_heartbeat_output("Status is HEARTBEAT_OK but there are warnings", 300);
        assert_eq!(
            result,
            NormalizeResult::Alert("Status is HEARTBEAT_OK but there are warnings".to_string())
        );
    }

    #[test]
    fn text_exceeding_ack_max_chars_is_alert() {
        let long_text = format!("HEARTBEAT_OK\n{}", "x".repeat(400));
        let result = normalize_heartbeat_output(&long_text, 300);
        assert!(matches!(result, NormalizeResult::Alert(_)));
    }

    #[test]
    fn empty_output_is_ack_only() {
        assert_eq!(
            normalize_heartbeat_output("", 300),
            NormalizeResult::AckOnly
        );
        assert_eq!(
            normalize_heartbeat_output("   ", 300),
            NormalizeResult::AckOnly
        );
    }

    #[test]
    fn no_heartbeat_ok_is_alert() {
        let result = normalize_heartbeat_output("Disk usage at 95%, action needed", 300);
        assert_eq!(
            result,
            NormalizeResult::Alert("Disk usage at 95%, action needed".to_string())
        );
    }
}
