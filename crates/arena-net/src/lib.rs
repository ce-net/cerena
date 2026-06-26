//! # arena-net — CE Arena netcode
//!
//! Pure netcode logic for the distributed FPS. No tokio, no `ce_rs`, no `wgpu`:
//! everything here is deterministic, allocation-light, and `wasm32` clean so the
//! exact same code runs in a browser client and in a native authority. Transport
//! (mesh pub/sub, RPC) lives in `arena-mesh`; this crate only turns world state
//! into wire snapshots and turns wire snapshots back into something renderable
//! and responsive.
//!
//! ## The netcode model
//!
//! CE Arena uses the standard server-authoritative recipe proven by Quake 3 →
//! Source → Overwatch, adapted to a mesh where the "server" is whichever node
//! currently holds the player's zone authority.
//!
//! ### 1. Delta snapshots against an acked baseline (server side)
//!
//! The authority simulates at [`arena_protocol::TICK_HZ`] (64 Hz) but only ships
//! snapshots at [`arena_protocol::SNAPSHOT_HZ`] (20 Hz). Each snapshot is scoped
//! to one client's area of interest (AOI) and **delta-encoded against a baseline
//! the client has already acknowledged**. Steady-state bandwidth is therefore
//! proportional to *motion in view*, not to the global player count — the key to
//! thousands of players. [`server::SnapshotEncoder`] does the encoding;
//! [`baseline::BaselineStore`] remembers, per client, exactly which full entity
//! sets it has sent so the diff is always against ground truth the client holds.
//! When the client has no usable baseline (join, packet loss, eviction) the
//! encoder falls back to a keyframe (`baseline_tick == 0`, every entity a spawn).
//!
//! ### 2. Client prediction + server reconciliation (client side)
//!
//! Waiting a full round-trip to see your own movement feels terrible. So the
//! client runs the *same* simulation locally for its own player only
//! ([`predict::Predictor`]): it applies each [`arena_protocol::input::InputFrame`]
//! immediately and renders the result, so input feels instant. Every input is
//! stamped with a `seq` and kept until the server acknowledges it. When a snapshot
//! arrives, the client throws away its predicted local state, snaps the local sim
//! to the server's authoritative `LocalPlayerState`, drops the now-acked inputs,
//! and **replays** the still-unacked inputs on top — re-deriving the present. If
//! the authoritative result differs from what was predicted (misprediction), the
//! discrepancy is folded into a decaying visual error offset so the correction is
//! smoothed over a few frames instead of popping.
//!
//! ### 3. Entity interpolation of remote players (client side)
//!
//! Remote players are never predicted (we don't know their intent). Instead the
//! client buffers their authoritative states and renders them slightly in the
//! past — `render_tick = now - INTERP_DELAY` — linearly interpolating position and
//! angles between the two bracketing snapshots ([`interp::InterpolationBuffer`]).
//! The deliberate delay (~2 snapshots) guarantees there are almost always two
//! samples to interpolate between, hiding the 20 Hz snapshot cadence behind smooth
//! 60+ fps motion.
//!
//! ### 4. Lag compensation (where the timings come from)
//!
//! Hit registration uses lag compensation, but the *rewind itself happens in
//! `arena-sim`* on the authority: when it processes a client's fire input it
//! rewinds other entities to the world the firing client actually saw. The inputs
//! that drive that rewind carry the client's clock estimate, and the budget is
//! [`arena_protocol::MAX_REWIND_MS`]. This crate's job is only to keep those
//! timings honest: [`clock::ClockSync`] estimates the server tick from snapshot
//! timestamps + smoothed RTT so the `client_tick` stamped on each input is a
//! faithful statement of "this is the world I was looking at".
//!
//! ## Crate layout
//!
//! - [`baseline`] — per-client sent-snapshot history for delta baselines (server).
//! - [`server`]   — [`server::SnapshotEncoder`]: world state → AOI delta snapshot.
//! - [`clock`]    — client clock/RTT estimation → server-tick estimate.
//! - [`interp`]   — remote-entity interpolation buffer.
//! - [`predict`]  — local prediction + reconciliation (the responsive-feel core).
//! - [`client`]   — [`client::ClientWorld`]: glue tying the above together for a
//!   renderer.

pub mod baseline;
pub mod client;
pub mod clock;
pub mod interp;
pub mod predict;
pub mod server;

pub use baseline::BaselineStore;
pub use client::ClientWorld;
pub use clock::ClockSync;
pub use interp::{INTERP_DELAY_TICKS, InterpolationBuffer};
pub use predict::{LocalSim, Predictor, SimReplay};
pub use server::SnapshotEncoder;
