use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::Utc;
use domain::{ChatroomMessageView, HeartbeatRunView, RunStatus, TeamChainStepView};
pub use pairing::VerifyResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone)]
pub struct StateStore {
    path: PathBuf,
    snapshot: Arc<Mutex<StoreSnapshot>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_key: String,
    pub agent_id: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_channel: Option<String>,
    #[serde(default)]
    pub last_peer_id: Option<String>,
    #[serde(default)]
    pub last_account_id: Option<String>,
    #[serde(default)]
    pub last_chat_type: Option<String>,
    #[serde(default)]
    pub last_heartbeat_text: Option<String>,
    #[serde(default)]
    pub last_heartbeat_sent_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderAccessEntry {
    pub key: String,
    pub channel: String,
    pub sender_id: String,
    pub sender_label: Option<String>,
    pub status: String,
    pub peer_id: String,
    pub account_id: Option<String>,
    pub requested_at: String,
    pub updated_at: String,
    pub last_message_preview: Option<String>,
    pub last_message_id: Option<String>,
    pub has_pairing_code: bool,
    pub pairing_code_expires_at: Option<String>,
    pub failed_pairing_attempts: u32,
    pub is_locked_out: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderAccessRegistration {
    Approved,
    PendingCreated,
    PendingExisting,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoreSnapshot {
    #[serde(default)]
    runs: HashMap<String, RunRecord>,
    #[serde(default)]
    chain_steps: Vec<ChainStepRecord>,
    #[serde(default)]
    chatroom_messages: Vec<ChatroomMessageRecord>,
    #[serde(default)]
    heartbeat_runs: Vec<HeartbeatRunRecord>,
    #[serde(default)]
    events: Vec<EventRecord>,
    #[serde(default)]
    sessions: HashMap<String, SessionRecord>,
    #[serde(default)]
    sender_access: HashMap<String, SenderAccessRecord>,
    #[serde(default)]
    next_event_id: i64,
    #[serde(default)]
    next_chatroom_message_id: i64,
    #[serde(default)]
    next_heartbeat_run_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunRecord {
    id: String,
    #[serde(default)]
    task_id: String,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    session_key: String,
    agent_id: String,
    status: String,
    prompt: Option<String>,
    output: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    started_at: Option<String>,
    ended_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChainStepRecord {
    chain_id: String,
    task_id: String,
    #[serde(default)]
    team_id: String,
    step_index: usize,
    agent_id: String,
    input: String,
    output: String,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EventRecord {
    id: i64,
    event_type: String,
    payload: Value,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatroomMessageRecord {
    id: i64,
    team_id: String,
    from_agent: String,
    message: String,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeartbeatRunRecord {
    id: i64,
    agent_id: String,
    prompt: String,
    output: Option<String>,
    status: String,
    started_at: String,
    finished_at: String,
    duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    session_key: String,
    agent_id: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    last_channel: Option<String>,
    #[serde(default)]
    last_peer_id: Option<String>,
    #[serde(default)]
    last_account_id: Option<String>,
    #[serde(default)]
    last_chat_type: Option<String>,
    #[serde(default)]
    last_heartbeat_text: Option<String>,
    #[serde(default)]
    last_heartbeat_sent_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SenderAccessRecord {
    key: String,
    channel: String,
    sender_id: String,
    sender_label: Option<String>,
    status: String,
    peer_id: String,
    account_id: Option<String>,
    requested_at: String,
    updated_at: String,
    last_message_preview: Option<String>,
    last_message_id: Option<String>,
    #[serde(default)]
    pairing_code: Option<String>,
    #[serde(default)]
    pairing_code_expires_at: Option<String>,
    #[serde(default)]
    failed_pairing_attempts: u32,
    #[serde(default)]
    locked_until: Option<String>,
}

impl StateStore {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create state dir: {}", parent.display()))?;
        }

        let mut snapshot = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read state file: {}", path.display()))?;
            if raw.trim().is_empty() {
                StoreSnapshot::default()
            } else {
                serde_json::from_str::<StoreSnapshot>(&raw)
                    .with_context(|| format!("failed to parse state file: {}", path.display()))?
            }
        } else {
            StoreSnapshot::default()
        };
        snapshot.normalize_ids();
        snapshot.remove_orphaned_runs();

        let store = Self {
            path,
            snapshot: Arc::new(Mutex::new(snapshot)),
        };
        store.persist()?;
        Ok(store)
    }

    pub fn record_run_start(
        &self,
        run_id: Uuid,
        task_id: Uuid,
        message_id: &str,
        session_key: &str,
        agent_id: &str,
        prompt: &str,
    ) -> Result<()> {
        let now = now_rfc3339();
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        snapshot.runs.insert(
            run_id.to_string(),
            RunRecord {
                id: run_id.to_string(),
                task_id: task_id.to_string(),
                message_id: message_id.to_string(),
                session_key: session_key.to_string(),
                agent_id: agent_id.to_string(),
                status: "pending".to_string(),
                prompt: Some(prompt.to_string()),
                output: None,
                error: None,
                duration_ms: None,
                created_at: now.clone(),
                updated_at: now,
                started_at: None,
                ended_at: None,
            },
        );
        self.persist_locked(&snapshot)
    }

    pub fn record_run_end(
        &self,
        run_id: Uuid,
        status: RunStatus,
        output: Option<&str>,
        error: Option<&str>,
        duration_ms: Option<u128>,
    ) -> Result<()> {
        let now = now_rfc3339();
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        if let Some(run) = snapshot.runs.get_mut(&run_id.to_string()) {
            run.status = status_to_str(status).to_string();
            run.output = output.map(ToString::to_string);
            run.error = error.map(ToString::to_string);
            run.duration_ms = duration_ms.map(|value| value as i64);
            run.ended_at = Some(now.clone());
            run.updated_at = now;
        }
        self.persist_locked(&snapshot)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_chain_step(
        &self,
        chain_id: Uuid,
        task_id: Uuid,
        team_id: &str,
        step_index: usize,
        agent_id: &str,
        input: &str,
        output: &str,
    ) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        snapshot.chain_steps.push(ChainStepRecord {
            chain_id: chain_id.to_string(),
            task_id: task_id.to_string(),
            team_id: team_id.to_string(),
            step_index,
            agent_id: agent_id.to_string(),
            input: input.to_string(),
            output: output.to_string(),
            created_at: now_rfc3339(),
        });
        self.persist_locked(&snapshot)
    }

    pub fn list_chain_steps(&self, team_id: &str, limit: usize) -> Result<Vec<TeamChainStepView>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let mut steps: Vec<_> = snapshot
            .chain_steps
            .iter()
            .filter(|s| s.team_id == team_id)
            .cloned()
            .collect();
        steps.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let start = steps.len().saturating_sub(limit);
        Ok(steps[start..]
            .iter()
            .map(|s| TeamChainStepView {
                chain_id: s.chain_id.clone(),
                task_id: s.task_id.clone(),
                step_index: s.step_index,
                agent_id: s.agent_id.clone(),
                input: s.input.clone(),
                output: s.output.clone(),
                created_at: s.created_at.clone(),
            })
            .collect())
    }

    pub fn record_event(&self, event_type: &str, payload: &Value) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        snapshot.next_event_id += 1;
        let id = snapshot.next_event_id;
        snapshot.events.push(EventRecord {
            id,
            event_type: event_type.to_string(),
            payload: payload.clone(),
            created_at: now_rfc3339(),
        });
        self.persist_locked(&snapshot)
    }

    pub fn record_chatroom_message(
        &self,
        team_id: &str,
        from_agent: &str,
        message: &str,
    ) -> Result<ChatroomMessageView> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        snapshot.next_chatroom_message_id += 1;
        let record = ChatroomMessageRecord {
            id: snapshot.next_chatroom_message_id,
            team_id: team_id.to_string(),
            from_agent: from_agent.to_string(),
            message: message.to_string(),
            created_at: now_rfc3339(),
        };
        snapshot.chatroom_messages.push(record.clone());
        self.persist_locked(&snapshot)?;
        Ok(ChatroomMessageView {
            id: record.id,
            team_id: record.team_id,
            from_agent: record.from_agent,
            message: record.message,
            created_at: record.created_at,
        })
    }

    pub fn list_chatroom_messages(
        &self,
        team_id: &str,
        limit: usize,
        since_id: Option<i64>,
    ) -> Result<Vec<ChatroomMessageView>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let mut messages: Vec<_> = snapshot
            .chatroom_messages
            .iter()
            .filter(|record| record.team_id == team_id)
            .filter(|record| since_id.map_or(true, |since| record.id > since))
            .cloned()
            .collect();
        messages.sort_by_key(|record| record.id);
        let start = messages.len().saturating_sub(limit);
        Ok(messages[start..]
            .iter()
            .map(|record| ChatroomMessageView {
                id: record.id,
                team_id: record.team_id.clone(),
                from_agent: record.from_agent.clone(),
                message: record.message.clone(),
                created_at: record.created_at.clone(),
            })
            .collect())
    }

    pub fn record_heartbeat_run(
        &self,
        agent_id: &str,
        prompt: &str,
        output: Option<&str>,
        status: &str,
        started_at: &str,
        finished_at: &str,
        duration_ms: i64,
    ) -> Result<HeartbeatRunView> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        snapshot.next_heartbeat_run_id += 1;
        let record = HeartbeatRunRecord {
            id: snapshot.next_heartbeat_run_id,
            agent_id: agent_id.to_string(),
            prompt: prompt.to_string(),
            output: output.map(ToString::to_string),
            status: status.to_string(),
            started_at: started_at.to_string(),
            finished_at: finished_at.to_string(),
            duration_ms,
        };
        snapshot.heartbeat_runs.push(record.clone());
        self.persist_locked(&snapshot)?;
        Ok(HeartbeatRunView {
            id: record.id,
            agent_id: record.agent_id,
            prompt: record.prompt,
            output: record.output,
            status: record.status,
            started_at: record.started_at,
            finished_at: record.finished_at,
            duration_ms: record.duration_ms,
        })
    }

    pub fn list_heartbeat_runs(
        &self,
        limit: usize,
        agent_id: Option<&str>,
    ) -> Result<Vec<HeartbeatRunView>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let mut runs: Vec<_> = snapshot
            .heartbeat_runs
            .iter()
            .filter(|record| agent_id.map_or(true, |value| record.agent_id == value))
            .cloned()
            .collect();
        runs.sort_by(|a, b| {
            b.finished_at
                .cmp(&a.finished_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        runs.truncate(limit);
        Ok(runs
            .into_iter()
            .map(|record| HeartbeatRunView {
                id: record.id,
                agent_id: record.agent_id,
                prompt: record.prompt,
                output: record.output,
                status: record.status,
                started_at: record.started_at,
                finished_at: record.finished_at,
                duration_ms: record.duration_ms,
            })
            .collect())
    }

    pub fn session_exists(&self, session_key: &str) -> Result<bool> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        Ok(snapshot.sessions.contains_key(session_key))
    }

    pub fn touch_session(&self, session_key: &str, agent_id: &str) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let now = now_rfc3339();
        snapshot
            .sessions
            .entry(session_key.to_string())
            .and_modify(|session| {
                session.agent_id = agent_id.to_string();
                session.updated_at = now.clone();
            })
            .or_insert_with(|| SessionRecord {
                session_key: session_key.to_string(),
                agent_id: agent_id.to_string(),
                created_at: now.clone(),
                updated_at: now,
                last_channel: None,
                last_peer_id: None,
                last_account_id: None,
                last_chat_type: None,
                last_heartbeat_text: None,
                last_heartbeat_sent_at: None,
            });
        self.persist_locked(&snapshot)
    }

    pub fn get_session(&self, session_key: &str) -> Result<Option<SessionSummary>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        Ok(snapshot
            .sessions
            .get(session_key)
            .cloned()
            .map(|session| SessionSummary {
                session_key: session.session_key,
                agent_id: session.agent_id,
                created_at: session.created_at,
                updated_at: session.updated_at,
                last_channel: session.last_channel,
                last_peer_id: session.last_peer_id,
                last_account_id: session.last_account_id,
                last_chat_type: session.last_chat_type,
                last_heartbeat_text: session.last_heartbeat_text,
                last_heartbeat_sent_at: session.last_heartbeat_sent_at,
            }))
    }

