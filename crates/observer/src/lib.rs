use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::info;

#[derive(Debug, Clone)]
pub struct EventRecord {
    pub timestamp: String,
    pub event_type: String,
    pub payload: Value,
}

#[derive(Clone)]
pub struct FileEventSink {
    file: Arc<Mutex<File>>,
    sender: broadcast::Sender<EventRecord>,
}

impl FileEventSink {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create events dir: {}", parent.display()))?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open event sink: {}", path.display()))?;
        let (sender, _) = broadcast::channel(256);

        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            sender,
        })
    }

    pub fn emit(&self, event_type: &str, payload: Value) -> Result<()> {
        let record = EventRecord {
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            payload,
        };
        let line = json!({
            "timestamp": record.timestamp,
            "event_type": record.event_type,
            "payload": record.payload,
        })
        .to_string();

        let mut file = self.file.lock().expect("event sink lock poisoned");
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        let _ = self.sender.send(record);
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventRecord> {
        self.sender.subscribe()
    }
}

pub fn log_startup_banner(home: &Path) {
    info!(home = %home.display(), "ClawPod started");
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentHealth {
    pub status: String,
    pub updated_at: String,
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
    pub restart_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    pub pid: u32,
    pub updated_at: String,
    pub uptime_seconds: u64,
    pub components: std::collections::BTreeMap<String, ComponentHealth>,
}

struct HealthRegistry {
    started_at: Instant,
    components: Mutex<std::collections::BTreeMap<String, ComponentHealth>>,
}

static REGISTRY: OnceLock<HealthRegistry> = OnceLock::new();

fn registry() -> &'static HealthRegistry {
    REGISTRY.get_or_init(|| HealthRegistry {
        started_at: Instant::now(),
        components: Mutex::new(std::collections::BTreeMap::new()),
    })
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn upsert_component<F>(component: &str, update: F)
where
    F: FnOnce(&mut ComponentHealth),
{
    let mut map = registry()
        .components
        .lock()
        .expect("health registry lock poisoned");
    let now = now_rfc3339();
    let entry = map
        .entry(component.to_string())
        .or_insert_with(|| ComponentHealth {
            status: "starting".to_string(),
            updated_at: now.clone(),
            last_ok: None,
            last_error: None,
            restart_count: 0,
        });
    update(entry);
    entry.updated_at = now;
}

pub fn mark_component_ok(component: &str) {
    upsert_component(component, |entry| {
        entry.status = "ok".to_string();
        entry.last_ok = Some(now_rfc3339());
        entry.last_error = None;
    });
}

pub fn mark_component_error(component: &str, error: impl ToString) {
    let err = error.to_string();
    upsert_component(component, move |entry| {
        entry.status = "error".to_string();
        entry.last_error = Some(err);
    });
}

pub fn mark_component_disabled(component: &str, reason: impl ToString) {
    let reason = reason.to_string();
    upsert_component(component, move |entry| {
        entry.status = "disabled".to_string();
        entry.last_error = Some(reason);
    });
}

pub fn bump_component_restart(component: &str) {
    upsert_component(component, |entry| {
        entry.restart_count = entry.restart_count.saturating_add(1);
    });
}

pub fn snapshot() -> HealthSnapshot {
    let components = registry()
        .components
        .lock()
        .expect("health registry lock poisoned")
        .clone();

    HealthSnapshot {
        pid: std::process::id(),
        updated_at: now_rfc3339(),
        uptime_seconds: registry().started_at.elapsed().as_secs(),
        components,
    }
}

pub fn snapshot_json() -> Value {
    serde_json::to_value(snapshot()).unwrap_or_else(|_| {
        json!({
            "status": "error",
            "message": "failed to serialize health snapshot",
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_component(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn mark_component_ok_initializes_state() {
        let component = unique_component("health-ok");
        mark_component_ok(&component);

        let snapshot = snapshot();
        let entry = snapshot
            .components
            .get(&component)
            .expect("component should exist");

        assert_eq!(entry.status, "ok");
        assert!(entry.last_ok.is_some());
        assert!(entry.last_error.is_none());
    }

    #[test]
    fn mark_component_error_then_disabled_updates_state() {
        let component = unique_component("health-error");
        mark_component_error(&component, "boom");
        mark_component_disabled(&component, "missing token");

        let snapshot = snapshot();
        let entry = snapshot
            .components
            .get(&component)
            .expect("component should exist");

        assert_eq!(entry.status, "disabled");
        assert_eq!(entry.last_error.as_deref(), Some("missing token"));
    }

    #[test]
    fn bump_component_restart_increments_counter() {
        let component = unique_component("health-restart");
        bump_component_restart(&component);
        bump_component_restart(&component);

        let snapshot = snapshot();
        let entry = snapshot
            .components
            .get(&component)
            .expect("component should exist");

        assert_eq!(entry.restart_count, 2);
    }

    #[test]
    fn snapshot_json_contains_component_data() {
        let component = unique_component("health-json");
        mark_component_ok(&component);

        let json = snapshot_json();
        assert_eq!(json["components"][&component]["status"], "ok");
        assert!(json["uptime_seconds"].as_u64().is_some());
    }
}
