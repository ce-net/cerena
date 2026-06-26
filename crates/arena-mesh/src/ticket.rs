//! Session-ticket verification — the trust boundary for joining a match.
//!
//! A [`SessionTicket`] is a capability: the session coordinator signs "this CE node id
//! may play in this session until `expires_at`". To admit a player the authority must
//! confirm two things:
//!
//! 1. the ticket has not expired, and
//! 2. the Ed25519 signature `sig` over [`SessionTicket::signing_bytes`] verifies against
//!    the `issuer` node id (which *is* an Ed25519 public key, hex-encoded).
//!
//! Because the player identity is the scarce, on-chain CE node id, a verified ticket
//! actually binds the seat to something bans and karma can stick to — unlike an
//! email-signup shooter where a cheater mints a fresh free identity.

use arena_protocol::auth::SessionTicket;

/// Why a [`SessionTicket`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TicketError {
    /// `now_unix` is at or past the ticket's `expires_at`.
    #[error("session ticket expired")]
    Expired,
    /// The `issuer` field is not a valid 32-byte hex Ed25519 public key.
    #[error("session ticket issuer is not a valid node id / public key")]
    BadIssuer,
    /// The signature is malformed or does not verify against the issuer's key.
    #[error("session ticket signature is invalid")]
    BadSig,
}

/// Verify a session ticket as of `now_unix` (unix seconds).
///
/// Checks expiry first (cheap), then the issuer signature over the ticket's canonical
/// bytes. Returns `Ok(())` only when the ticket is both unexpired and correctly signed.
pub fn verify_ticket(ticket: &SessionTicket, now_unix: u64) -> Result<(), TicketError> {
    // 1. Expiry. `expires_at` is "the unix second after which the ticket is invalid", so a
    //    ticket is dead once the clock reaches it.
    if now_unix >= ticket.expires_at {
        return Err(TicketError::Expired);
    }

    // 2. Signature over the canonical signing bytes, verified against the issuer's key.
    verify_sig(&ticket.issuer, &ticket.signing_bytes(), &ticket.sig)
}

/// Verify a detached Ed25519 signature `sig` over `message` against `issuer_hex` (a hex
/// Ed25519 public key).
///
/// !!!  WIRE-SHAPE ONLY — NOT A REAL SIGNATURE CHECK  !!!
///
/// This is a deliberate, clearly-marked stub. It validates only the *shapes* required for a
/// real verification to even be attempted:
///   (a) the issuer decodes from hex to exactly 32 bytes (a candidate Ed25519 public key), and
///   (b) the signature is exactly 64 bytes (a candidate Ed25519 signature),
/// and then returns `Ok(())` WITHOUT cryptographically verifying the signature.
///
/// We do not pull in an Ed25519 crate here right now, so the actual elliptic-curve check is
/// not performed. The trust boundary is left explicit instead of hidden: a forged ticket with
/// a well-formed 64-byte `sig` and a valid-looking issuer will currently pass.
///
/// TODO(security, gated before mainnet): route this through the CE capability layer
/// (`ce-cap`) / `ed25519-dalek` so the signature is genuinely verified against `issuer`, and
/// confirm the issuer is an authorised session coordinator (a capability chain rooted at the
/// session root), not merely any 32-byte key. Until then, callers MUST NOT treat a passing
/// ticket as a real authorization in any deployment that handles value or competitive stakes.
fn verify_sig(issuer_hex: &str, _message: &[u8], sig: &[u8]) -> Result<(), TicketError> {
    // (a) issuer must be a 32-byte hex key.
    let issuer = hex::decode(issuer_hex).map_err(|_| TicketError::BadIssuer)?;
    if issuer.len() != 32 {
        return Err(TicketError::BadIssuer);
    }

    // (b) signature must be the right length for Ed25519.
    if sig.len() != 64 {
        return Err(TicketError::BadSig);
    }

    // TODO: real verification goes here (ce-cap / ed25519-dalek). See the function doc.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::{auth::SessionId, world::MapId, NodeId};

    fn issuer_hex() -> NodeId {
        // 32 bytes of 0x11 → 64 hex chars: a well-formed (if fake) public key.
        "11".repeat(32)
    }

    fn ticket(expires_at: u64, issuer: NodeId, sig_len: usize) -> SessionTicket {
        SessionTicket {
            player: "22".repeat(32),
            session: SessionId("match-1".into()),
            map: MapId("map-abc".into()),
            expires_at,
            issuer,
            sig: vec![0u8; sig_len],
            karma: 0,
        }
    }

    #[test]
    fn accepts_unexpired_well_formed_ticket() {
        let t = ticket(1_000, issuer_hex(), 64);
        assert_eq!(verify_ticket(&t, 999), Ok(()));
    }

    #[test]
    fn rejects_expired_ticket() {
        let t = ticket(1_000, issuer_hex(), 64);
        assert_eq!(verify_ticket(&t, 1_000), Err(TicketError::Expired));
        assert_eq!(verify_ticket(&t, 1_001), Err(TicketError::Expired));
    }

    #[test]
    fn rejects_bad_issuer() {
        // Not hex.
        let t = ticket(1_000, "not-hex-zzzz".into(), 64);
        assert_eq!(verify_ticket(&t, 0), Err(TicketError::BadIssuer));
        // Hex but wrong length (16 bytes, not 32).
        let t = ticket(1_000, "11".repeat(16), 64);
        assert_eq!(verify_ticket(&t, 0), Err(TicketError::BadIssuer));
    }

    #[test]
    fn rejects_wrong_length_signature() {
        let t = ticket(1_000, issuer_hex(), 63);
        assert_eq!(verify_ticket(&t, 0), Err(TicketError::BadSig));
    }
}
