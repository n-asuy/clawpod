use domain::{ChatType, DmScope, InboundEvent};

pub fn build_session_key(
    agent_id: &str,
    event: &InboundEvent,
    dm_scope: DmScope,
    main_key: &str,
) -> String {
    match event.chat_type {
        ChatType::Direct => build_direct_session_key(agent_id, event, dm_scope, main_key),
        ChatType::Group => format!("agent:{agent_id}:{}:group:{}", event.channel, event.peer_id),
        ChatType::Thread => format!(
            "agent:{agent_id}:{}:thread:{}",
            event.channel, event.peer_id
        ),
    }
}

fn build_direct_session_key(
    agent_id: &str,
    event: &InboundEvent,
    dm_scope: DmScope,
    main_key: &str,
) -> String {
    let account_id = event
        .account_id
        .clone()
        .unwrap_or_else(|| "default".to_string());

    match dm_scope {
        DmScope::Main => format!("agent:{agent_id}:{main_key}"),
        DmScope::PerPeer => format!("agent:{agent_id}:direct:{}", event.sender_id),
        DmScope::PerChannelPeer => {
            format!(
                "agent:{agent_id}:{}:direct:{}",
                event.channel, event.sender_id
            )
        }
        DmScope::PerAccountChannelPeer => {
            format!(
                "agent:{agent_id}:{}:{account_id}:direct:{}",
                event.channel, event.sender_id
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use domain::{ChatType, InboundEvent};

    use super::*;

    fn sample_event() -> InboundEvent {
        InboundEvent {
            message_id: "m1".to_string(),
            channel: "telegram".to_string(),
            sender: "alice".to_string(),
            sender_id: "alice_1".to_string(),
            text: "hello".to_string(),
            timestamp: Utc::now(),
            chat_type: ChatType::Direct,
            peer_id: "alice_1".to_string(),
            account_id: Some("work".to_string()),
            files: vec![],
            pre_routed_agent: None,
            from_agent: None,
            chain_depth: 0,
        }
    }

    #[test]
    fn builds_per_channel_peer_key() {
        let event = sample_event();
        let key = build_session_key("default", &event, DmScope::PerChannelPeer, "main");
        assert_eq!(key, "agent:default:telegram:direct:alice_1");
    }

    #[test]
    fn builds_main_key() {
        let event = sample_event();
        let key = build_session_key("default", &event, DmScope::Main, "main");
        assert_eq!(key, "agent:default:main");
    }
}