    pub fn update_session_route(
        &self,
        session_key: &str,
        agent_id: &str,
        channel: &str,
        peer_id: &str,
        account_id: Option<&str>,
        chat_type: &str,
    ) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let now = now_rfc3339();
        snapshot
            .sessions
            .entry(session_key.to_string())
            .and_modify(|session| {
                session.agent_id = agent_id.to_string();
                session.updated_at = now.clone();
                session.last_channel = Some(channel.to_string());
                session.last_peer_id = Some(peer_id.to_string());
                session.last_account_id = account_id.map(ToString::to_string);
                session.last_chat_type = Some(chat_type.to_string());
            })
            .or_insert_with(|| SessionRecord {
                session_key: session_key.to_string(),
                agent_id: agent_id.to_string(),
                created_at: now.clone(),
                updated_at: now,
                last_channel: Some(channel.to_string()),
                last_peer_id: Some(peer_id.to_string()),
                last_account_id: account_id.map(ToString::to_string),
                last_chat_type: Some(chat_type.to_string()),
                last_heartbeat_text: None,
                last_heartbeat_sent_at: None,
            });
        self.persist_locked(&snapshot)
    }

    pub fn record_heartbeat_delivery(
        &self,
        session_key: &str,
        agent_id: &str,
        text: Option<&str>,
        sent_at_ms: Option<i64>,
    ) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let now = now_rfc3339();
        snapshot
            .sessions
            .entry(session_key.to_string())
            .and_modify(|session| {
                session.agent_id = agent_id.to_string();
                session.updated_at = now.clone();
                session.last_heartbeat_text = text.map(ToString::to_string);
                session.last_heartbeat_sent_at = sent_at_ms;
            })
            .or_insert_with(|| SessionRecord {
                session_key: session_key.to_string(),
                agent_id: agent_id.to_string(),
                created_at: now.clone(),
                updated_at: now,
                last_channel: None,
                last_peer_id: None,
                last_account_id: None,
                last_chat_type: None,
                last_heartbeat_text: text.map(ToString::to_string),
                last_heartbeat_sent_at: sent_at_ms,
            });
        self.persist_locked(&snapshot)
    }

    pub fn restore_session_updated_at(&self, session_key: &str, updated_at: &str) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        if let Some(session) = snapshot.sessions.get_mut(session_key) {
            session.updated_at = updated_at.to_string();
        }
        self.persist_locked(&snapshot)
    }

    pub fn clear_agent_sessions(&self, agent_id: &str) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        snapshot
            .sessions
            .retain(|_, session| session.agent_id != agent_id);
        self.persist_locked(&snapshot)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let mut sessions = snapshot.sessions.values().cloned().collect::<Vec<_>>();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions
            .into_iter()
            .map(|session| SessionSummary {
                session_key: session.session_key,
                agent_id: session.agent_id,
                created_at: session.created_at,
                updated_at: session.updated_at,
                last_channel: session.last_channel,
                last_peer_id: session.last_peer_id,
                last_account_id: session.last_account_id,
                last_chat_type: session.last_chat_type,
                last_heartbeat_text: session.last_heartbeat_text,
                last_heartbeat_sent_at: session.last_heartbeat_sent_at,
            })
            .collect())
    }

    pub fn list_recent_runs(&self, limit: usize) -> Result<Vec<Value>> {
        self.list_recent_runs_filtered(limit, None, None)
    }

    pub fn list_recent_runs_filtered(
        &self,
        limit: usize,
        session_key: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let mut runs = snapshot.runs.values().cloned().collect::<Vec<_>>();
        if let Some(session_key) = session_key {
            runs.retain(|run| run.session_key == session_key);
        }
        if let Some(agent_id) = agent_id {
            runs.retain(|run| run.agent_id == agent_id);
        }
        runs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        runs.truncate(limit);

        Ok(runs
            .into_iter()
            .map(|run| {
                serde_json::json!({
                    "run_id": run.id,
                    "task_id": run.task_id,
                    "message_id": run.message_id,
                    "session_key": run.session_key,
                    "agent_id": run.agent_id,
                    "status": run.status,
                    "error": run.error,
                    "created_at": run.created_at,
                    "updated_at": run.updated_at,
                    "output": run.output,
                    "duration_ms": run.duration_ms,
                })
            })
            .collect())
    }

    pub fn list_recent_events(&self, limit: usize) -> Result<Vec<Value>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let mut events = snapshot.events.clone();
        events.sort_by(|a, b| b.id.cmp(&a.id));
        events.truncate(limit);
        Ok(events
            .into_iter()
            .map(|event| {
                serde_json::json!({
                    "id": event.id,
                    "event_type": event.event_type,
                    "payload": event.payload,
                    "created_at": event.created_at,
                })
            })
            .collect())
    }

    pub fn is_sender_approved(&self, channel: &str, sender_id: &str) -> Result<bool> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let key = sender_access_key(channel, sender_id);
        Ok(snapshot
            .sender_access
            .get(&key)
            .is_some_and(|entry| entry.status == "approved"))
    }

    pub fn register_sender_access_request(
        &self,
        channel: &str,
        sender_id: &str,
        sender_label: Option<&str>,
        peer_id: &str,
        account_id: Option<&str>,
        last_message_preview: Option<&str>,
        last_message_id: Option<&str>,
    ) -> Result<SenderAccessRegistration> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let key = sender_access_key(channel, sender_id);
        if let Some(entry) = snapshot.sender_access.get_mut(&key) {
            entry.sender_label = normalize_optional_str(sender_label);
            entry.peer_id = peer_id.to_string();
            entry.account_id = normalize_optional_str(account_id);
            entry.updated_at = now_rfc3339();
            entry.last_message_preview = normalize_optional_str(last_message_preview);
            entry.last_message_id = normalize_optional_str(last_message_id);
            let result = if entry.status == "approved" {
                SenderAccessRegistration::Approved
            } else {
                entry.status = "pending".to_string();
                SenderAccessRegistration::PendingExisting
            };
            self.persist_locked(&snapshot)?;
            return Ok(result);
        }

        let now = now_rfc3339();
        snapshot.sender_access.insert(
            key.clone(),
            SenderAccessRecord {
                key,
                channel: channel.to_string(),
                sender_id: sender_id.to_string(),
                sender_label: normalize_optional_str(sender_label),
                status: "pending".to_string(),
                peer_id: peer_id.to_string(),
                account_id: normalize_optional_str(account_id),
                requested_at: now.clone(),
                updated_at: now,
                last_message_preview: normalize_optional_str(last_message_preview),
                last_message_id: normalize_optional_str(last_message_id),
                pairing_code: None,
                pairing_code_expires_at: None,
                failed_pairing_attempts: 0,
                locked_until: None,
            },
        );
        self.persist_locked(&snapshot)?;
        Ok(SenderAccessRegistration::PendingCreated)
    }

    pub fn list_sender_access(
        &self,
        channel: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<SenderAccessEntry>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let mut entries = snapshot
            .sender_access
            .values()
            .filter(|entry| match channel {
                Some(value) => entry.channel == value,
                None => true,
            })
            .filter(|entry| match status {
                Some(value) => entry.status == value,
                None => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            sender_access_status_rank(&a.status)
                .cmp(&sender_access_status_rank(&b.status))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
                .then_with(|| a.channel.cmp(&b.channel))
                .then_with(|| a.sender_id.cmp(&b.sender_id))
        });
        Ok(entries.into_iter().map(sender_access_from_record).collect())
    }

    pub fn approve_sender_access(
        &self,
        channel: &str,
        sender_id: &str,
    ) -> Result<Option<SenderAccessEntry>> {
        self.update_sender_access_status(channel, sender_id, "approved")
    }

    pub fn reject_sender_access(
        &self,
        channel: &str,
        sender_id: &str,
    ) -> Result<Option<SenderAccessEntry>> {
        self.update_sender_access_status(channel, sender_id, "rejected")
    }

    fn persist(&self) -> Result<()> {
        let snapshot = self.lock_snapshot()?;
        self.persist_locked(&snapshot)
    }

    fn persist_locked(&self, snapshot: &StoreSnapshot) -> Result<()> {
        let tmp_path = self
            .path
            .with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
        let raw =
            serde_json::to_vec_pretty(snapshot).context("failed to serialize state snapshot")?;
        fs::write(&tmp_path, raw)
            .with_context(|| format!("failed to write temp state file: {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &self.path).with_context(|| {
            format!(
                "failed to atomically replace state file: {}",
                self.path.display()
            )
        })?;
        Ok(())
    }

    fn lock_snapshot(&self) -> Result<std::sync::MutexGuard<'_, StoreSnapshot>> {
        self.snapshot
            .lock()
            .map_err(|_| anyhow::anyhow!("state store lock poisoned"))
    }

    fn refresh_locked(&self, snapshot: &mut StoreSnapshot) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }

        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read state file: {}", self.path.display()))?;
        if raw.trim().is_empty() {
            *snapshot = StoreSnapshot::default();
            return Ok(());
        }

        *snapshot = serde_json::from_str::<StoreSnapshot>(&raw)
            .with_context(|| format!("failed to parse state file: {}", self.path.display()))?;
        snapshot.normalize_ids();
        Ok(())
    }

    pub fn store_pairing_code(
        &self,
        channel: &str,
        sender_id: &str,
        code: &str,
        expires_at: &str,
    ) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let key = sender_access_key(channel, sender_id);
        if let Some(entry) = snapshot.sender_access.get_mut(&key) {
            entry.pairing_code = Some(code.to_string());
            entry.pairing_code_expires_at = Some(expires_at.to_string());
            entry.updated_at = now_rfc3339();
        }
        self.persist_locked(&snapshot)
    }

    pub fn get_pairing_code(
        &self,
        channel: &str,
        sender_id: &str,
    ) -> Result<Option<(String, String)>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let key = sender_access_key(channel, sender_id);
        Ok(snapshot.sender_access.get(&key).and_then(|entry| {
            match (&entry.pairing_code, &entry.pairing_code_expires_at) {
                (Some(code), Some(expires)) => Some((code.clone(), expires.clone())),
                _ => None,
            }
        }))
    }

    pub fn clear_pairing_code(&self, channel: &str, sender_id: &str) -> Result<()> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let key = sender_access_key(channel, sender_id);
        if let Some(entry) = snapshot.sender_access.get_mut(&key) {
            entry.pairing_code = None;
            entry.pairing_code_expires_at = None;
        }
        self.persist_locked(&snapshot)
    }

    pub fn find_pending_by_code(&self, code: &str) -> Result<Option<SenderAccessEntry>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        for record in snapshot.sender_access.values() {
            if record.status != "pending" {
                continue;
            }
            if let Some(stored_code) = &record.pairing_code {
                if pairing::verify_code(code, stored_code) {
                    return Ok(Some(sender_access_from_record(record.clone())));
                }
            }
        }
        Ok(None)
    }

    /// Verify a pairing code for a sender.
    /// On success: approves the sender, clears the code.
    /// On failure: tracks attempts, may lock out.
    pub fn verify_pairing_code(
        &self,
        channel: &str,
        sender_id: &str,
        provided_code: &str,
        max_failed_attempts: u32,
        lockout_secs: u64,
    ) -> Result<VerifyResult> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let key = sender_access_key(channel, sender_id);

        let Some(entry) = snapshot.sender_access.get_mut(&key) else {
            return Ok(VerifyResult::InvalidCode);
        };

        // Check lockout
        if let Some(locked_until_str) = &entry.locked_until {
            if let Ok(locked_until) = chrono::DateTime::parse_from_rfc3339(locked_until_str) {
                if Utc::now() < locked_until {
                    return Ok(VerifyResult::LockedOut);
                }
                // Lockout expired, reset
                entry.locked_until = None;
                entry.failed_pairing_attempts = 0;
            }
        }

        // Check code exists
        let Some(stored_code) = &entry.pairing_code else {
            return Ok(VerifyResult::InvalidCode);
        };
        let stored_code = stored_code.clone();

        // Check expiration
        if let Some(expires_str) = &entry.pairing_code_expires_at {
            if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_str) {
                if pairing::is_code_expired(&expires.with_timezone(&Utc)) {
                    return Ok(VerifyResult::Expired);
                }
            }
        }

        // Constant-time comparison
        if pairing::verify_code(provided_code, &stored_code) {
            entry.status = "approved".to_string();
            entry.pairing_code = None;
            entry.pairing_code_expires_at = None;
            entry.failed_pairing_attempts = 0;
            entry.locked_until = None;
            entry.updated_at = now_rfc3339();
            self.persist_locked(&snapshot)?;
            return Ok(VerifyResult::Approved);
        }

        // Wrong code: track attempt
        entry.failed_pairing_attempts += 1;
        if entry.failed_pairing_attempts >= max_failed_attempts {
            let lockout_end = Utc::now() + chrono::Duration::seconds(lockout_secs as i64);
            entry.locked_until = Some(lockout_end.to_rfc3339());
        }
        entry.updated_at = now_rfc3339();
        self.persist_locked(&snapshot)?;
        Ok(VerifyResult::InvalidCode)
    }

    fn update_sender_access_status(
        &self,
        channel: &str,
        sender_id: &str,
        status: &str,
    ) -> Result<Option<SenderAccessEntry>> {
        let mut snapshot = self.lock_snapshot()?;
        self.refresh_locked(&mut snapshot)?;
        let key = sender_access_key(channel, sender_id);
        let Some(entry) = snapshot.sender_access.get_mut(&key) else {
            return Ok(None);
        };
        entry.status = status.to_string();
        entry.updated_at = now_rfc3339();
        let updated = entry.clone();
        self.persist_locked(&snapshot)?;
        Ok(Some(sender_access_from_record(updated)))
    }
}

