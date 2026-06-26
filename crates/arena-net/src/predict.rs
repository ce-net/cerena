//! Client-side prediction + server reconciliation — the heart of responsive feel.
//!
//! The local player must move the instant the key is pressed, not a round-trip
//! later. So the client runs the authoritative simulation locally *for its own
//! player only* and renders the result immediately. Each input is stamped with a
//! monotonic `seq` and held until the server acknowledges it. When an
//! authoritative [`LocalPlayerState`] arrives:
//!
//! 1. snap the local sim to the server's truth (`local.state`),
//! 2. discard inputs the server has already applied (`seq <= last_input_seq`),
//! 3. **replay** the still-unacked inputs on top, re-deriving the present.
//!
//! If step 3 lands somewhere other than where we had been rendering, that gap is a
//! misprediction. Rather than teleport, we fold the gap into a decaying visual
//! error offset so the camera glides to the corrected position over a few frames.
//!
//! ## Why a trait instead of a hard-wired `arena_sim::World`
//!
//! Prediction must use the *exact* movement code the server uses, or it diverges
//! every frame and reconciliation pops constantly. That code lives in `arena-sim`.
//! But replay also needs to *reset* the sim to an arbitrary authoritative state
//! each snapshot, and to do that for `arena_sim::World` we need the map geometry,
//! which only the client glue (`arena-client`) holds. So the predictor talks to a
//! small [`LocalSim`] seam: the netcode owns the prediction/replay loop, and the
//! client supplies a concrete sim. [`SimReplay`] is the production binding to
//! `arena_sim::World`; tests use a trivial stub.

use std::collections::VecDeque;

use arena_protocol::{
    EntityId, MAX_INPUT_LAG_TICKS,
    entity::EntityState,
    input::InputFrame,
    snapshot::LocalPlayerState,
};
use glam::Vec3;

/// The minimal simulation interface the predictor needs to replay the local
/// player. Implemented for the real engine via [`SimReplay`]; mockable in tests.
pub trait LocalSim {
    /// Reset the sim so the local player (`local_id`) is exactly at `authoritative`
    /// and nothing else has stale momentum. Called at the start of every reconcile
    /// so replay always begins from server-confirmed truth.
    fn reset(&mut self, local_id: EntityId, authoritative: &EntityState);
    /// Queue the local player's intent for the next [`LocalSim::step`].
    fn set_input(&mut self, id: EntityId, input: InputFrame);
    /// Advance the sim one fixed tick (movement + collision for the local player).
    fn step(&mut self);
    /// The local player's state after the most recent step, if present.
    fn local_state(&self, id: EntityId) -> Option<EntityState>;
}

/// Largest correction we will smooth visually. A bigger jump (teleport, respawn,
/// big lag spike) is snapped instantly — smoothing it would drift the player
/// through walls or feel like ice-skating.
pub const MAX_SMOOTH_DISTANCE_M: f32 = 2.0;

/// Per-render-frame decay of the visual error offset. 0.85 clears a correction in
/// ~6 frames at 60 fps (~100 ms) — fast enough to feel crisp, slow enough to hide
/// the pop.
pub const ERROR_DECAY: f32 = 0.85;

/// Below this magnitude the error offset is snapped to zero to avoid endless tiny
/// residual jitter.
const ERROR_EPSILON_M: f32 = 0.005;

/// Predicts the local player ahead of the server and reconciles against authority.
pub struct Predictor<S: LocalSim> {
    local_id: EntityId,
    /// The replay sim, advanced as inputs arrive and reset on each reconcile.
    sim: S,
    /// Unacked inputs, oldest first (seq strictly increasing).
    pending: VecDeque<InputFrame>,
    /// Highest input seq the server has confirmed applying.
    last_acked_seq: u32,
    /// The predicted local state at the present tick (post all pending inputs).
    /// `None` until the first reconcile seeds authoritative truth.
    predicted: Option<EntityState>,
    /// Visual-only position offset that decays to zero, hiding mispredictions.
    error_offset: Vec3,
}

impl<S: LocalSim> Predictor<S> {
    /// Create a predictor for `local_id` driven by sim `sim`. The sim need not be
    /// seeded yet — the first [`Predictor::reconcile`] resets it to server truth.
    pub fn new(local_id: EntityId, sim: S) -> Self {
        Self {
            local_id,
            sim,
            pending: VecDeque::new(),
            last_acked_seq: 0,
            predicted: None,
            error_offset: Vec3::ZERO,
        }
    }

    /// Record and immediately apply a new local input, advancing the predicted
    /// present by one tick so rendering reflects the keystroke this frame.
    pub fn push_input(&mut self, frame: InputFrame) {
        self.pending.push_back(frame);
        // Bound the ring: a client whose acks stall (server silent) must not grow
        // unbounded. Anything older than this is hopeless to reconcile anyway.
        while self.pending.len() > MAX_INPUT_LAG_TICKS as usize {
            self.pending.pop_front();
        }

        // Only advance once we have an authoritative base to predict from. The sim
        // is already at the present (post previous inputs), so one step extends it.
        if self.predicted.is_some() {
            self.sim.set_input(self.local_id, frame);
            self.sim.step();
            if let Some(s) = self.sim.local_state(self.local_id) {
                self.predicted = Some(s);
            }
        }
    }

