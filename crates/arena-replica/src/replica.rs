//! The deterministic per-zone engine. Given the same tick-tagged input log, every
//! [`Replica`] of a zone ends in the same [`arena_sim::World`] state — independent of
//! the order packets arrived in, or which node is simulating. That determinism is
//! what makes "the players present ARE the servers, and they agree" possible.

use std::collections::BTreeMap;

use arena_protocol::replica::{ReplicaInput, TaggedInput};
use arena_protocol::world::Team;
use arena_protocol::{NodeId, Tick};
use arena_sim::World;

use crate::clock::INPUT_DELAY;

/// A long stall (a backgrounded tab, or the first frame measured against the shared
/// wall-clock tick) must not spin the sim through thousands of ticks. Cap the
/// catch-up; a gap larger than this is a desync the quorum merge / a fresh snapshot
/// repairs, not something to simulate frame-by-frame. ~250 ms at 64 Hz.
const MAX_CATCHUP: Tick = 16;

/// A deterministic replica of one zone's simulation, driven by a tick-tagged input
/// log. The `World` is the single source of truth — render from [`Replica::world`].
pub struct Replica {
    world: World,
    /// Inputs scheduled for a future tick: `tick -> [(author, seq, input)]`. Drained
    /// as the sim reaches each tick. A `BTreeMap` so the entry for the exact tick
    /// being simulated is cheap to find and remove.
    scheduled: BTreeMap<Tick, Vec<(NodeId, u32, ReplicaInput)>>,
}

impl Replica {
    /// Wrap an initial `world` — a fresh `World::new`, or one restored from a
    /// [`arena_sim::ZoneSnapshot`]. All replicas of a zone MUST start from the same
    /// initial state (same tick, same entities) for their inputs to converge.
    pub fn new(world: World) -> Self {
        Replica { world, scheduled: BTreeMap::new() }
    }

    /// The next tick this replica will simulate.
    pub fn tick(&self) -> Tick {
        self.world.current_tick()
    }

    /// The authoritative world (read-only) — render from this; it is the same on
    /// every honest replica.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// This replica's deterministic state hash — its claim about the zone, for the
    /// quorum merge (see [`crate::quorum::agree`]).
    pub fn state_hash(&self) -> [u8; 32] {
        self.world.state_hash()
    }

    /// Set this replica's tick directly (e.g. to the shared wall-clock tick at
    /// creation, or after adopting a snapshot), so [`Replica::advance_to`] steps only
    /// the elapsed ticks rather than from zero.
    pub fn set_tick(&mut self, tick: Tick) {
        self.world.set_tick(tick);
    }

    /// Replace the world wholesale — the merge-to-quorum action. An out-voted replica
    /// adopts the agreed snapshot here, then advances normally so it re-converges with
    /// the quorum. Inputs already scheduled for the future are kept (they still apply
    /// at their canonical ticks); stale ones are dropped on the next advance.
    pub fn reseed(&mut self, world: World) {
        self.world = world;
    }

    /// Schedule a tick-tagged input received from the zone's input stream, authored by
    /// `author` (the authenticated mesh sender). An input for a tick already simulated
    /// is dropped — too late to apply deterministically; the quorum merge repairs a
    /// replica that fell behind, not a late-applied input that would itself diverge.
    /// Returns `false` if the input was too late.
    pub fn schedule(&mut self, author: NodeId, ti: TaggedInput) -> bool {
        if ti.tick < self.world.current_tick() {
            return false;
        }
        self.scheduled.entry(ti.tick).or_default().push((author, ti.seq, ti.input));
        true
    }

    /// Schedule one of THIS node's inputs at the canonical future tick
    /// (`current + INPUT_DELAY`) and return the [`TaggedInput`] to broadcast verbatim,
    /// so every replica (including this one) applies it at exactly that tick.
    pub fn schedule_local(&mut self, author: NodeId, seq: u32, input: ReplicaInput) -> TaggedInput {
        let at = self.world.current_tick().saturating_add(INPUT_DELAY);
        let ti = TaggedInput { tick: at, seq, input: input.clone() };
        self.scheduled.entry(at).or_default().push((author, seq, input));
        ti
    }

    /// Advance the simulation up to (but not including) `target`, applying each tick's
    /// scheduled inputs in canonical `(author, seq)` order before stepping that tick.
    /// Deterministic: given the same scheduled inputs, every replica ends in the same
    /// state regardless of arrival order or host. No-op if already at or past `target`.
    pub fn advance_to(&mut self, target: Tick) {
        if target.saturating_sub(self.world.current_tick()) > MAX_CATCHUP {
            let floor = target.saturating_sub(MAX_CATCHUP);
            self.scheduled.retain(|&t, _| t >= floor);
            self.world.set_tick(floor);
        }
        while self.world.current_tick() < target {
            let t = self.world.current_tick();
            if let Some(mut inputs) = self.scheduled.remove(&t) {
                // Canonical order so packet arrival order can't change the result.
                inputs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                for (author, _seq, input) in inputs {
                    apply(&mut self.world, &author, input);
                }
            }
            self.world.tick();
        }
    }
}

