//! Session tickets binding a player to a CE identity.
//!
//! There is no account system: a player *is* their CE node id (an Ed25519 key).
//! To join a match the client presents a [`SessionTicket`] — a capability granted
//! by the match's coordinator authority that says "this node id may play in this
//! session". The mesh layer (`arena-mesh`) verifies the signature against the
//! capability chain; this struct is just the wire-shape.
//!
//! Because identity is cryptographic and scarce (a node id is tied to on-chain
//! stake/karma), bans and karma penalties actually stick — a cheater cannot mint
//! a fresh free identity the way they can on an email-signup shooter.

use serde::{Deserialize, Serialize};

use crate::{NodeId, world::MapId};

/// Issued by the session coordinator; the client includes it in [`crate::message::Join`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTicket {
    /// The player's CE node id (their public key, hex).
    pub player: NodeId,
    /// The session this ticket admits the player to.
    pub session: SessionId,
    pub map: MapId,
    /// Unix seconds after which the ticket is invalid.
    pub expires_at: u64,
    /// The coordinator node that issued and signed this ticket.
    pub issuer: NodeId,
    /// Detached Ed25519 signature over the canonical bytes of the fields above,
    /// produced by `issuer`. Verified by `arena-mesh` via the CE capability layer.
    pub sig: Vec<u8>,
    /// The player's current karma snapshot at issue time, so authorities can
    /// gate matchmaking (e.g. low-karma players into a quarantine pool) without
    /// a round-trip to the karma service on the hot join path.
    pub karma: i32,
}

/// A match/session identifier. One session spans many zones; each zone is hosted
/// by a (possibly different) authority node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Mesh topic root for this session, e.g. `ce-game/arena/<session>`.
    pub fn topic_root(&self) -> String {
        format!("ce-game/arena/{}", self.0)
    }
}

impl SessionTicket {
    /// The canonical byte string that `sig` covers. Both signer and verifier
    /// must build this identically. Deliberately excludes `sig` and `karma`
    /// (karma is advisory and re-checked authoritatively).
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(self.player.as_bytes());
        b.push(0);
        b.extend_from_slice(self.session.0.as_bytes());
        b.push(0);
        b.extend_from_slice(self.map.0.as_bytes());
        b.push(0);
        b.extend_from_slice(&self.expires_at.to_le_bytes());
        b.extend_from_slice(self.issuer.as_bytes());
        b
    }
}