    /// Reconcile against an authoritative snapshot's local-player state.
    pub fn reconcile(&mut self, local: &LocalPlayerState) {
        // Where were we actually rendering? Keep continuity from this point so a
        // correction never visibly pops.
        let prev_render_pos = self
            .predicted
            .as_ref()
            .map(|s| s.pos + self.error_offset);

        // 1. Snap the sim to server truth.
        self.sim.reset(self.local_id, &local.state);
        self.last_acked_seq = local.last_input_seq;

        // 2. Drop inputs the server has already applied (and any older). `pending`
        //    is seq-ordered, so we pop from the front while acked.
        while let Some(front) = self.pending.front() {
            if front.seq <= local.last_input_seq {
                self.pending.pop_front();
            } else {
                break;
            }
        }

        // 3. Replay the still-unacked inputs to re-derive the present.
        for frame in &self.pending {
            self.sim.set_input(self.local_id, *frame);
            self.sim.step();
        }
        let repredicted = self
            .sim
            .local_state(self.local_id)
            .unwrap_or_else(|| local.state.clone());

        // 4. Fold any misprediction into the decaying visual error. Setting the
        //    offset to (old_render - new_base) means the rendered position is
        //    unchanged this frame; it then relaxes to zero over the next few.
        self.error_offset = match prev_render_pos {
            Some(prev) => {
                let off = prev - repredicted.pos;
                if off.length() > MAX_SMOOTH_DISTANCE_M {
                    Vec3::ZERO // too big to smooth: snap to truth
                } else {
                    off
                }
            }
            None => Vec3::ZERO, // first reconcile: nothing to smooth from
        };

        self.predicted = Some(repredicted);
    }

    /// Decay the visual error one render frame toward zero. Call once per rendered
    /// frame (the renderer's cadence, not the sim's).
    pub fn relax_error(&mut self) {
        self.error_offset *= ERROR_DECAY;
        if self.error_offset.length() < ERROR_EPSILON_M {
            self.error_offset = Vec3::ZERO;
        }
    }

    /// The local player's state to render *right now*: the predicted present with
    /// the smoothed error offset applied to position. `None` until the first
    /// reconcile has seeded authoritative truth (the renderer hides the player
    /// until then). Returns `EntityState` by value; the position carries the
    /// in-flight visual correction.
    pub fn predicted_local_state(&self) -> Option<EntityState> {
        self.predicted.as_ref().map(|s| {
            let mut out = s.clone();
            out.pos += self.error_offset;
            out
        })
    }

    /// The local player's *true* predicted position, ignoring the visual smoothing
    /// offset — this is the position the camera's aim/raycast should use so shots
    /// match the authoritative world rather than the cosmetic correction.
    pub fn authoritative_local_state(&self) -> Option<&EntityState> {
        self.predicted.as_ref()
    }

    /// The currently-unacked inputs, oldest first, for inclusion in an outgoing
    /// [`arena_protocol::input::InputBatch`] (re-sending unacked frames makes a
    /// single dropped packet harmless).
    pub fn unacked_frames(&self) -> Vec<InputFrame> {
        self.pending.iter().copied().collect()
    }

    /// Highest input seq the server has confirmed. Diagnostics / batch ack.
    pub fn last_acked_seq(&self) -> u32 {
        self.last_acked_seq
    }

    pub fn local_id(&self) -> EntityId {
        self.local_id
    }
}

/// Production [`LocalSim`] binding to `arena_sim::World`.
///
/// `arena_sim::World` cannot be reset to an arbitrary state from inside the netcode
/// crate — seeding a fresh single-player world needs the map geometry, which lives
/// client-side. So this wrapper holds a `rebuild` closure (supplied by
/// `arena-client` from its `arena_sim::map::MapDef`) that constructs a stripped
/// world containing just the local player at a given authoritative state plus the
/// static geometry needed for movement/collision. Reset rebuilds; replay uses the
/// frozen `set_input` / `tick` / `entities` API.
pub struct SimReplay<F>
where
    F: Fn(&EntityState) -> arena_sim::World,
{
    world: arena_sim::World,
    rebuild: F,
}

impl<F> SimReplay<F>
where
    F: Fn(&EntityState) -> arena_sim::World,
{
    /// `initial` is any starting world (it is overwritten on the first reconcile);
    /// `rebuild` constructs a single-player replay world from an authoritative
    /// state. Both come from `arena-client`, which owns the map.
    pub fn new(initial: arena_sim::World, rebuild: F) -> Self {
        Self {
            world: initial,
            rebuild,
        }
    }

    /// Borrow the underlying world (e.g. to read combat state for ammo prediction).
    pub fn world(&self) -> &arena_sim::World {
        &self.world
    }
}

