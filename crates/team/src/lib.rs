use std::collections::HashMap;

use anyhow::{anyhow, Result};
use domain::{
    AgentConfig, ChainResult, ChainState, ChainStep, ProviderKind, RunRequest, Runner, TeamConfig,
};
use futures::future::join_all;
use routing::extract_teammate_mentions;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TeamExecutionInput {
    pub task_id: Uuid,
    pub session_key: String,
    pub team_id: String,
    pub start_agent_id: String,
    pub initial_message: String,
    pub continue_session: bool,
    pub max_chain_steps: usize,
    pub agents: HashMap<String, AgentConfig>,
    pub teams: HashMap<String, TeamConfig>,
    pub session_workdirs: HashMap<String, String>,
    pub run_metadata: HashMap<String, HashMap<String, String>>,
}

pub async fn execute_team_chain(
    runner: &dyn Runner,
    input: TeamExecutionInput,
) -> Result<ChainResult> {
    let team = input
        .teams
        .get(&input.team_id)
        .ok_or_else(|| anyhow!("team not found: {}", input.team_id))?;

    if !team.agents.iter().any(|a| a == &input.start_agent_id) {
        return Err(anyhow!(
            "start agent {} not in team {}",
            input.start_agent_id,
            input.team_id
        ));
    }

    let mut state = ChainState::Succeeded;
    let mut steps = vec![];
    let mut current_agent = input.start_agent_id;
    let mut current_message = input.initial_message;
    let mut continue_session = input.continue_session;

    for _ in 0..input.max_chain_steps {
        let agent = input
            .agents
            .get(&current_agent)
            .ok_or_else(|| anyhow!("agent not found: {}", current_agent))?;

        let run_req = build_run_request(
            input.task_id,
            &input.session_key,
            &current_agent,
            &current_message,
            continue_session,
            agent,
            &input.session_workdirs,
            &input.run_metadata,
        );

        let run = runner.run(run_req).await?;
        let response = if run.text.is_empty() {
            run.stdout
        } else {
            run.text
        };

        let handoffs = extract_teammate_mentions(
            &response,
            &current_agent,
            &input.team_id,
            &input.teams,
            &input.agents,
        );

        steps.push(ChainStep {
            agent_id: current_agent.clone(),
            input: current_message.clone(),
            output: response.clone(),
            handoffs: handoffs.clone(),
        });

        if handoffs.is_empty() {
            return Ok(ChainResult {
                final_text: response,
                steps,
                state,
            });
        }

        if handoffs.len() == 1 {
            let mention = handoffs[0].clone();
            current_agent = mention.teammate_id;
            current_message = format!(
                "[Message from teammate @{}]:\n{}",
                steps.last().map(|s| s.agent_id.clone()).unwrap_or_default(),
                mention.message
            );
            continue_session = true;
            continue;
        }

        // fan-out: execute all mentions in parallel, aggregate and finish
        let current_sender = current_agent.clone();
        let task_id = input.task_id;
        let session_key_base = input.session_key.clone();
        let agents_map = input.agents.clone();
        let session_workdirs = input.session_workdirs.clone();
        let run_metadata = input.run_metadata.clone();

        let futures = handoffs
            .into_iter()
            .map(|mention| {
                let sender = current_sender.clone();
                let session_key = session_key_base.clone();
                let agents = agents_map.clone();
                let workdirs = session_workdirs.clone();
                let metadata_map = run_metadata.clone();
                async move {
                    let Some(target_agent) = agents.get(&mention.teammate_id) else {
                        return (mention.teammate_id, "agent not found".to_string());
                    };

                    let metadata = metadata_map
                        .get(&mention.teammate_id)
                        .cloned()
                        .unwrap_or_default();
                    let fan_req = RunRequest {
                        run_id: Uuid::new_v4(),
                        task_id,
                        session_key,
                        agent_id: mention.teammate_id.clone(),
                        provider: provider_from_metadata(&metadata)
                            .unwrap_or(target_agent.provider),
                        model: metadata
                            .get("effective_model")
                            .cloned()
                            .unwrap_or_else(|| target_agent.model.clone()),
                        think_level: target_agent.think_level.unwrap_or_default(),
                        working_directory: workdirs
                            .get(&mention.teammate_id)
                            .cloned()
                            .expect("session workdir must be pre-resolved for all agents"),
                        prompt: format!("[Message from teammate @{sender}]:\n{}", mention.message),
                        continue_session: true,
                        metadata,
                    };

                    match runner.run(fan_req).await {
                        Ok(out) => {
                            let text = if out.text.is_empty() {
                                out.stdout
                            } else {
                                out.text
                            };
                            (mention.teammate_id, text)
                        }
                        Err(err) => (mention.teammate_id, format!("error: {err}")),
                    }
                }
            })
            .collect::<Vec<_>>();

        let fanout_results = join_all(futures).await;
        let mut aggregated = vec![];
        for (agent_id, text) in fanout_results {
            aggregated.push(format!("@{agent_id}: {text}"));
        }

        let final_text = aggregated.join("\n\n---\n\n");
        steps.push(ChainStep {
            agent_id: "fanout".to_string(),
            input: "fan-out".to_string(),
            output: final_text.clone(),
            handoffs: vec![],
        });

        return Ok(ChainResult {
            final_text,
            steps,
            state,
        });
    }

    state = ChainState::MaxStepsReached;
    let final_text = steps
        .last()
        .map(|s| s.output.clone())
        .unwrap_or_else(|| "chain ended without output".to_string());

    Ok(ChainResult {
        final_text,
        steps,
        state,
    })
}

fn build_run_request(
    task_id: Uuid,
    session_key: &str,
    agent_id: &str,
    prompt: &str,
    continue_session: bool,
    agent: &AgentConfig,
    session_workdirs: &HashMap<String, String>,
    run_metadata: &HashMap<String, HashMap<String, String>>,
) -> RunRequest {
    let metadata = run_metadata.get(agent_id).cloned().unwrap_or_default();
    RunRequest {
        run_id: Uuid::new_v4(),
        task_id,
        session_key: session_key.to_string(),
        agent_id: agent_id.to_string(),
        provider: provider_from_metadata(&metadata).unwrap_or(agent.provider),
        model: metadata
            .get("effective_model")
            .cloned()
            .unwrap_or_else(|| agent.model.clone()),
        think_level: agent.think_level.unwrap_or_default(),
        working_directory: session_workdirs
            .get(agent_id)
            .cloned()
            .expect("session workdir must be pre-resolved for all agents"),
        prompt: prompt.to_string(),
        continue_session,
        metadata,
    }
}

fn provider_from_metadata(metadata: &HashMap<String, String>) -> Option<ProviderKind> {
    match metadata.get("effective_provider").map(String::as_str) {
        Some("anthropic") => Some(ProviderKind::Anthropic),
        Some("openai") => Some(ProviderKind::Openai),
        Some("custom") => Some(ProviderKind::Custom),
        Some("mock") => Some(ProviderKind::Mock),
        _ => None,
    }
}
