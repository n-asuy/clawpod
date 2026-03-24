use std::sync::Arc;

use ::heartbeat::{HeartbeatLoopControl, HeartbeatLoopSettings, HeartbeatService};
use anyhow::Result;
use observer::{mark_component_disabled, mark_component_error, mark_component_ok, FileEventSink};
use serde_json::json;
use store::StateStore;
use tokio::time::sleep;
use tracing::{info, warn};

pub async fn run_loop(
    service: Arc<HeartbeatService>,
    control: HeartbeatLoopControl,
    store: StateStore,
    sink: FileEventSink,
) -> Result<()> {
    let mut rx = control.subscribe();
    let mut previous_settings: Option<HeartbeatLoopSettings> = None;

    loop {
        let settings = *rx.borrow_and_update();
        let settings_changed = previous_settings != Some(settings);

        if settings_changed {
            let event_type = if previous_settings.is_none() {
                "heartbeat_started"
            } else {
                "heartbeat_settings_updated"
            };
            previous_settings = Some(settings);
            info!(
                enabled = settings.enabled,
                interval_sec = settings.interval_sec,
                "heartbeat loop configured (HeartbeatService)"
            );
            emit_event(
                &store,
                &sink,
                event_type,
                json!({
                    "enabled": settings.enabled,
                    "interval_sec": settings.interval_sec,
                }),
            );
        }

        if !settings.enabled {
            if settings_changed {
                mark_component_disabled("heartbeat", "heartbeat disabled");
            }
            if rx.changed().await.is_err() {
                return Ok(());
            }
            continue;
        }

        if settings_changed {
            mark_component_ok("heartbeat");
        }

        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
            _ = sleep(settings.interval()) => {
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
                    "enabled": settings.enabled,
                    "interval_sec": settings.interval_sec,
                    "ran": outcome.ran,
                    "skipped": outcome.skipped,
                    "failed": outcome.failed,
                }));
            }
        }
    }
}

fn emit_event(
    store: &StateStore,
    sink: &FileEventSink,
    event_type: &str,
    payload: serde_json::Value,
) {
    if let Err(err) = sink.emit(event_type, payload.clone()) {
        warn!("failed to emit heartbeat event {event_type}: {err:#}");
    }
    if let Err(err) = store.record_event(event_type, &payload) {
        warn!("failed to persist heartbeat event {event_type}: {err:#}");
    }
}
