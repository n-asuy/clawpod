use domain::HeartbeatIndicatorType;

use crate::normalize::NormalizeResult;

/// Heartbeat event status used to derive indicator type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatEventStatus {
    OkEmpty,
    OkToken,
    Sent,
    Failed,
    Skipped,
}

/// Resolve the indicator type from event status.
pub fn resolve_indicator_type(status: HeartbeatEventStatus) -> Option<HeartbeatIndicatorType> {
    match status {
        HeartbeatEventStatus::OkEmpty | HeartbeatEventStatus::OkToken => {
            Some(HeartbeatIndicatorType::Ok)
        }
        HeartbeatEventStatus::Sent => Some(HeartbeatIndicatorType::Alert),
        HeartbeatEventStatus::Failed => Some(HeartbeatIndicatorType::Error),
        HeartbeatEventStatus::Skipped => None,
    }
}

/// Derive event status from normalization result and delivery outcome.
pub fn derive_event_status(
    normalized: &NormalizeResult,
    delivered: bool,
    failed: bool,
) -> HeartbeatEventStatus {
    if failed {
        return HeartbeatEventStatus::Failed;
    }
    match normalized {
        NormalizeResult::AckOnly => HeartbeatEventStatus::OkEmpty,
        NormalizeResult::OkWithText(_) => HeartbeatEventStatus::OkToken,
        NormalizeResult::Alert(_) => {
            if delivered {
                HeartbeatEventStatus::Sent
            } else {
                HeartbeatEventStatus::OkToken
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_empty_maps_to_ok() {
        assert_eq!(
            resolve_indicator_type(HeartbeatEventStatus::OkEmpty),
            Some(HeartbeatIndicatorType::Ok)
        );
    }

    #[test]
    fn ok_token_maps_to_ok() {
        assert_eq!(
            resolve_indicator_type(HeartbeatEventStatus::OkToken),
            Some(HeartbeatIndicatorType::Ok)
        );
    }

    #[test]
    fn sent_maps_to_alert() {
        assert_eq!(
            resolve_indicator_type(HeartbeatEventStatus::Sent),
            Some(HeartbeatIndicatorType::Alert)
        );
    }

    #[test]
    fn failed_maps_to_error() {
        assert_eq!(
            resolve_indicator_type(HeartbeatEventStatus::Failed),
            Some(HeartbeatIndicatorType::Error)
        );
    }

    #[test]
    fn skipped_maps_to_none() {
        assert_eq!(resolve_indicator_type(HeartbeatEventStatus::Skipped), None);
    }

    #[test]
    fn derive_ack_only() {
        let status = derive_event_status(&NormalizeResult::AckOnly, false, false);
        assert_eq!(status, HeartbeatEventStatus::OkEmpty);
    }

    #[test]
    fn derive_alert_delivered() {
        let status =
            derive_event_status(&NormalizeResult::Alert("disk full".into()), true, false);
        assert_eq!(status, HeartbeatEventStatus::Sent);
    }

    #[test]
    fn derive_failed_overrides() {
        let status =
            derive_event_status(&NormalizeResult::Alert("disk full".into()), false, true);
        assert_eq!(status, HeartbeatEventStatus::Failed);
    }
}
