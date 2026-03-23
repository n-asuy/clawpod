use std::collections::{HashMap, HashSet};

use domain::{AgentConfig, BindingRule, InboundEvent, MentionHandoff, RouteDecision, TeamConfig};
use regex::Regex;

pub fn parse_agent_routing(
    raw_message: &str,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
) -> RouteDecision {
    let re = Regex::new(r"^@(\S+)\s+([\s\S]*)$").expect("valid regex");
    if let Some(caps) = re.captures(raw_message) {
        let candidate = caps
            .get(1)
            .map(|m| m.as_str().to_lowercase())
            .unwrap_or_default();
        let stripped_message = caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();

        if agents.contains_key(&candidate) {
            return RouteDecision {
                agent_id: candidate,
                message: stripped_message,
                is_team_routed: false,
                team_id: None,
            };
        }

        if let Some(team) = teams.get(&candidate) {
            return RouteDecision {
                agent_id: team.leader_agent.clone(),
                message: stripped_message,
                is_team_routed: true,
                team_id: Some(candidate),
            };
        }

        for (id, agent) in agents {
            if agent.name.eq_ignore_ascii_case(&candidate) {
                return RouteDecision {
                    agent_id: id.clone(),
                    message: stripped_message,
                    is_team_routed: false,
                    team_id: None,
                };
            }
        }

        for (id, team) in teams {
            if team.name.eq_ignore_ascii_case(&candidate) {
                return RouteDecision {
                    agent_id: team.leader_agent.clone(),
                    message: stripped_message,
                    is_team_routed: true,
                    team_id: Some(id.clone()),
                };
            }
        }
    }

    RouteDecision {
        agent_id: "default".to_string(),
        message: raw_message.to_string(),
        is_team_routed: false,
        team_id: None,
    }
}

pub fn resolve_binding(
    event: &InboundEvent,
    bindings: &[BindingRule],
    default_agent: &str,
) -> String {
    let mut best: Option<(usize, &BindingRule)> = None;

    for rule in bindings {
        if let Some(score) = binding_match_score(event, rule) {
            match best {
                None => best = Some((score, rule)),
                Some((best_score, _)) if score > best_score => best = Some((score, rule)),
                _ => {}
            }
        }
    }

    best.map(|(_, r)| r.agent_id.clone())
        .unwrap_or_else(|| default_agent.to_string())
}

fn binding_match_score(event: &InboundEvent, rule: &BindingRule) -> Option<usize> {
    let mut score = 0;

    if let Some(channel) = &rule.matcher.channel {
        if &event.channel != channel {
            return None;
        }
        score += 1;
    }

    if let Some(account_id) = &rule.matcher.account_id {
        let actual = event
            .account_id
            .as_ref()
            .map(String::as_str)
            .unwrap_or("default");
        if account_id != "*" && actual != account_id {
            return None;
        }
        score += 1;
    }

    if let Some(peer_id) = &rule.matcher.peer_id {
        if &event.peer_id != peer_id {
            return None;
        }
        score += 3;
    }

    if let Some(group_id) = &rule.matcher.group_id {
        if &event.peer_id != group_id {
            return None;
        }
        score += 2;
    }

    if let Some(thread_id) = &rule.matcher.thread_id {
        if &event.peer_id != thread_id {
            return None;
        }
        score += 2;
    }

    Some(score)
}

pub fn find_team_for_agent(agent_id: &str, teams: &HashMap<String, TeamConfig>) -> Option<String> {
    for (team_id, team) in teams {
        if team.agents.iter().any(|m| m == agent_id) {
            return Some(team_id.clone());
        }
    }
    None
}

pub fn extract_teammate_mentions(
    response: &str,
    current_agent_id: &str,
    from_agent: Option<&str>,
    team_id: &str,
    teams: &HashMap<String, TeamConfig>,
    agents: &HashMap<String, AgentConfig>,
) -> Vec<MentionHandoff> {
    let Some(team) = teams.get(team_id) else {
        return vec![];
    };

    let mut seen = HashSet::new();
    let mut handoffs = vec![];

    // Tagged format: [@agent: message]
    let tag_re = Regex::new(r"\[@(\S+?):\s*([\s\S]*?)\]").expect("valid regex");
    for caps in tag_re.captures_iter(response) {
        let teammate_id = caps
            .get(1)
            .map(|m| m.as_str().to_lowercase())
            .unwrap_or_default();

        if !is_valid_teammate(&teammate_id, current_agent_id, from_agent, team, agents) {
            continue;
        }

        if !seen.insert(teammate_id.clone()) {
            continue;
        }

        let message = caps
            .get(2)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        handoffs.push(MentionHandoff {
            teammate_id,
            message,
        });
    }

    if !handoffs.is_empty() {
        return handoffs;
    }

    // Bare mention fallback: @agent
    let bare_re = Regex::new(r"@(\S+)").expect("valid regex");
    for caps in bare_re.captures_iter(response) {
        let teammate_id = caps
            .get(1)
            .map(|m| m.as_str().to_lowercase())
            .unwrap_or_default();

        if is_valid_teammate(&teammate_id, current_agent_id, from_agent, team, agents) {
            return vec![MentionHandoff {
                teammate_id,
                message: response.to_string(),
            }];
        }
    }

    vec![]
}