impl StoreSnapshot {
    fn remove_orphaned_runs(&mut self) {
        self.runs.retain(|_, run| !run.session_key.is_empty());
    }

    fn normalize_ids(&mut self) {
        self.next_event_id = self
            .events
            .iter()
            .map(|event| event.id)
            .max()
            .unwrap_or_default();
        self.next_chatroom_message_id = self
            .chatroom_messages
            .iter()
            .map(|message| message.id)
            .max()
            .unwrap_or_default();
        self.next_heartbeat_run_id = self
            .heartbeat_runs
            .iter()
            .map(|run| run.id)
            .max()
            .unwrap_or_default();
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn status_to_str(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Pending => "pending",
        RunStatus::Running => "running",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
    }
}

fn sender_access_key(channel: &str, sender_id: &str) -> String {
    format!("{channel}:{sender_id}")
}

fn sender_access_status_rank(status: &str) -> usize {
    match status {
        "pending" => 0,
        "approved" => 1,
        "rejected" => 2,
        _ => 99,
    }
}

fn normalize_optional_str(value: Option<&str>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn sender_access_from_record(record: SenderAccessRecord) -> SenderAccessEntry {
    let is_locked_out = record
        .locked_until
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|locked| chrono::Utc::now() < locked);
    SenderAccessEntry {
        key: record.key,
        channel: record.channel,
        sender_id: record.sender_id,
        sender_label: record.sender_label,
        status: record.status,
        peer_id: record.peer_id,
        account_id: record.account_id,
        requested_at: record.requested_at,
        updated_at: record.updated_at,
        last_message_preview: record.last_message_preview,
        last_message_id: record.last_message_id,
        has_pairing_code: record.pairing_code.is_some(),
        pairing_code_expires_at: record.pairing_code_expires_at,
        failed_pairing_attempts: record.failed_pairing_attempts,
        is_locked_out,
    }
}

#[cfg(test)]
mod tests {
    use super::{SenderAccessRegistration, StateStore};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_state_path(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("clawpod_store_{label}_{stamp}.json"))
    }

