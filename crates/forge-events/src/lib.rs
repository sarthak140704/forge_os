//! Event bus + append-only event store.
//!
//! The bus and the store together form the event-sourcing spine. Every state
//! change in the runtime becomes a `ForgeEvent`, which is:
//!   1. Appended to the SQLite `events` table (durable, sequence-numbered).
//!   2. Fanned out via `tokio::sync::broadcast` to in-process subscribers.
//!
//! The Tauri layer subscribes to the broadcast channel and forwards each event
//! to the webview as `forge://event`.

use forge_domain::{EventId, EventEnvelope, ForgeEvent};
use forge_persistence::EventStore;
use std::sync::Arc;
use thiserror::Error;
use time::OffsetDateTime;
use tokio::sync::broadcast;

#[derive(Debug, Error)]
pub enum EventBusError {
    #[error("persistence error: {0}")]
    Persist(#[from] forge_persistence::PersistenceError),
    #[error("no active subscribers to broadcast channel (event still persisted)")]
    NoSubscribers,
}

/// Broadcast channel + backing store. Cheaply cloneable; hand out clones to
/// every service that needs to publish or subscribe.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EventEnvelope>,
    store: Arc<dyn EventStore>,
}

impl EventBus {
    pub fn new(store: Arc<dyn EventStore>, capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx, store }
    }

    /// Persist then broadcast. Persistence is the source of truth; broadcast
    /// is best-effort (may drop for slow subscribers).
    pub async fn publish(&self, event: ForgeEvent) -> Result<EventEnvelope, EventBusError> {
        let ts = OffsetDateTime::now_utc();
        let seq = self.store.append(&event, ts).await?;
        let envelope = EventEnvelope { seq, ts, event };
        // ignore send error — no subscribers is fine; persistence already happened.
        let _ = self.tx.send(envelope.clone());
        tracing::debug!(seq = ?envelope.seq, "event published");
        Ok(envelope)
    }

    /// Subscribe to future events. Historical events must be read from the
    /// store via `replay_since`.
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.tx.subscribe()
    }

    /// Read all events since a given sequence number (exclusive). Used by the
    /// UI on reconnect to catch up.
    pub async fn replay_since(&self, since: Option<EventId>) -> Result<Vec<EventEnvelope>, EventBusError> {
        let events = self.store.read_since(since).await?;
        Ok(events)
    }
}
