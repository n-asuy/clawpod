use domain::ChannelHeartbeatConfig;

/// Resolved visibility policy for a specific delivery channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveVisibility {
    pub show_ok: bool,
    pub show_alerts: bool,
    pub use_indicator: bool,
}

impl EffectiveVisibility {
    /// If all visibility flags are false, the heartbeat run should be
    /// short-circuited before making a model call.
    pub fn should_short_circuit(&self) -> bool {
        !self.show_ok && !self.show_alerts && !self.use_indicator
    }
}

/// Resolve effective visibility by merging per-channel config over channel
/// defaults, falling back to built-in defaults.
pub fn resolve_visibility(
    channel_config: Option<&ChannelHeartbeatConfig>,
    channel_defaults: Option<&ChannelHeartbeatConfig>,
) -> EffectiveVisibility {
    EffectiveVisibility {
        show_ok: pick(
            channel_config.and_then(|c| c.show_ok),
            channel_defaults.and_then(|c| c.show_ok),
            false,
        ),
        show_alerts: pick(
            channel_config.and_then(|c| c.show_alerts),
            channel_defaults.and_then(|c| c.show_alerts),
            true,
        ),
        use_indicator: pick(
            channel_config.and_then(|c| c.use_indicator),
            channel_defaults.and_then(|c| c.use_indicator),
            true,
        ),
    }
}

fn pick(channel: Option<bool>, defaults: Option<bool>, fallback: bool) -> bool {
    channel.or(defaults).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_only() {
        let v = resolve_visibility(None, None);
        assert!(!v.show_ok);
        assert!(v.show_alerts);
        assert!(v.use_indicator);
        assert!(!v.should_short_circuit());
    }

    #[test]
    fn channel_defaults_override_builtins() {
        let defaults = ChannelHeartbeatConfig {
            show_ok: Some(true),
            show_alerts: Some(false),
            use_indicator: Some(false),
        };
        let v = resolve_visibility(None, Some(&defaults));
        assert!(v.show_ok);
        assert!(!v.show_alerts);
        assert!(!v.use_indicator);
    }

    #[test]
    fn channel_config_overrides_defaults() {
        let defaults = ChannelHeartbeatConfig {
            show_ok: Some(false),
            show_alerts: Some(true),
            use_indicator: Some(true),
        };
        let channel = ChannelHeartbeatConfig {
            show_ok: Some(true),
            show_alerts: None,
            use_indicator: None,
        };
        let v = resolve_visibility(Some(&channel), Some(&defaults));
        assert!(v.show_ok);
        assert!(v.show_alerts); // falls through from defaults
        assert!(v.use_indicator); // falls through from defaults
    }

    #[test]
    fn all_false_triggers_short_circuit() {
        let cfg = ChannelHeartbeatConfig {
            show_ok: Some(false),
            show_alerts: Some(false),
            use_indicator: Some(false),
        };
        let v = resolve_visibility(Some(&cfg), None);
        assert!(v.should_short_circuit());
    }
}