    #[test]
    fn sender_access_registers_then_approves() {
        let path = temp_state_path("pairing");
        let store = StateStore::new(&path).expect("store");

        let first = store
            .register_sender_access_request(
                "slack",
                "U123",
                Some("alice"),
                "D123",
                None,
                Some("hello"),
                Some("msg-1"),
            )
            .expect("register");
        assert_eq!(first, SenderAccessRegistration::PendingCreated);
        assert!(!store.is_sender_approved("slack", "U123").expect("approved"));

        let second = store
            .register_sender_access_request(
                "slack",
                "U123",
                Some("alice"),
                "D123",
                None,
                Some("follow up"),
                Some("msg-2"),
            )
            .expect("register again");
        assert_eq!(second, SenderAccessRegistration::PendingExisting);

        let approved = store
            .approve_sender_access("slack", "U123")
            .expect("approve")
            .expect("entry");
        assert_eq!(approved.status, "approved");
        assert!(store.is_sender_approved("slack", "U123").expect("approved"));

        let third = store
            .register_sender_access_request(
                "slack",
                "U123",
                Some("alice"),
                "D123",
                None,
                Some("post approval"),
                Some("msg-3"),
            )
            .expect("register approved");
        assert_eq!(third, SenderAccessRegistration::Approved);
    }

