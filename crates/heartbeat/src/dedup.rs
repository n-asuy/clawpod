use store::SessionSummary;

const DEDUP_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Check whether the given text is a duplicate of the last heartbeat delivery
/// within the 24-hour suppression window.
pub fn is_duplicate_heartbeat(
    session: Option<&SessionSummary>,
    current_text: &str,
    now_rfc3339: &str,
) -> bool {
    let Some(session) = session else {
        return false;
    };
    let Some(prev_text) = session.last_heartbeat_text.as_deref() else {
        return false;
    };
    let Some(prev_sent_at) = session.last_heartbeat_sent_at.as_deref() else {
        return false;
    };

    if prev_text != current_text {
        return false;
    }

    // Check time window
    let Ok(prev_dt) = chrono::DateTime::parse_from_rfc3339(prev_sent_at) else {
        return false;
    };
    let Ok(now_dt) = chrono::DateTime::parse_from_rfc3339(now_rfc3339) else {
        return false;
    };

    let elapsed = now_dt
        .signed_duration_since(prev_dt)
        .num_seconds()
        .abs();
    elapsed < DEDUP_WINDOW_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(text: Option<&str>, sent_at: Option<&str>) -> SessionSummary {
        SessionSummary {
            session_key: "k".into(),
            agent_id: "a".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            last_channel: None,
            last_peer_id: None,
            last_account_id: None,
            last_chat_type: None,
            last_sender_id: None,
            last_heartbeat_text: text.map(ToString::to_string),
            last_heartbeat_sent_at: sent_at.map(ToString::to_string),
        }
    }

    #[test]
    fn no_session_not_duplicate() {
        assert!(!is_duplicate_heartbeat(None, "hello", "2026-01-01T12:00:00Z"));
    }

    #[test]
    fn no_previous_text_not_duplicate() {
        let session = make_session(None, None);
        assert!(!is_duplicate_heartbeat(
            Some(&session),
            "hello",
            "2026-01-01T12:00:00Z"
        ));
    }

    #[test]
    fn same_text_within_window_is_duplicate() {
        let session = make_session(
            Some("Disk at 90%"),
            Some("2026-01-01T11:00:00Z"),
        );
        assert!(is_duplicate_heartbeat(
            Some(&session),
            "Disk at 90%",
            "2026-01-01T12:00:00Z"
        ));
    }

    #[test]
    fn same_text_outside_window_not_duplicate() {
        let session = make_session(
            Some("Disk at 90%"),
            Some("2025-12-30T00:00:00Z"),
        );
        assert!(!is_duplicate_heartbeat(
            Some(&session),
            "Disk at 90%",
            "2026-01-01T12:00:00Z"
        ));
    }

    #[test]
    fn different_text_not_duplicate() {
        let session = make_session(
            Some("Disk at 90%"),
            Some("2026-01-01T11:00:00Z"),
        );
        assert!(!is_duplicate_heartbeat(
            Some(&session),
            "Disk at 95%",
            "2026-01-01T12:00:00Z"
        ));
    }
}
