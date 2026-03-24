use std::sync::Arc;

use anyhow::Result;
use config::RuntimeConfig;
use ::heartbeat::{resolve_global_interval, HeartbeatService};
use observer::{mark_component_error, mark_component_ok, FileEventSink};
use serde_json::json;
use store::StateStore;
use tokio::time::sleep;
use tracing::{info, warn};

pub async fn run_loop(
    service: Arc<HeartbeatService>,
    config: Arc<RuntimeConfig>,
    store: StateStore,
    sink: FileEventSink,
) -> Result<()> {
    let interval = resolve_global_interval(&config);
    info!(
        interval_sec = interval.as_secs(),
        "heartbeat loop started (HeartbeatService)"
    );

    emit_event(&store, &sink, "heartbeat_started", json!({
        "interval_sec": interval.as_secs(),
    }));

    loop {
        sleep(interval).await;

        let outcome = service.run_scheduled_cycle().await;
        let total_errors = outcome.failed;

        if total_errors == 0 {
            mark_component_ok("heartbeat");
        } else {
            mark_component_error(
                "heartbeat",
                format!("{total_errors} heartbeat run(s) failed"),
            );
        }

        emit_event(&store, &sink, "heartbeat_tick", json!({
            "interval_sec": interval.as_secs(),
            "ran": outcome.ran,
            "skipped": outcome.skipped,
            "failed": outcome.failed,
        }));
    }
}

fn emit_event(store: &StateStore, sink: &FileEventSink, event_type: &str, payload: serde_json::Value) {
    if let Err(err) = sink.emit(event_type, payload.clone()) {
        warn!("failed to emit heartbeat event {event_type}: {err:#}");
    }
    if let Err(err) = store.record_event(event_type, &payload) {
        warn!("failed to persist heartbeat event {event_type}: {err:#}");
    }
}