    #[test]
    fn chatroom_messages_roundtrip_in_order() {
        let path = temp_state_path("chatroom");
        let store = StateStore::new(&path).expect("store");

        let first = store
            .record_chatroom_message("dev", "default", "hello team")
            .expect("first");
        let second = store
            .record_chatroom_message("dev", "reviewer", "on it")
            .expect("second");

        let messages = store
            .list_chatroom_messages("dev", 10, None)
            .expect("list messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, first.id);
        assert_eq!(messages[1].id, second.id);
        assert_eq!(messages[1].from_agent, "reviewer");
    }

    #[test]
    fn heartbeat_runs_roundtrip_in_reverse_chronological_order() {
        let path = temp_state_path("heartbeat_runs");
        let store = StateStore::new(&path).expect("store");

        store
            .record_heartbeat_run(
                "default",
                "check backlog",
                Some("all clear"),
                "ok",
                "2025-01-01T00:00:00Z",
                "2025-01-01T00:00:01Z",
                1000,
            )
            .expect("first run");
        store
            .record_heartbeat_run(
                "reviewer",
                "review queue",
                Some("two pending"),
                "ok",
                "2025-01-01T00:05:00Z",
                "2025-01-01T00:05:01Z",
                900,
            )
            .expect("second run");

        let runs = store.list_heartbeat_runs(10, None).expect("list runs");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].agent_id, "reviewer");
        assert_eq!(runs[1].agent_id, "default");

