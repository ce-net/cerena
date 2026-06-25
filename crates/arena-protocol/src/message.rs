//! The wire envelope: every byte that crosses the mesh between a client and an
//! authority, or between authorities, is one of these.
//!
//! Transport mapping (see `arena-mesh`):
//! - Unreliable, high-rate game traffic (inputs, snapshots) → `ce_rs::send_message`
//!   on per-zone topics, fire-and-forget.
//! - Reliable RPC (join, leave, report, hand-off) → `ce_rs::request`/`reply`.
//! - The discriminant byte at the front lets a single mesh handler fan out.

use serde::{Deserialize, Serialize};

use crate::{
    NodeId, PROTOCOL_VERSION, Tick,
    auth::{SessionId, SessionTicket},
    input::InputBatch,
    karma::{KarmaUpdate, Report},
    snapshot::Snapshot,
    weapon::WeaponDef,
    world::{MapId, SpawnPoint, Team, ZoneId},
};

/// Client → authority. Most of these are RPC (request/reply); `Input` is the one
/// high-rate fire-and-forget path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    /// Handshake. Carries the protocol version and the signed session ticket.
    Join {
        protocol: u32,
        ticket: SessionTicket,
        /// The client's preferred spawn team (None = assigned).
        team_pref: Option<Team>,
        /// Display name (cosmetic; identity is the node id). Length-capped.
        name: String,
    },
    /// High-rate intent. Fire-and-forget on the zone input topic.
    Input(InputBatch),
    /// The client crossed into a new zone and wants the new authority to adopt it.
    /// The old authority hands off; see [`AuthorityMsg::AdoptPlayer`].
    RequestZoneSwitch { to: ZoneId },
    /// File a report against another player.
    Report(Report),
    /// Graceful disconnect.
    Leave,
    /// Liveness; the authority drops players who stop pinging.
    Ping { client_time_ms: u64 },
}

/// Authority → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Join accepted. Everything the client needs to start predicting.
    JoinAccept {
        protocol: u32,
        /// The entity id assigned to this client within its current zone.
        entity: crate::EntityId,
        zone: ZoneId,
        /// The node currently authoritative for `zone`. The client sends inputs here.
        authority: NodeId,
        tick: Tick,
        server_time_ms: u64,
        loadout: Vec<WeaponDef>,
        spawn: SpawnPoint,
        map: MapId,
    },
    /// Join refused (bad ticket, expired, banned, full).
    JoinReject { reason: String },
    /// Authoritative world state for this client's AOI.
    Snapshot(Snapshot),
    /// The client must re-home to a new authority (zone change or hand-off /
    /// authority failover). The client redirects its input stream there.
    Redirect {
        zone: ZoneId,
        authority: NodeId,
        /// The entity id under the new authority (ids are per-zone).
        entity: crate::EntityId,
        tick: Tick,
    },
    /// Karma changed (e.g. a quarantine kicked in mid-session).
    Karma(KarmaUpdate),
    Pong { client_time_ms: u64, server_time_ms: u64 },
    /// The authority is shutting this player's session down (kick/ban/match end).
    Kick { reason: String },
}

