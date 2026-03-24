use std::time::Duration;

use anyhow::Result;
use config::parse_duration_str;
use domain::{AgentHeartbeatConfig, HeartbeatDirectPolicy, HeartbeatTarget, ActiveHoursConfig};

/// Default heartbeat prompt used when no explicit override is configured.
pub const DEFAULT_HEARTBEAT_PROMPT: &str = "\
1. Read `memory/reflections.md` for your open questions and hypotheses.\n\
2. Review `focus.md` for your current tasks — update status if anything changed.\n\
3. If you have new insights, record them in `memory/reflections.md` under Insights.\n\
4. Seed `memory/reflections.md` > Next Cycle Seeds with topics for your next heartbeat.\n\
5. If anything needs the user's attention, report it clearly.\n\
6. If nothing needs attention, reply HEARTBEAT_OK.";

const DEFAULT_EVERY: &str = "30m";
const DEFAULT_ACK_MAX_CHARS: usize = 300;

/// Fully resolved heartbeat policy for a single agent, with all defaults applied.
#[derive(Debug, Clone)]
pub struct EffectiveHeartbeatPolicy {
    pub every: Duration,
    pub model: Option<String>,
    pub prompt: String,
    pub target: HeartbeatTarget,
    pub to: Option<String>,
    pub account_id: Option<String>,
    pub ack_max_chars: usize,
    pub direct_policy: HeartbeatDirectPolicy,
    pub include_reasoning: bool,
    pub light_context: bool,
    pub isolated_session: bool,
    pub active_hours: Option<ActiveHoursConfig>,
}

/// Merge agent-level overrides on top of agent_defaults, falling back to
/// hardcoded defaults for any unset field.
pub fn resolve_effective_policy(
    agent_defaults: Option<&AgentHeartbeatConfig>,
    agent_config: Option<&AgentHeartbeatConfig>,
) -> Result<EffectiveHeartbeatPolicy> {
    let every_str = pick_str(
        agent_config.and_then(|c| c.every.as_deref()),
        agent_defaults.and_then(|c| c.every.as_deref()),
        DEFAULT_EVERY,
    );
    let every = parse_duration_str(every_str)?;

    Ok(EffectiveHeartbeatPolicy {
        every,
        model: pick_opt_string(
            agent_config.and_then(|c| c.model.as_deref()),
            agent_defaults.and_then(|c| c.model.as_deref()),
        ),
        prompt: pick_str(
            agent_config.and_then(|c| c.prompt.as_deref()),
            agent_defaults.and_then(|c| c.prompt.as_deref()),
            DEFAULT_HEARTBEAT_PROMPT,
        )
        .to_string(),
        target: pick_copy(
            agent_config.and_then(|c| c.target),
            agent_defaults.and_then(|c| c.target),
            HeartbeatTarget::None,
        ),
        to: pick_opt_string(
            agent_config.and_then(|c| c.to.as_deref()),
            agent_defaults.and_then(|c| c.to.as_deref()),
        ),
        account_id: pick_opt_string(
            agent_config.and_then(|c| c.account_id.as_deref()),
            agent_defaults.and_then(|c| c.account_id.as_deref()),
        ),
        ack_max_chars: pick_copy(
            agent_config.and_then(|c| c.ack_max_chars),
            agent_defaults.and_then(|c| c.ack_max_chars),
            DEFAULT_ACK_MAX_CHARS,
        ),
        direct_policy: pick_copy(
            agent_config.and_then(|c| c.direct_policy),
            agent_defaults.and_then(|c| c.direct_policy),
            HeartbeatDirectPolicy::Allow,
        ),
        include_reasoning: pick_copy(
            agent_config.and_then(|c| c.include_reasoning),
            agent_defaults.and_then(|c| c.include_reasoning),
            false,
        ),
        light_context: pick_copy(
            agent_config.and_then(|c| c.light_context),
            agent_defaults.and_then(|c| c.light_context),
            false,
        ),
        isolated_session: pick_copy(
            agent_config.and_then(|c| c.isolated_session),
            agent_defaults.and_then(|c| c.isolated_session),
            true,
        ),
        active_hours: agent_config
            .and_then(|c| c.active_hours.clone())
            .or_else(|| agent_defaults.and_then(|c| c.active_hours.clone())),
    })
}

fn pick_str<'a>(agent: Option<&'a str>, defaults: Option<&'a str>, fallback: &'a str) -> &'a str {
    agent.or(defaults).unwrap_or(fallback)
}

fn pick_opt_string(agent: Option<&str>, defaults: Option<&str>) -> Option<String> {
    agent.or(defaults).map(ToString::to_string)
}

fn pick_copy<T: Copy>(agent: Option<T>, defaults: Option<T>, fallback: T) -> T {
    agent.or(defaults).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_only() {
        let policy = resolve_effective_policy(None, None).unwrap();
        assert_eq!(policy.every, Duration::from_secs(1800));
        assert_eq!(policy.target, HeartbeatTarget::None);
        assert_eq!(policy.ack_max_chars, 300);
        assert_eq!(policy.direct_policy, HeartbeatDirectPolicy::Allow);
        assert!(!policy.light_context);
        assert!(policy.isolated_session, "isolated_session defaults to true to avoid main session context pollution");
        assert!(policy.prompt.contains("HEARTBEAT_OK"));
    }

    #[test]
    fn agent_defaults_override_hardcoded() {
        let defaults = AgentHeartbeatConfig {
            every: Some("1h".into()),
            target: Some(HeartbeatTarget::Last),
            ack_max_chars: Some(500),
            ..Default::default()
        };
        let policy = resolve_effective_policy(Some(&defaults), None).unwrap();
        assert_eq!(policy.every, Duration::from_secs(3600));
        assert_eq!(policy.target, HeartbeatTarget::Last);
        assert_eq!(policy.ack_max_chars, 500);
    }

    #[test]
    fn agent_config_overrides_defaults() {
        let defaults = AgentHeartbeatConfig {
            every: Some("1h".into()),
            target: Some(HeartbeatTarget::None),
            ..Default::default()
        };
        let agent = AgentHeartbeatConfig {
            target: Some(HeartbeatTarget::Telegram),
            ack_max_chars: Some(100),
            ..Default::default()
        };
        let policy = resolve_effective_policy(Some(&defaults), Some(&agent)).unwrap();
        // every falls through from defaults
        assert_eq!(policy.every, Duration::from_secs(3600));
        // target overridden by agent
        assert_eq!(policy.target, HeartbeatTarget::Telegram);
        // ack_max_chars overridden by agent
        assert_eq!(policy.ack_max_chars, 100);
    }

    #[test]
    fn agent_config_only() {
        let agent = AgentHeartbeatConfig {
            every: Some("5m".into()),
            light_context: Some(true),
            isolated_session: Some(true),
            ..Default::default()
        };
        let policy = resolve_effective_policy(None, Some(&agent)).unwrap();
        assert_eq!(policy.every, Duration::from_secs(300));
        assert!(policy.light_context);
        assert!(policy.isolated_session);
    }
}
