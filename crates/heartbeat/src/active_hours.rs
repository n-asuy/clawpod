use chrono::{NaiveTime, Timelike, Utc};
use chrono_tz::Tz;
use domain::ActiveHoursConfig;

/// Check whether the current time falls within the configured active-hours window.
/// Returns `true` if active hours are not configured (always active).
pub fn is_within_active_hours(config: Option<&ActiveHoursConfig>) -> bool {
    let Some(cfg) = config else {
        return true; // No active hours = always active
    };

    let start = match NaiveTime::parse_from_str(&cfg.start, "%H:%M") {
        Ok(t) => t,
        Err(_) => return true, // Invalid start = safe fallback
    };
    let end = match NaiveTime::parse_from_str(&cfg.end, "%H:%M") {
        Ok(t) => t,
        Err(_) => return true, // Invalid end = safe fallback
    };

    if start == end {
        return false; // Zero-width window = always outside
    }

    let now_in_tz = resolve_current_time(cfg.timezone.as_deref());
    let current = NaiveTime::from_hms_opt(now_in_tz.hour(), now_in_tz.minute(), 0)
        .unwrap_or(NaiveTime::from_hms_opt(0, 0, 0).unwrap());

    if end > start {
        // Normal window: e.g. 09:00 to 18:00
        current >= start && current < end
    } else {
        // Overnight window: e.g. 22:00 to 06:00
        current >= start || current < end
    }
}

/// Resolve the current time in the configured timezone.
fn resolve_current_time(timezone: Option<&str>) -> chrono::DateTime<Tz> {
    let tz: Tz = timezone
        .and_then(|s| s.parse::<Tz>().ok())
        .unwrap_or(chrono_tz::UTC);
    Utc::now().with_timezone(&tz)
}

/// Testable version that accepts an explicit current time.
pub fn is_within_active_hours_at(
    config: Option<&ActiveHoursConfig>,
    hour: u32,
    minute: u32,
) -> bool {
    let Some(cfg) = config else {
        return true;
    };

    let start = match NaiveTime::parse_from_str(&cfg.start, "%H:%M") {
        Ok(t) => t,
        Err(_) => return true,
    };
    let end = match NaiveTime::parse_from_str(&cfg.end, "%H:%M") {
        Ok(t) => t,
        Err(_) => return true,
    };

    if start == end {
        return false;
    }

    let current = NaiveTime::from_hms_opt(hour, minute, 0)
        .unwrap_or(NaiveTime::from_hms_opt(0, 0, 0).unwrap());

    if end > start {
        current >= start && current < end
    } else {
        current >= start || current < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(start: &str, end: &str) -> ActiveHoursConfig {
        ActiveHoursConfig {
            start: start.to_string(),
            end: end.to_string(),
            timezone: None,
        }
    }

    #[test]
    fn no_config_always_active() {
        assert!(is_within_active_hours_at(None, 3, 0));
        assert!(is_within_active_hours_at(None, 12, 0));
    }

    #[test]
    fn normal_window_inside() {
        let cfg = hours("09:00", "18:00");
        assert!(is_within_active_hours_at(Some(&cfg), 9, 0));
        assert!(is_within_active_hours_at(Some(&cfg), 12, 30));
        assert!(is_within_active_hours_at(Some(&cfg), 17, 59));
    }

    #[test]
    fn normal_window_outside() {
        let cfg = hours("09:00", "18:00");
        assert!(!is_within_active_hours_at(Some(&cfg), 8, 59));
        assert!(!is_within_active_hours_at(Some(&cfg), 18, 0));
        assert!(!is_within_active_hours_at(Some(&cfg), 23, 0));
    }

    #[test]
    fn overnight_window_inside() {
        let cfg = hours("22:00", "06:00");
        assert!(is_within_active_hours_at(Some(&cfg), 22, 0));
        assert!(is_within_active_hours_at(Some(&cfg), 23, 30));
        assert!(is_within_active_hours_at(Some(&cfg), 2, 0));
        assert!(is_within_active_hours_at(Some(&cfg), 5, 59));
    }

    #[test]
    fn overnight_window_outside() {
        let cfg = hours("22:00", "06:00");
        assert!(!is_within_active_hours_at(Some(&cfg), 6, 0));
        assert!(!is_within_active_hours_at(Some(&cfg), 12, 0));
        assert!(!is_within_active_hours_at(Some(&cfg), 21, 59));
    }

    #[test]
    fn zero_width_window_always_outside() {
        let cfg = hours("12:00", "12:00");
        assert!(!is_within_active_hours_at(Some(&cfg), 12, 0));
        assert!(!is_within_active_hours_at(Some(&cfg), 0, 0));
    }

    #[test]
    fn invalid_format_defaults_active() {
        let cfg = ActiveHoursConfig {
            start: "invalid".to_string(),
            end: "18:00".to_string(),
            timezone: None,
        };
        assert!(is_within_active_hours_at(Some(&cfg), 3, 0));
    }
}