impl<F> LocalSim for SimReplay<F>
where
    F: Fn(&EntityState) -> arena_sim::World,
{
    fn reset(&mut self, _local_id: EntityId, authoritative: &EntityState) {
        // Rebuild a fresh world seeded at the authoritative state. The closure is
        // responsible for placing the local player and the static map collision.
        self.world = (self.rebuild)(authoritative);
    }

    fn set_input(&mut self, id: EntityId, input: InputFrame) {
        self.world.set_input(id, input);
    }

    fn step(&mut self) {
        // We drive the same fixed-timestep tick the authority uses; the report is
        // irrelevant for local replay (no events are authoritative on the client).
        let _ = self.world.tick();
    }

    fn local_state(&self, id: EntityId) -> Option<EntityState> {
        self.world.entities().get(&id).cloned()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use arena_protocol::{
        entity::{EntityFlags, EntityKind},
        input::Buttons,
        world::Team,
    };

    fn state(id: EntityId, x: f32) -> EntityState {
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

    fn input(seq: u32, forward: bool) -> InputFrame {
        let mut buttons = Buttons::default();
        buttons.set(Buttons::FORWARD, forward);
        InputFrame {
            seq,
            client_tick: seq,
            buttons,
            yaw: 0.0,
            pitch: 0.0,
            weapon_slot: 0,
        }
    }

    /// A trivial deterministic sim used in place of `arena_sim::World`: each step
    /// moves the local player +1 on X while FORWARD is held. Pure, no engine dep.
    pub(crate) struct StubSim {
        state: EntityState,
        pending_input: Option<InputFrame>,
    }

    impl StubSim {
        pub(crate) fn new() -> Self {
            Self {
                state: state(1, 0.0),
                pending_input: None,
            }
        }
    }

    impl LocalSim for StubSim {
        fn reset(&mut self, _local_id: EntityId, authoritative: &EntityState) {
            self.state = authoritative.clone();
            self.pending_input = None;
        }
        fn set_input(&mut self, _id: EntityId, input: InputFrame) {
            self.pending_input = Some(input);
        }
        fn step(&mut self) {
            if let Some(i) = self.pending_input.take() {
                if i.buttons.has(Buttons::FORWARD) {
                    self.state.pos.x += 1.0;
                }
            }
        }
        fn local_state(&self, _id: EntityId) -> Option<EntityState> {
            Some(self.state.clone())
        }
    }

    fn local_player_state(s: EntityState, last_seq: u32) -> LocalPlayerState {
        LocalPlayerState {
            entity: s.id,
            state: s,
            last_input_seq: last_seq,
            ammo_in_mag: 30,
            ammo_reserve: 90,
            respawn_at_tick: 0,
        }
    }

    #[test]
    fn reconcile_drops_acked_inputs() {
        let mut p = Predictor::new(1, StubSim::new());
        p.push_input(input(1, true));
        p.push_input(input(2, true));
        p.push_input(input(3, true));
        assert_eq!(p.unacked_frames().len(), 3);

        // Server has applied through seq 2; only seq 3 remains unacked.
        p.reconcile(&local_player_state(state(1, 2.0), 2));
        let remaining = p.unacked_frames();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].seq, 3);
        assert_eq!(p.last_acked_seq(), 2);
    }

    #[test]
    fn replay_repredicts_present_from_authority() {
        let mut p = Predictor::new(1, StubSim::new());
        p.push_input(input(1, true));
        p.push_input(input(2, true));
        p.push_input(input(3, true));

        // Authority confirms seq 2 left the player at x = 2.0. Replaying the one
        // unacked input (seq 3, FORWARD) must put the prediction at x = 3.0.
        p.reconcile(&local_player_state(state(1, 2.0), 2));
        let pred = p.predicted_local_state().expect("seeded after reconcile");
        assert!((pred.pos.x - 3.0).abs() < 1e-6, "x={}", pred.pos.x);
    }

    #[test]
    fn misprediction_smooths_then_decays_to_zero() {
        let mut p = Predictor::new(1, StubSim::new());
        // Predict forward a bit.
        p.push_input(input(1, true));
        p.reconcile(&local_player_state(state(1, 1.0), 1)); // agrees: no error
        assert_eq!(p.predicted_local_state().unwrap().pos.x, 1.0);

        // Now a small misprediction: we were rendering ~x=1, server says x=0.5 with
        // no unacked inputs. Rendered position should stay continuous (~1.0) then
        // relax toward the authoritative 0.5.
        p.reconcile(&local_player_state(state(1, 0.5), 1));
        let immediately = p.predicted_local_state().unwrap().pos.x;
        assert!((immediately - 1.0).abs() < 1e-6, "should hold prior render");
        for _ in 0..64 {
            p.relax_error();
        }
        let settled = p.predicted_local_state().unwrap().pos.x;
        assert!((settled - 0.5).abs() < 1e-3, "should settle on truth, got {settled}");
    }
}