/// Authority ↔ authority (server-to-server). These keep the distributed sim
/// coherent: zone hand-off, border mirroring, and cross-validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthorityMsg {
    /// Hand a player (and its authoritative entity state) to a neighbouring
    /// authority as it crosses a zone boundary. The receiver replies with the
    /// new local entity id so the origin can `Redirect` the client.
    AdoptPlayer {
        session: SessionId,
        zone: ZoneId,
        player: NodeId,
        state: crate::entity::EntityState,
        /// Carry the player's last applied input seq so prediction is seamless.
        last_input_seq: u32,
        ammo_in_mag: u16,
        ammo_reserve: u16,
    },
    /// Reply to [`AuthorityMsg::AdoptPlayer`].
    Adopted { player: NodeId, entity: crate::EntityId },
    /// Periodic mirror of border-region entities to a neighbour so cross-zone
    /// hitscan and rendering work. Read-only on the receiver.
    BorderMirror {
        from_zone: ZoneId,
        tick: Tick,
        entities: Vec<crate::entity::EntityState>,
    },
    /// A claim that this node is (or wishes to be) authoritative for `zone`,
    /// carrying a monotonically increasing lease epoch. Conflicts resolve by
    /// rendezvous-hash priority + highest epoch (see `arena-mesh`).
    AuthorityClaim {
        session: SessionId,
        zone: ZoneId,
        claimant: NodeId,
        epoch: u64,
        /// On-chain stake the claimant has bonded for this lease (Sybil cost).
        stake_base_units: i128,
    },
    /// Cross-validation request: replay these inputs against your shadow sim and
    /// report whether the originating authority's result matches within epsilon.
    /// This is how the distributed sim catches a *malicious authority* — not just
    /// a malicious client. See `arena-karma::crossval`.
    VerifyTick {
        session: SessionId,
        zone: ZoneId,
        tick: Tick,
        /// Hash of the authority's post-tick entity set.
        result_hash: [u8; 32],
        /// The inputs the authority claims it applied this tick.
        inputs: Vec<(NodeId, crate::input::InputFrame)>,
    },
    /// Reply to [`AuthorityMsg::VerifyTick`]: does the shadow result match?
    VerifyResult {
        zone: ZoneId,
        tick: Tick,
        verifier: NodeId,
        agree: bool,
        /// The verifier's own result hash, for dispute logging.
        their_hash: [u8; 32],
    },
}

/// Top-level tagged envelope. The first decoded field is the variant tag, so a
/// single mesh handler can route without a separate header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Envelope {
    Client(ClientMsg),
    Server(ServerMsg),
    Authority(AuthorityMsg),
}

impl Envelope {
    /// Reject a mismatched protocol version early (only `Join`/`JoinAccept` carry it).
    pub fn check_version(&self) -> Result<(), crate::WireError> {
        let theirs = match self {
            Envelope::Client(ClientMsg::Join { protocol, .. }) => Some(*protocol),
            Envelope::Server(ServerMsg::JoinAccept { protocol, .. }) => Some(*protocol),
            _ => None,
        };
        if let Some(theirs) = theirs {
            if theirs != PROTOCOL_VERSION {
                return Err(crate::WireError::Version {
                    ours: PROTOCOL_VERSION,
                    theirs,
                });
            }
        }
        Ok(())
    }
}

/// Mesh topic helpers. Topics are derived deterministically so any node can
/// compute where to publish/subscribe without a directory lookup.
pub mod topic {
    use super::*;

    /// High-rate input topic for a zone: clients publish, authority subscribes.
    pub fn zone_input(session: &SessionId, zone: ZoneId) -> String {
        format!("{}/{}/in", session.topic_root(), zone.token())
    }

    /// Snapshot topic for a zone: authority publishes, clients subscribe. In
    /// practice snapshots are AOI-personalized and unicast per client, but the
    /// shared topic carries broadcast-safe events for spectators/observers.
    pub fn zone_state(session: &SessionId, zone: ZoneId) -> String {
        format!("{}/{}/state", session.topic_root(), zone.token())
    }

    /// Reliable RPC topic for a zone authority (join/leave/report/switch).
    pub fn zone_rpc(session: &SessionId, zone: ZoneId) -> String {
        format!("{}/{}/rpc", session.topic_root(), zone.token())
    }

    /// Authority-to-authority control plane for a session.
    pub fn authority(session: &SessionId) -> String {
        format!("{}/authority", session.topic_root())
    }

    /// The session coordinator's join/matchmaking endpoint.
    pub fn coordinator(session: &SessionId) -> String {
        format!("{}/coordinator", session.topic_root())
    }

    /// Karma service ingest (reports + telemetry) and verdict broadcast.
    pub const KARMA_INGEST: &str = "ce-game/arena/karma/ingest";
    pub const KARMA_VERDICT: &str = "ce-game/arena/karma/verdict";
}