        let filtered = store
            .list_heartbeat_runs(10, Some("default"))
            .expect("list filtered");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].prompt, "check backlog");
    }

    #[test]
    fn store_and_retrieve_pairing_code() {
        let path = temp_state_path("pairing_code");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "T456",
                Some("bob"),
                "C456",
                None,
                Some("hi"),
                Some("msg-t1"),
            )
            .expect("register");

        store
            .store_pairing_code("telegram", "T456", "ABCDEFGH", "2099-01-01T00:00:00Z")
            .expect("store code");

        let code = store
            .get_pairing_code("telegram", "T456")
            .expect("get code");
        assert_eq!(
            code,
            Some(("ABCDEFGH".to_string(), "2099-01-01T00:00:00Z".to_string()))
        );
    }

    #[test]
    fn get_pairing_code_returns_none_without_code() {
        let path = temp_state_path("pairing_no_code");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "T789",
                Some("carol"),
                "C789",
                None,
                Some("hello"),
                Some("msg-t2"),
            )
            .expect("register");

        let code = store
            .get_pairing_code("telegram", "T789")
            .expect("get code");
        assert!(code.is_none());
    }

    #[test]
    fn get_pairing_code_returns_none_for_unknown_sender() {
        let path = temp_state_path("pairing_unknown");
        let store = StateStore::new(&path).expect("store");

        let code = store
            .get_pairing_code("telegram", "UNKNOWN")
            .expect("get code");
        assert!(code.is_none());
    }

    #[test]
    fn clear_pairing_code_removes_code() {
        let path = temp_state_path("pairing_clear");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "discord",
                "D001",
                Some("dave"),
                "G001",
                None,
                Some("yo"),
                Some("msg-d1"),
            )
            .expect("register");

        store
            .store_pairing_code("discord", "D001", "XYZWVUTS", "2099-01-01T00:00:00Z")
            .expect("store code");

        store
            .clear_pairing_code("discord", "D001")
            .expect("clear code");

        let code = store.get_pairing_code("discord", "D001").expect("get code");
        assert!(code.is_none());
    }

    #[test]
    fn find_pending_by_code_matches_correct_sender() {
        let path = temp_state_path("pairing_find");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "slack",
                "S001",
                Some("eve"),
                "D001",
                None,
                Some("hey"),
                Some("msg-s1"),
            )
            .expect("register");

        store
            .store_pairing_code("slack", "S001", "TESTCODE", "2099-01-01T00:00:00Z")
            .expect("store code");

        let found = store
            .find_pending_by_code("TESTCODE")
            .expect("find")
            .expect("entry");
        assert_eq!(found.channel, "slack");
        assert_eq!(found.sender_id, "S001");
    }

    #[test]
    fn find_pending_by_code_case_insensitive() {
        let path = temp_state_path("pairing_find_case");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "T100",
                Some("frank"),
                "C100",
                None,
                Some("hi"),
                Some("msg-t100"),
            )
            .expect("register");

        store
            .store_pairing_code("telegram", "T100", "UPPERCASE", "2099-01-01T00:00:00Z")
            .expect("store code");

        let found = store.find_pending_by_code("uppercase").expect("find");
        assert!(found.is_some());
    }

    #[test]
    fn find_pending_by_code_ignores_approved_entries() {
        let path = temp_state_path("pairing_find_approved");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "slack",
                "S002",
                Some("grace"),
                "D002",
                None,
                Some("hey"),
                Some("msg-s2"),
            )
            .expect("register");

        store
            .store_pairing_code("slack", "S002", "APPROVED1", "2099-01-01T00:00:00Z")
            .expect("store code");

        store
            .approve_sender_access("slack", "S002")
            .expect("approve");

        let found = store.find_pending_by_code("APPROVED1").expect("find");
        assert!(found.is_none(), "should not find approved entry by code");
    }

    // ---- verify_pairing_code ----

    #[test]
    fn verify_correct_code_approves_sender() {
        let path = temp_state_path("verify_ok");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "V1",
                Some("alice"),
                "P1",
                None,
                Some("hi"),
                Some("m1"),
            )
            .expect("register");

        store
            .store_pairing_code("telegram", "V1", "ABCDEFGH", "2099-01-01T00:00:00Z")
            .expect("store code");

        let result = store
            .verify_pairing_code("telegram", "V1", "ABCDEFGH", 5, 300)
            .expect("verify");
        assert_eq!(result, super::VerifyResult::Approved);
        assert!(store.is_sender_approved("telegram", "V1").expect("check"));
    }

    #[test]
    fn verify_correct_code_case_insensitive() {
        let path = temp_state_path("verify_case");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "V2",
                Some("bob"),
                "P2",
                None,
                Some("hi"),
                Some("m2"),
            )
            .expect("register");

        store
            .store_pairing_code("telegram", "V2", "ABCDEFGH", "2099-01-01T00:00:00Z")
            .expect("store code");

        let result = store
            .verify_pairing_code("telegram", "V2", "abcdefgh", 5, 300)
            .expect("verify");
        assert_eq!(result, super::VerifyResult::Approved);
    }

    #[test]
    fn verify_wrong_code_returns_invalid() {
        let path = temp_state_path("verify_wrong");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "V3",
                Some("carol"),
                "P3",
                None,
                Some("hi"),
                Some("m3"),
            )
            .expect("register");

        store
            .store_pairing_code("telegram", "V3", "ABCDEFGH", "2099-01-01T00:00:00Z")
            .expect("store code");

        let result = store
            .verify_pairing_code("telegram", "V3", "WRONGCDE", 5, 300)
            .expect("verify");
        assert_eq!(result, super::VerifyResult::InvalidCode);
        assert!(!store.is_sender_approved("telegram", "V3").expect("check"));
    }

    #[test]
    fn verify_expired_code_returns_expired() {
        let path = temp_state_path("verify_expired");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "V4",
                Some("dave"),
                "P4",
                None,
                Some("hi"),
                Some("m4"),
            )
            .expect("register");

        // Expired timestamp in the past
        store
            .store_pairing_code("telegram", "V4", "ABCDEFGH", "2020-01-01T00:00:00Z")
            .expect("store code");

        let result = store
            .verify_pairing_code("telegram", "V4", "ABCDEFGH", 5, 300)
            .expect("verify");
        assert_eq!(result, super::VerifyResult::Expired);
    }

    #[test]
    fn verify_lockout_after_max_attempts() {
        let path = temp_state_path("verify_lockout");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "V5",
                Some("eve"),
                "P5",
                None,
                Some("hi"),
                Some("m5"),
            )
            .expect("register");

        store
            .store_pairing_code("telegram", "V5", "ABCDEFGH", "2099-01-01T00:00:00Z")
            .expect("store code");

        // Exhaust attempts (max_failed_attempts = 3 for this test)
        for _ in 0..3 {
            let result = store
                .verify_pairing_code("telegram", "V5", "WRONGCDE", 3, 300)
                .expect("verify");
            assert_eq!(result, super::VerifyResult::InvalidCode);
        }

        // Now locked out
        let result = store
            .verify_pairing_code("telegram", "V5", "ABCDEFGH", 3, 300)
            .expect("verify");
        assert_eq!(result, super::VerifyResult::LockedOut);
    }

    #[test]
    fn verify_no_code_returns_invalid() {
        let path = temp_state_path("verify_no_code");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "V6",
                Some("frank"),
                "P6",
                None,
                Some("hi"),
                Some("m6"),
            )
            .expect("register");

        let result = store
            .verify_pairing_code("telegram", "V6", "ANYTHING", 5, 300)
            .expect("verify");
        assert_eq!(result, super::VerifyResult::InvalidCode);
    }

    #[test]
    fn verify_unknown_sender_returns_invalid() {
        let path = temp_state_path("verify_unknown");
        let store = StateStore::new(&path).expect("store");

        let result = store
            .verify_pairing_code("telegram", "NOBODY", "ANYTHING", 5, 300)
            .expect("verify");
        assert_eq!(result, super::VerifyResult::InvalidCode);
    }

    #[test]
    fn verify_clears_code_on_success() {
        let path = temp_state_path("verify_clears");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "telegram",
                "V7",
                Some("grace"),
                "P7",
                None,
                Some("hi"),
                Some("m7"),
            )
            .expect("register");

        store
            .store_pairing_code("telegram", "V7", "ABCDEFGH", "2099-01-01T00:00:00Z")
            .expect("store code");

        store
            .verify_pairing_code("telegram", "V7", "ABCDEFGH", 5, 300)
            .expect("verify");

        // Code should be cleared after approval
        let code = store.get_pairing_code("telegram", "V7").expect("get");
        assert!(code.is_none());
    }

    #[test]
    fn find_pending_by_code_returns_none_for_wrong_code() {
        let path = temp_state_path("pairing_find_wrong");
        let store = StateStore::new(&path).expect("store");

        store
            .register_sender_access_request(
                "slack",
                "S003",
                Some("heidi"),
                "D003",
                None,
                Some("hello"),
                Some("msg-s3"),
            )
            .expect("register");

        store
            .store_pairing_code("slack", "S003", "REALCODE", "2099-01-01T00:00:00Z")
            .expect("store code");

        let found = store.find_pending_by_code("WRONGONE").expect("find");
        assert!(found.is_none());
    }
}