/// Apply one participant's intent to the world. Mirrors the meaningful client
/// messages; deterministic given the world state and the input.
fn apply(world: &mut World, author: &NodeId, input: ReplicaInput) {
    match input {
        ReplicaInput::Join { team_pref, .. } => {
            // Idempotent: a duplicate Join for an already-present player is ignored.
            if world.player_entity(author).is_none() {
                let team = team_pref.unwrap_or(Team::Red);
                world.spawn_player(author.clone(), team);
            }
        }
        ReplicaInput::Input(frame) => {
            // Intent for a player who has not joined this zone yet is dropped (an RPG
            // requires an explicit Join to allocate the body + starter loadout).
            if let Some(id) = world.player_entity(author) {
                world.set_input(id, frame);
            }
        }
        ReplicaInput::Leave => {
            if let Some(id) = world.player_entity(author) {
                world.remove_entity(id);
            }
        }
        // Cross-zone movement is an orchestration decision (which zone's replicas
        // adopt the player), handled by the host loop, not the pure sim step.
        ReplicaInput::ZoneSwitch { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::default_pack;
    use arena_content::registry::ContentRegistry;
    use arena_protocol::input::{Buttons, InputFrame};
    use arena_sim::map::MapDef;

    fn world() -> World {
        let content = ContentRegistry::new(1, default_pack()).expect("default pack is valid");
        World::new(MapDef::test_arena(), content)
    }

    fn join(tick: Tick, seq: u32) -> TaggedInput {
        TaggedInput { tick, seq, input: ReplicaInput::Join { team_pref: None, name: "p".into() } }
    }

    fn mv(tick: Tick, seq: u32, yaw: f32) -> TaggedInput {
        TaggedInput {
            tick,
            seq,
            input: ReplicaInput::Input(InputFrame {
                seq,
                client_tick: tick,
                buttons: Buttons(Buttons::FORWARD),
                yaw,
                pitch: 0.0,
                weapon_slot: 0,
            }),
        }
    }

    #[test]
    fn two_replicas_from_the_same_inputs_reach_identical_state() {
        // The premise of "everyone runs the server and they agree": two replicas, the
        // SAME tick-tagged inputs scheduled in DIFFERENT order, must end identical. If
        // this fails the quorum merge would falsely flag honest players — so this is
        // the load-bearing test for the whole model.
        let mut a = Replica::new(world());
        let mut b = Replica::new(world());
        assert_eq!(a.state_hash(), b.state_hash(), "identical fresh worlds start equal");

        let inputs = vec![
            ("p1".to_string(), join(2, 1)),
            ("p2".to_string(), join(2, 1)),
            ("p1".to_string(), mv(5, 2, 0.4)),
            ("p2".to_string(), mv(4, 2, -0.6)),
            ("p1".to_string(), mv(6, 3, 0.8)),
        ];
        for (author, ti) in inputs.iter().cloned() {
            a.schedule(author, ti);
        }
        for (author, ti) in inputs.iter().rev().cloned() {
            b.schedule(author, ti);
        }
        // Advance within one catch-up window so every scheduled input is applied.
        a.advance_to(12);
        b.advance_to(12);
        assert_eq!(a.tick(), 12);
        assert_eq!(
            a.state_hash(),
            b.state_hash(),
            "same inputs => same state, independent of arrival order"
        );
    }

    #[test]
    fn a_forged_input_diverges_the_hash_so_the_merge_can_catch_it() {
        // A replica that injects an input no one else has (a phantom player) computes a
        // different hash and is the odd one out — what quorum::agree turns into a merge.
        let mut honest = Replica::new(world());
        let mut cheat = Replica::new(world());
        let shared = ("p1".to_string(), join(1, 1));
        honest.schedule(shared.0.clone(), shared.1.clone());
        cheat.schedule(shared.0, shared.1);
        cheat.schedule("ghost".to_string(), join(2, 1));
        honest.advance_to(10);
        cheat.advance_to(10);
        assert_ne!(honest.state_hash(), cheat.state_hash(), "a forged join shows as a divergent hash");
    }

    #[test]
    fn an_input_for_an_already_simulated_tick_is_rejected() {
        let mut r = Replica::new(world());
        r.advance_to(10);
        assert!(!r.schedule("p1".into(), join(4, 1)), "a simulated tick can't accept a late input");
        assert!(r.schedule("p1".into(), join(12, 1)), "a future tick still accepts inputs");
    }

    #[test]
    fn schedule_local_targets_the_input_delay_tick() {
        let mut r = Replica::new(world());
        r.advance_to(100);
        let ti = r.schedule_local("me".into(), 1, ReplicaInput::Leave);
        assert_eq!(ti.tick, 100 + INPUT_DELAY, "local input is scheduled INPUT_DELAY ticks ahead");
    }

    #[test]
    fn reseed_then_advance_reconverges_with_the_quorum() {
        // An out-voted replica adopts the agreed snapshot and re-converges. Advance
        // within one catch-up window so every scheduled join actually applies (a jump
        // larger than MAX_CATCHUP would deliberately drop stale inputs).
        let mut good = Replica::new(world());
        good.schedule("p1".into(), join(1, 1));
        good.advance_to(12);

        // A desynced replica that took a different path (a phantom player).
        let mut bad = Replica::new(world());
        bad.schedule("p1".into(), join(1, 1));
        bad.schedule("phantom".into(), join(2, 1));
        bad.advance_to(12);
        assert_ne!(good.state_hash(), bad.state_hash());

        // Merge: import the good replica's snapshot, set the tick, advance — equal again.
        let snap = good.world().export_snapshot();
        let restored = World::import_snapshot(MapDef::test_arena(), {
            ContentRegistry::new(1, default_pack()).unwrap()
        }, snap);
        bad.reseed(restored);
        bad.set_tick(good.tick());
        assert_eq!(good.state_hash(), bad.state_hash(), "after reseed the replica matches the quorum");
    }
}
