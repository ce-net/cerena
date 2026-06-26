//! # arena-mesh — CE mesh integration for Cerena
//!
//! This crate is the seam between the abstract game protocol (`arena-protocol`) and
//! the **real CE mesh** (`ce_rs`). It sits *beneath* the simulation, networking, and
//! server crates and knows nothing about gameplay: it only answers four questions.
//!
//! 1. **Where does a message go?** — [`transport`] adapts the protocol [`Envelope`]
//!    onto concrete `ce_rs` transport calls (`publish`, `send_message`, `request`,
//!    and the `ce_rs::serve` RPC loop). It is deliberately dumb: `Envelope` in,
//!    `Envelope` out, no game logic.
//!
//! 2. **Who is authoritative for a zone?** — [`authority`] computes the owner of
//!    each [`world::ZoneId`] with **stake-weighted rendezvous (HRW) hashing** over the
//!    set of candidate node ids. Every node runs the identical, directory-free
//!    function and arrives at the same owner, so there is no central matchmaker and
//!    no lookup table to keep coherent. Stake (on-chain bond) biases ownership toward
//!    well-bonded nodes — raising the Sybil cost of seizing a zone — but the bias is
//!    *log-scaled*, so a small node still wins a fair share of zones and no single
//!    node can monopolise the whole map. The full ranking doubles as the failover
//!    order: if the primary dies, the next node in the ranking adopts the zone (the
//!    handoff itself is sequenced by `AuthorityClaim` epochs in `arena-server`).
//!
//! 3. **May this player join?** — [`ticket`] verifies a [`auth::SessionTicket`]: it
//!    checks expiry and the issuer's Ed25519 signature over the ticket's canonical
//!    bytes. Identity *is* the CE node id (a public key), so a verified ticket binds
//!    a session seat to a scarce, on-chain identity that bans and karma can stick to.
//!
//! 4. **Which nodes can host?** — [`discovery`] reads the CE capacity atlas and keeps
//!    the entries that advertise the `"arena"` capability tag, turning them into the
//!    [`Candidate`] set the authority assignment consumes.
//!
//! Everything here runs on `tokio` and talks only to the **local** CE node over its
//! HTTP API via `ce_rs::CeClient`; peer-to-peer reachability, NAT traversal, and relay
//! routing are the node's job, not ours.

pub mod authority;
pub mod discovery;
pub mod ticket;
pub mod transport;

// Re-export the stable public API. `arena-server` builds on all of these.
pub use authority::{
    assign_authority, authority_ranking, hrw_score, Candidate, ZoneRouter,
};
pub use discovery::Discovery;
pub use ticket::{verify_ticket, TicketError};
pub use transport::MeshTransport;

// Re-export the protocol envelope so downstream crates can name it through arena-mesh
// without a second `use arena_protocol::...` line where they only touch transport.
pub use arena_protocol::message::Envelope;