fn is_valid_teammate(
    mentioned_id: &str,
    current_agent_id: &str,
    from_agent: Option<&str>,
    team: &TeamConfig,
    agents: &HashMap<String, AgentConfig>,
) -> bool {
    if mentioned_id == current_agent_id {
        return false;
    }
    // Prevent ping-pong: reject mentions back to the agent that sent us this message
    if from_agent == Some(mentioned_id) {
        return false;
    }
    team.agents.iter().any(|id| id == mentioned_id) && agents.contains_key(mentioned_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::ProviderKind;

    fn sample_agents() -> HashMap<String, AgentConfig> {
        HashMap::from([
            (
                "default".to_string(),
                AgentConfig {
                    name: "Default".to_string(),
                    provider: ProviderKind::Anthropic,
                    model: "claude-sonnet-4-5".to_string(),
                    provider_id: None,
                    system_prompt: None,
                    prompt_file: None,
                    think_level: None,
                },
            ),
            (
                "reviewer".to_string(),
                AgentConfig {
                    name: "Reviewer".to_string(),
                    provider: ProviderKind::Anthropic,
                    model: "claude-sonnet-4-5".to_string(),
                    provider_id: None,
                    system_prompt: None,
                    prompt_file: None,
                    think_level: None,
                },
            ),
        ])
    }

    fn sample_teams() -> HashMap<String, TeamConfig> {
        HashMap::from([(
            "dev".to_string(),
            TeamConfig {
                name: "Development".to_string(),
                agents: vec!["default".to_string(), "reviewer".to_string()],
                leader_agent: "default".to_string(),
            },
        )])
    }

    #[test]
    fn parses_team_prefix() {
        let agents = sample_agents();
        let teams = sample_teams();
        let route = parse_agent_routing("@dev please fix bug", &agents, &teams);
        assert_eq!(route.agent_id, "default");
        assert!(route.is_team_routed);
        assert_eq!(route.team_id.as_deref(), Some("dev"));
        assert_eq!(route.message, "please fix bug");
    }

    #[test]
    fn extracts_tagged_handoff() {
        let agents = sample_agents();
        let teams = sample_teams();
        let mentions = extract_teammate_mentions(
            "done [@reviewer: review this patch]",
            "default",
            None,
            "dev",
            &teams,
            &agents,
        );
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].teammate_id, "reviewer");
        assert_eq!(mentions[0].message, "review this patch");
    }

    #[test]
    fn rejects_mention_back_to_sender() {
        let agents = sample_agents();
        let teams = sample_teams();
        // reviewer received a message from default, tries to mention default back
        let mentions = extract_teammate_mentions(
            "[@default: here is my review]",
            "reviewer",
            Some("default"),
            "dev",
            &teams,
            &agents,
        );
        assert!(mentions.is_empty(), "mention back to from_agent should be rejected");
    }

    #[test]
    fn allows_mention_to_third_party() {
        let mut agents = sample_agents();
        agents.insert(
            "tester".to_string(),
            AgentConfig {
                name: "Tester".to_string(),
                provider: ProviderKind::Anthropic,
                model: "claude-sonnet-4-5".to_string(),
                provider_id: None,
                system_prompt: None,
                prompt_file: None,
                think_level: None,
            },
        );
        let mut teams = sample_teams();
        teams.get_mut("dev").unwrap().agents.push("tester".to_string());

        // reviewer received a message from default, mentions tester (not default)
        let mentions = extract_teammate_mentions(
            "[@tester: please run tests]",
            "reviewer",
            Some("default"),
            "dev",
            &teams,
            &agents,
        );
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].teammate_id, "tester");
    }

    #[test]
    fn allows_mention_when_no_from_agent() {
        let agents = sample_agents();
        let teams = sample_teams();
        // user message (no from_agent), default mentions reviewer
        let mentions = extract_teammate_mentions(
            "[@reviewer: check this]",
            "default",
            None,
            "dev",
            &teams,
            &agents,
        );
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].teammate_id, "reviewer");
    }
}
