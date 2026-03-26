use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use domain::RunEvent;
use tokio::sync::broadcast;

/// In-memory buffer for run events with broadcast capability.
///
/// Events are held per run_id and evicted after the run completes and the
/// retention window is exceeded. The broadcast channel enables real-time
/// SSE streaming to the Office UI.
pub struct RunEventBuffer {
    inner: Mutex<BufferInner>,
    sender: broadcast::Sender<RunEvent>,
}

struct BufferInner {
    /// Events keyed by run_id, ordered by seq.
    runs: HashMap<String, Vec<RunEvent>>,
    /// Completed run IDs in FIFO order for eviction.
    completed: VecDeque<String>,
    max_completed_runs: usize,
    max_events_per_run: usize,
}

impl RunEventBuffer {
    pub fn new(max_completed_runs: usize, max_events_per_run: usize) -> Self {
        let (sender, _) = broadcast::channel(512);
        Self {
            inner: Mutex::new(BufferInner {
                runs: HashMap::new(),
                completed: VecDeque::new(),
                max_completed_runs,
                max_events_per_run,
            }),
            sender,
        }
    }

    /// Push an event into the buffer and broadcast it.
    pub fn push(&self, event: RunEvent) {
        let _ = self.sender.send(event.clone());
        let mut inner = self.inner.lock().expect("run event buffer lock poisoned");
        let max = inner.max_events_per_run;
        let events = inner.runs.entry(event.run_id.clone()).or_default();
        if events.len() < max {
            events.push(event);
        }
    }

    /// Mark a run as completed so it can be evicted when the buffer is full.
    pub fn mark_completed(&self, run_id: &str) {
        let mut inner = self.inner.lock().expect("run event buffer lock poisoned");
        inner.completed.push_back(run_id.to_string());
        while inner.completed.len() > inner.max_completed_runs {
            if let Some(evicted) = inner.completed.pop_front() {
                inner.runs.remove(&evicted);
            }
        }
    }

    /// Get all buffered events for a run.
    pub fn get_events(&self, run_id: &str) -> Vec<RunEvent> {
        let inner = self.inner.lock().expect("run event buffer lock poisoned");
        inner.runs.get(run_id).cloned().unwrap_or_default()
    }

    /// Get the event count for a run.
    pub fn event_count(&self, run_id: &str) -> usize {
        let inner = self.inner.lock().expect("run event buffer lock poisoned");
        inner.runs.get(run_id).map_or(0, |v| v.len())
    }

    /// Subscribe to the broadcast channel for live events.
    pub fn subscribe(&self) -> broadcast::Receiver<RunEvent> {
        self.sender.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::RunEventType;

    fn make_event(run_id: &str, seq: u32) -> RunEvent {
        RunEvent {
            run_id: run_id.to_string(),
            seq,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            event_type: RunEventType::TextChunk,
            data: serde_json::json!({"text": "hello"}),
        }
    }

    #[test]
    fn push_and_get_events() {
        let buf = RunEventBuffer::new(10, 100);
        buf.push(make_event("r1", 1));
        buf.push(make_event("r1", 2));
        buf.push(make_event("r2", 1));

        assert_eq!(buf.get_events("r1").len(), 2);
        assert_eq!(buf.get_events("r2").len(), 1);
        assert_eq!(buf.get_events("r3").len(), 0);
    }

    #[test]
    fn eviction_on_completion() {
        let buf = RunEventBuffer::new(2, 100);
        buf.push(make_event("r1", 1));
        buf.push(make_event("r2", 1));
        buf.push(make_event("r3", 1));

        buf.mark_completed("r1");
        buf.mark_completed("r2");
        // Both still available (max = 2)
        assert_eq!(buf.get_events("r1").len(), 1);

        buf.mark_completed("r3");
        // r1 evicted
        assert_eq!(buf.get_events("r1").len(), 0);
        assert_eq!(buf.get_events("r2").len(), 1);
        assert_eq!(buf.get_events("r3").len(), 1);
    }

    #[test]
    fn max_events_per_run() {
        let buf = RunEventBuffer::new(10, 3);
        for i in 1..=10 {
            buf.push(make_event("r1", i));
        }
        assert_eq!(buf.get_events("r1").len(), 3);
    }

    #[test]
    fn event_count() {
        let buf = RunEventBuffer::new(10, 100);
        buf.push(make_event("r1", 1));
        buf.push(make_event("r1", 2));
        assert_eq!(buf.event_count("r1"), 2);
        assert_eq!(buf.event_count("r2"), 0);
    }

    #[test]
    fn broadcast_delivers_events() {
        let buf = RunEventBuffer::new(10, 100);
        let mut rx = buf.subscribe();
        buf.push(make_event("r1", 1));
        let received = rx.try_recv().unwrap();
        assert_eq!(received.run_id, "r1");
        assert_eq!(received.seq, 1);
    }
}
