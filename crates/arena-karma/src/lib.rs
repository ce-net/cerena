//! # arena-karma
//!
//! The reporting, anti-cheat, and karma subsystem for Cerena. It runs as a
//! service *inside* the session coordinator / `arena-server`; it is pure logic
//! plus `serde_json` persistence — no tokio, no `ce_rs`. The transport layer
//! (`arena-mesh`) feeds it the wire types from [`arena_protocol::karma`] and the
//! authority cross-validation votes from [`arena_protocol::message`], and ships
//! out the [`KarmaUpdate`](arena_protocol::karma::KarmaUpdate) verdicts it emits.
//!
//! ## Threat model
//!
//! Cerena runs an authoritative FPS simulation *on the mesh itself*: any
//! sufficiently-staked node can host a slice of the world. That buys scale, but it
//! splits the cheating problem in two, and this crate defends both halves.
//!
//! ### (a) Cheating clients
//!
//! Aimbot, wallhack, speedhack, and triggerbot are mounted by the *player* against
//! the authority that simulates them. The authority already rejects the impossible
//! (it clamps look-deltas, gates fire-rate, and corrects teleporting movement), and
//! every rejection is counted into a per-round [`CheatTelemetry`] record. On top of
//! that, honest players file [`Report`]s. Neither source is conclusive on its own:
//!
//! - Telemetry is *statistical*. A great player has high accuracy; an aimbot has
//!   inhuman accuracy *sustained over a large sample*. The [`detector`] weighs every
//!   signal by its sample size and fuses them into a probabilistic
//!   [`SuspicionScore`](detector::SuspicionScore) — never a single-signal ban.
//! - Reports are a *social prior*, easily brigaded. The [`reports`] aggregator weights
//!   each report by the reporter's own karma (a quarantined reporter counts for ~0)
//!   and decays it over time, and it actively flags brigading as its own offense.
//!
//! The [`ledger`] fuses detector suspicion with report pressure into a karma delta and
//! emits an update only when karma crosses an enforcement band
//! ([`KarmaAction`](arena_protocol::karma::KarmaAction)). Escalation is graduated:
//! `Quarantine` (matched only with other suspects) → `TempBan` → `PermBan`.
//!
//! ### (b) Cheating authorities
//!
//! A far nastier attacker bonds stake, wins an [`AuthorityClaim`] for a zone, and then
//! *simulates it dishonestly* — fabricating hits for a confederate, or denying them for
//! a rival. No amount of client-side telemetry catches this, because the authority owns
//! the telemetry. The defense is redundancy: a sample of other authorities run a *shadow
//! simulation* of the same tick from the same inputs ([`VerifyTick`]) and report whether
//! their result hash matches ([`VerifyResult`]). The [`crossval`] module collects those
//! votes; if a quorum disagrees, the authority's tick is `DISPUTED` and a
//! [`Verdict`](crossval::Verdict) recommends slashing the authority's bonded stake (via
//! ce-gov) and reassigning the zone. Sustained disputes also dock the authority's karma —
//! the same scarce-identity reputation a cheating client burns.
//!
//! ## Why karma sticks
//!
//! Karma keys off the CE node id, an Ed25519 public key. Minting a fresh identity is
//! cheap, but *earning* karma back from the [`KARMA_DEFAULT`] starting line — or buying
//! into authority work, which requires bonded on-chain stake — is not. That asymmetry is
//! what makes a ban durable: a banned cheat can make a new key, but it lands back in the
//! quarantine pool with strangers and no stake, exactly where it cannot ruin honest games.
//!
//! [`Report`]: arena_protocol::karma::Report
//! [`CheatTelemetry`]: arena_protocol::karma::CheatTelemetry
//! [`KarmaUpdate`]: arena_protocol::karma::KarmaUpdate
//! [`KARMA_DEFAULT`]: arena_protocol::karma::KARMA_DEFAULT
//! [`AuthorityClaim`]: arena_protocol::message::AuthorityMsg::AuthorityClaim
//! [`VerifyTick`]: arena_protocol::message::AuthorityMsg::VerifyTick
//! [`VerifyResult`]: arena_protocol::message::AuthorityMsg::VerifyResult

pub mod crossval;
pub mod detector;
pub mod ledger;
pub mod reports;

pub use crossval::{CrossValidator, Verdict};
pub use detector::{CheatDetector, SuspicionScore};
pub use ledger::KarmaLedger;
pub use reports::ReportAggregator;
