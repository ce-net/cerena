//! # arena-replica
//!
//! The unit each participant runs so that **the players present in a zone ARE the
//! servers** — the same model spacegame proved (`replica.rs` + `replication.rs`),
//! brought to Cerena. There is no trusted host: every replica of a zone advances
//! the SAME [`arena_sim::World`] from the SAME tick-tagged input log, so honest
//! replicas compute identical state, and a periodic quorum over each replica's
//! [`arena_sim::World::state_hash`] catches a desynced or cheating node — the odd
//! one out merges to the agreed snapshot.
//!
//! This crate is **mesh-free and wasm-clean**: it depends only on `arena-protocol`
//! and `arena-sim` (no tokio, no `ce_rs`, no wgpu), so the identical engine runs in
//! the browser tab, the native client, a headless donor node, and the relay. Every
//! participant is the same kind of replica. The mesh I/O that feeds it (publishing
//! tick-tagged inputs, gossiping state proofs, fetching snapshot blobs) lives in
//! `arena-mesh`/`arena-client`, never here.
//!
//! - [`clock`] — the shared wall-clock tick (`TICK_EPOCH_MS` + [`clock::tick_at`])
//!   and the input-delay budget, so every replica agrees what tick "now" is.
//! - [`replica`] — [`replica::Replica`], the deterministic per-zone engine.
//! - [`quorum`] — [`quorum::agree`] + [`quorum::Verdict`], the state-hash consensus.

pub mod clock;
pub mod quorum;
pub mod replica;

pub use clock::{tick_at, INPUT_DELAY, TICK_EPOCH_MS, TICK_MS};
pub use quorum::{agree, Agreement, Verdict};
pub use replica::Replica;
