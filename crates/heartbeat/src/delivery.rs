use std::collections::HashMap;

use domain::{HeartbeatDirectPolicy, HeartbeatTarget, TeamConfig};
use store::SessionSummary;

use crate::policy::EffectiveHeartbeatPolicy;

/// Resolved delivery target for a heartbeat run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTarget {
    pub channel: String,
    pub recipient: String,
    pub account_id: Option<String>,
}

/// Discriminated delivery strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryKind {
    /// Deliver via outgoing message queue to an external channel.
    Outbound(DeliveryTarget),
    /// Deliver to the agent's team chatroom.
    Chatroom { team_id: String },
}

/// Resolve where to deliver heartbeat output based on policy and session state.
pub fn resolve_delivery_target(
    policy: &EffectiveHeartbeatPolicy,
    session: Option<&SessionSummary>,
    agent_id: &str,
    teams: &HashMap<String, TeamConfig>,
) -> Option<DeliveryKind> {
    match policy.target {
        HeartbeatTarget::None => None,

        HeartbeatTarget::Last => {
            let session = session?;
            let channel = session.last_channel.as_deref()?;
            let peer_id = session.last_peer_id.as_deref()?;

            let target = DeliveryTarget {
                channel: channel.to_string(),
                recipient: policy.to.clone().unwrap_or_else(|| peer_id.to_string()),
                account_id: policy
                    .account_id
                    .clone()
                    .or_else(|| session.last_account_id.clone()),
            };

            if should_block_dm(policy, session) {
                return None;
            }

            Some(DeliveryKind::Outbound(target))
        }

        HeartbeatTarget::Telegram | HeartbeatTarget::Discord | HeartbeatTarget::Slack => {
            let recipient = policy.to.as_ref()?;
            let channel = match policy.target {
                HeartbeatTarget::Telegram => "telegram",
                HeartbeatTarget::Discord => "discord",
                HeartbeatTarget::Slack => "slack",
                _ => unreachable!(),
            };

            Some(DeliveryKind::Outbound(DeliveryTarget {
                channel: channel.to_string(),
                recipient: recipient.clone(),
                account_id: policy.account_id.clone(),
            }))
        }

        HeartbeatTarget::Chatroom => {
            let team_id = routing::find_team_for_agent(agent_id, teams)?;
            Some(DeliveryKind::Chatroom { team_id })
        }
    }
}

fn should_block_dm(policy: &EffectiveHeartbeatPolicy, session: &SessionSummary) -> bool {
    if policy.direct_policy != HeartbeatDirectPolicy::Block {
        return false;
    }
    session
        .last_chat_type
        .as_deref()
        .is_none_or(|t| t == "direct")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(channel: &str, peer_id: &str) -> SessionSummary {
        SessionSummary {
            session_key: "agent:default:main".into(),
            agent_id: "default".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            last_channel: Some(channel.into()),
            last_peer_id: Some(peer_id.into()),
            last_account_id: None,
            last_chat_type: Some("direct".into()),
            last_sender_id: Some("U123".into()),
            last_heartbeat_text: None,
            last_heartbeat_sent_at: None,
        }
    }

    fn base_policy(target: HeartbeatTarget) -> EffectiveHeartbeatPolicy {
        EffectiveHeartbeatPolicy {
            every: std::time::Duration::from_secs(1800),
            model: None,
            prompt: "check".into(),
            target,
            to: None,
            account_id: None,
            ack_max_chars: 300,
            direct_policy: HeartbeatDirectPolicy::Allow,
            include_reasoning: false,
            light_context: false,
            isolated_session: false,
            active_hours: None,
        }
    }

    fn empty_teams() -> HashMap<String, TeamConfig> {
        HashMap::new()
    }

    fn teams_with(agent_id: &str) -> HashMap<String, TeamConfig> {
        let mut teams = HashMap::new();
        teams.insert(
            "dev".to_string(),
            TeamConfig {
                name: "Dev".to_string(),
                agents: vec![agent_id.to_string(), "other".to_string()],
                leader_agent: agent_id.to_string(),
            },
        );
        teams
    }

    #[test]
    fn target_none_returns_none() {
        let policy = base_policy(HeartbeatTarget::None);
        assert!(resolve_delivery_target(&policy, None, "default", &empty_teams()).is_none());
    }

    #[test]
    fn target_last_resolves_from_session() {
        let policy = base_policy(HeartbeatTarget::Last);
        let session = make_session("telegram", "C456");
        let kind =
            resolve_delivery_target(&policy, Some(&session), "default", &empty_teams()).unwrap();
        match kind {
            DeliveryKind::Outbound(t) => {
                assert_eq!(t.channel, "telegram");
                assert_eq!(t.recipient, "C456");
            }
            _ => panic!("expected Outbound"),
        }
    }

    #[test]
    fn target_last_no_session_returns_none() {
        let policy = base_policy(HeartbeatTarget::Last);
        assert!(resolve_delivery_target(&policy, None, "default", &empty_teams()).is_none());
    }

    #[test]
    fn target_last_empty_session_returns_none() {
        let policy = base_policy(HeartbeatTarget::Last);
        let session = SessionSummary {
            session_key: "k".into(),
            agent_id: "a".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            last_channel: None,
            last_peer_id: None,
            last_account_id: None,
            last_chat_type: None,
            last_sender_id: None,
            last_heartbeat_text: None,
            last_heartbeat_sent_at: None,
        };
        assert!(
            resolve_delivery_target(&policy, Some(&session), "default", &empty_teams()).is_none()
        );
    }

    #[test]
    fn target_explicit_channel() {
        let mut policy = base_policy(HeartbeatTarget::Telegram);
        policy.to = Some("U789".into());
        let kind = resolve_delivery_target(&policy, None, "default", &empty_teams()).unwrap();
        match kind {
            DeliveryKind::Outbound(t) => {
                assert_eq!(t.channel, "telegram");
                assert_eq!(t.recipient, "U789");
            }
            _ => panic!("expected Outbound"),
        }
    }

    #[test]
    fn target_explicit_channel_no_to_returns_none() {
        let policy = base_policy(HeartbeatTarget::Slack);
        assert!(resolve_delivery_target(&policy, None, "default", &empty_teams()).is_none());
    }

    #[test]
    fn direct_policy_block_suppresses_dm() {
        let mut policy = base_policy(HeartbeatTarget::Last);
        policy.direct_policy = HeartbeatDirectPolicy::Block;
        let session = make_session("telegram", "C456");
        assert!(
            resolve_delivery_target(&policy, Some(&session), "default", &empty_teams()).is_none()
        );
    }

    #[test]
    fn direct_policy_block_allows_group() {
        let mut policy = base_policy(HeartbeatTarget::Last);
        policy.direct_policy = HeartbeatDirectPolicy::Block;
        let mut session = make_session("telegram", "C456");
        session.last_chat_type = Some("group".into());
        let kind =
            resolve_delivery_target(&policy, Some(&session), "default", &empty_teams());
        assert!(kind.is_some());
    }

    #[test]
    fn target_chatroom_resolves_team() {
        let policy = base_policy(HeartbeatTarget::Chatroom);
        let teams = teams_with("default");
        let kind = resolve_delivery_target(&policy, None, "default", &teams).unwrap();
        assert_eq!(kind, DeliveryKind::Chatroom { team_id: "dev".to_string() });
    }

    #[test]
    fn target_chatroom_no_team_returns_none() {
        let policy = base_policy(HeartbeatTarget::Chatroom);
        assert!(resolve_delivery_target(&policy, None, "default", &empty_teams()).is_none());
    }
}
