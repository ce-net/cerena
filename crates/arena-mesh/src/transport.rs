//! The mesh transport adapter: `Envelope` in, `Envelope` out.
//!
//! Every higher layer (`arena-net`, `arena-server`) talks to the CE mesh **only** through
//! [`MeshTransport`]. It maps the protocol [`Envelope`] onto concrete `ce_rs` calls and does
//! nothing else — no game logic, no authorization, no state. That keeps the trust and routing
//! decisions where they belong (the server) and makes this layer trivially testable.
//!
//! Which `ce_rs` primitive each path uses (see `arena_protocol::message::topic` for the topic
//! helpers that name the destinations):
//!
//! - **High-rate, lossy** game traffic (player [`Input`], snapshots, border mirrors) →
//!   [`send_envelope`](MeshTransport::send_envelope) (directed, fire-and-forget) or
//!   [`publish_envelope`](MeshTransport::publish_envelope) (pub/sub to a zone topic). Drops are
//!   fine; the sim sends a fresh frame next tick.
//! - **Reliable RPC** (join, leave, report, zone switch, authority handoff) →
//!   [`request_envelope`](MeshTransport::request_envelope) on the requester side and
//!   [`serve_session`](MeshTransport::serve_session) on the authority side.
//! - **Inbound fan-in** of everything pushed to this node → [`envelopes`](MeshTransport::envelopes).

use anyhow::Result;
use arena_protocol::{decode, encode, message::Envelope, NodeId};
use ce_rs::serve::{serve_where, Handler};
use ce_rs::CeClient;
use futures_util::{stream::Stream, StreamExt};

/// Adapts the protocol [`Envelope`] onto the CE mesh via a local-node [`CeClient`]. Cheap to
/// clone (the client is a thin `reqwest` handle).
#[derive(Clone)]
pub struct MeshTransport {
    ce: CeClient,
}

impl MeshTransport {
    /// Wrap an existing CE client (typically the local node).
    pub fn new(ce: CeClient) -> Self {
        Self { ce }
    }

    /// Connect to the local CE node on the default port, attaching `token` (the node's API token,
    /// e.g. from `ce_rs::discover_api_token`) so mutating calls pass the node's auth middleware.
    /// Pass `None` for read-only access.
    pub fn local(token: Option<String>) -> Self {
        Self { ce: CeClient::with_token(ce_rs::DEFAULT_BASE_URL, token) }
    }

    /// Borrow the underlying client (e.g. to drive an endpoint this adapter does not wrap).
    pub fn client(&self) -> &CeClient {
        &self.ce
    }

    /// This node's own id (`GET /status`).
    pub async fn node_id(&self) -> Result<NodeId> {
        Ok(self.ce.status().await?.node_id)
    }

    /// Publish an envelope to a pub/sub `topic` (the node signs it; every subscriber receives it).
    /// Use for broadcast-safe zone traffic, e.g. `topic::zone_state` for spectator snapshots.
    pub async fn publish_envelope(&self, topic: &str, env: &Envelope) -> Result<()> {
        let bytes = encode(env)?;
        self.ce.publish(topic, &bytes).await
    }

    /// Send an envelope directly to one node, fire-and-forget (no reply awaited). The high-rate
    /// path: player inputs on `topic::zone_input`, AOI snapshots back to a client, border mirrors
    /// to a neighbour authority. Delivery is best-effort, which is exactly right for game state.
    pub async fn send_envelope(&self, to: &NodeId, topic: &str, env: &Envelope) -> Result<()> {
        let bytes = encode(env)?;
        self.ce.send_message(to, topic, &bytes).await
    }

    /// Send an envelope to one node and await its envelope reply (reliable RPC). The peer's
    /// [`serve_session`](Self::serve_session) handler answers with an encoded [`Envelope`]; we
    /// decode it here. Errors on timeout (`timeout_ms`) or a malformed reply.
    pub async fn request_envelope(
        &self,
        to: &NodeId,
        topic: &str,
        env: &Envelope,
        timeout_ms: u64,
    ) -> Result<Envelope> {
        let bytes = encode(env)?;
        let reply = self.ce.request(to, topic, &bytes, timeout_ms).await?;
        Ok(decode(&reply)?)
    }

    /// Subscribe this node to a pub/sub `topic` so it begins receiving that topic's messages on
    /// the inbound stream (idempotent; lasts for the node's lifetime).
    pub async fn subscribe(&self, topic: &str) -> Result<()> {
        self.ce.subscribe(topic).await
    }

    /// The inbound fan-in: every app message pushed to this node, decoded to an [`Envelope`].
    ///
    /// Yields `(from, topic, reply_token, envelope)` where `from` is the cryptographically
    /// authenticated sender NodeId, `topic` is where it arrived, and `reply_token` is `Some` only
    /// for RPC requests expecting a reply (pass it to `ce_rs::CeClient::reply`). Messages whose
    /// payload is not a decodable arena [`Envelope`] are silently skipped, so traffic from other
    /// apps sharing this node does not break the stream.
    ///
    /// This is the low-level firehose; the reliable RPC side is usually better served by
    /// [`serve_session`](Self::serve_session), which handles dedup/reconnect/reply for you. Use
    /// this directly for the fire-and-forget paths (inputs, snapshots) that have no reply token.
    pub async fn envelopes(
        &self,
    ) -> Result<impl Stream<Item = (NodeId, String, Option<u64>, Envelope)>> {
        let stream = self.ce.messages_stream().await?;
        Ok(stream.filter_map(|item| async move {
            let m = item.ok()?;
            let bytes = m.payload().ok()?;
            let env: Envelope = decode(&bytes).ok()?;
            Some((m.from, m.topic, m.reply_token, env))
        }))
    }

    /// Serve the reliable RPC side for a session until `shutdown` resolves: answer every inbound
    /// request whose topic starts with `accept_prefix` (e.g. a session's topic root, so all of its
    /// `zone_rpc` / `authority` / `coordinator` topics match) via `handler`.
    ///
    /// This is a thin wrapper over `ce_rs::serve::serve_where`, which owns the inbox push,
    /// reply-token de-duplication, and reconnect-with-backoff loop. The [`Handler`] is implemented
    /// in `arena-server`: it authorizes `req.from` (ticket / capability / authority-ownership
    /// checks), decodes the request [`Envelope`], runs the game-side logic, and returns the reply
    /// as an **encoded** [`Envelope`] (a `ServerMsg` or `AuthorityMsg`) via `arena_protocol::encode`.
    /// Transport stays dumb: it never inspects or decides anything about the payload.
    pub async fn serve_session<H>(
        &self,
        accept_prefix: &str,
        handler: &H,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()>
    where
        H: Handler,
    {
        let prefix = accept_prefix.to_string();
        // No pub/sub subscriptions needed: directed requests arrive regardless. A handler that also
        // wants broadcast topics can `subscribe` to them separately before calling this.
        serve_where(&self.ce, &[], move |topic| topic.starts_with(&prefix), handler, shutdown).await
    }
}
