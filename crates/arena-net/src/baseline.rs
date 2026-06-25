//! Per-client baseline history for delta snapshot encoding.
//!
//! To delta-encode a snapshot the server must diff the current world against the
//! exact entity set the *client already holds*. UDP-style mesh traffic is lossy
//! and reordered, so we cannot assume the client has the most recent snapshot —
//! it might still be acking one from several frames ago. [`BaselineStore`] keeps a
//! short ring of every full (AOI-scoped) entity set we have sent each client,
//! keyed by tick, plus the highest tick that client has acknowledged. The encoder
//! diffs against the acked tick if its snapshot is still in the ring, otherwise it
//! ships a keyframe.

use std::collections::{HashMap, VecDeque};

use arena_protocol::{EntityId, NodeId, Tick, entity::EntityState};

/// How many recent snapshots we retain per client. At 20 Hz this is ~1.6 s of
/// history — comfortably longer than any reasonable ack latency, and bounded so a
/// client that vanishes cannot grow our memory without limit.
pub const BASELINE_RING: usize = 32;

/// One client's sent-snapshot history.
#[derive(Debug, Default)]
struct ClientBaselines {
    /// (tick, full AOI entity set we sent at that tick), oldest first.
    ring: VecDeque<(Tick, HashMap<EntityId, EntityState>)>,
    /// Highest tick the client has confirmed receiving.
    acked: Option<Tick>,
}

impl ClientBaselines {
    fn record(&mut self, tick: Tick, entities: HashMap<EntityId, EntityState>) {
        // Replace in place if we somehow re-send the same tick, else append.
        if let Some(slot) = self.ring.iter_mut().find(|(t, _)| *t == tick) {
            slot.1 = entities;
        } else {
            self.ring.push_back((tick, entities));
            while self.ring.len() > BASELINE_RING {
                self.ring.pop_front();
            }
        }
    }

    fn get(&self, tick: Tick) -> Option<&HashMap<EntityId, EntityState>> {
        self.ring
            .iter()
            .find(|(t, _)| *t == tick)
            .map(|(_, set)| set)
    }
}

/// Tracks, per client, the snapshots we have sent and what they have acked, so the
/// [`crate::server::SnapshotEncoder`] can pick a valid delta baseline.
#[derive(Debug, Default)]
pub struct BaselineStore {
    clients: HashMap<NodeId, ClientBaselines>,
}

impl BaselineStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember the full entity set we just sent `client` at `tick`. This becomes a
    /// candidate baseline once the client acks it.
    pub fn record(
        &mut self,
        client: &NodeId,
        tick: Tick,
        entities: HashMap<EntityId, EntityState>,
    ) {
        self.clients
            .entry(client.clone())
            .or_default()
            .record(tick, entities);
    }

    /// The full entity set we sent `client` at `tick`, if still retained.
    pub fn get(&self, client: &NodeId, tick: Tick) -> Option<&HashMap<EntityId, EntityState>> {
        self.clients.get(client).and_then(|c| c.get(tick))
    }

    /// The highest tick `client` has acknowledged receiving, if any.
    pub fn latest_acked(&self, client: &NodeId) -> Option<Tick> {
        self.clients.get(client).and_then(|c| c.acked)
    }

    /// Record a client ack. Monotonic: a stale (reordered) ack never lowers the
    /// high-water mark.
    pub fn ack(&mut self, client: &NodeId, tick: Tick) {
        let c = self.clients.entry(client.clone()).or_default();
        c.acked = Some(match c.acked {
            Some(prev) if prev >= tick => prev,
            _ => tick,
        });
    }

    /// Drop all history for a client that has left. Called by the authority on
    /// disconnect / hand-off so memory tracks the live roster.
    pub fn forget(&mut self, client: &NodeId) {
        self.clients.remove(client);
    }

    /// Number of tracked clients (diagnostics).
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::{
        entity::{EntityFlags, EntityKind},
        world::Team,
    };
    use glam::Vec3;

    fn ent(id: EntityId, x: f32) -> EntityState {
        EntityState {
            id,
            kind: EntityKind::Player,
            pos: Vec3::new(x, 0.0, 0.0),
            vel: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            flags: EntityFlags::default(),
            team: Team::None,
            health: 100,
            armor: 0,
            weapon: 0,
            owner: String::new(),
        }
    }

    #[test]
    fn record_get_ack_roundtrip() {
        let mut store = BaselineStore::new();
        let client: NodeId = "peerA".into();
        let mut set = HashMap::new();
        set.insert(1, ent(1, 3.0));
        store.record(&client, 10, set);

        assert!(store.get(&client, 10).is_some());
        assert!(store.get(&client, 11).is_none());
        assert_eq!(store.latest_acked(&client), None);

        store.ack(&client, 10);
        assert_eq!(store.latest_acked(&client), Some(10));
        // A stale ack must not move the high-water mark backwards.
        store.ack(&client, 5);
        assert_eq!(store.latest_acked(&client), Some(10));
    }

    #[test]
    fn ring_evicts_oldest() {
        let mut store = BaselineStore::new();
        let client: NodeId = "peerB".into();
        for t in 0..(BASELINE_RING as Tick + 5) {
            let mut set = HashMap::new();
            set.insert(1, ent(1, t as f32));
            store.record(&client, t + 1, set); // ticks 1.. to keep them non-zero
        }
        // The earliest ticks should have been evicted.
        assert!(store.get(&client, 1).is_none());
        let newest = BASELINE_RING as Tick + 5;
        assert!(store.get(&client, newest).is_some());
    }
}
