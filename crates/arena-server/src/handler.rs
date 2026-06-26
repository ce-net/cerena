//! The mesh request/reply [`Handler`] and the inbound command channel.
//!
//! This is the *producer* side of the actor architecture (see the crate docs). The
//! [`ArenaHandler`] runs inside `ce_rs::serve::serve_where`: for each reliable RPC it
//! decodes the [`Envelope`], hands it to the single tick-loop consumer as an
//! [`Inbound::Request`] carrying a `oneshot` reply channel, and awaits the reply to send
//! back over the mesh. It does **no** game logic and touches **no** `World` — all of that
//! lives behind the channel, single-threaded, in the tick loop.
//!
//! The fire-and-forget paths (player input, border mirrors, verify results, content/
//! discovery control) are pushed onto the same channel as [`Inbound::Message`] /
//! [`Inbound::Candidates`] / [`Inbound::StageContent`] by dedicated tasks in
//! [`crate::server`]. The channel is the one seam between concurrent mesh I/O and the
//! lock-free simulation.

use tokio::sync::{mpsc, oneshot};

use ce_rs::serve::{Handler, Request};

use arena_mesh::{Candidate, Envelope};
use arena_protocol::{decode, encode, NodeId};

use arena_content::ContentPack;

/// A message from a mesh producer task to the single tick-loop consumer.
pub enum Inbound {
    /// A reliable RPC needing a reply. The consumer runs the game-side logic and sends the
    /// reply [`Envelope`] back on `reply`; the [`ArenaHandler`] forwards it over the mesh.
    Request {
        /// Authenticated sender node id.
        from: NodeId,
        /// The topic the request arrived on (zone_rpc / authority / coordinator).
        topic: String,
        env: Envelope,
        reply: oneshot::Sender<Envelope>,
    },
    /// A fire-and-forget message (player input, border mirror, verify result, claim).
    Message {
        from: NodeId,
        topic: String,
        env: Envelope,
    },
    /// A freshly-discovered arena candidate set (from the discovery task). The consumer
    /// updates its router and reconciles ownership.
    Candidates(Vec<Candidate>),
    /// A content pack fetched after a [`ContentVersion`](arena_content::hotreload::ContentVersion)
    /// announcement, to be staged into every owned zone at the next tick boundary.
    StageContent { epoch: u64, pack: ContentPack },
}

/// The reliable-RPC handler. Cheap to clone (just an `mpsc::Sender`).
#[derive(Clone)]
pub struct ArenaHandler {
    inbound: mpsc::Sender<Inbound>,
}

impl ArenaHandler {
    pub fn new(inbound: mpsc::Sender<Inbound>) -> Self {
        Self { inbound }
    }
}

impl Handler for ArenaHandler {
    fn handle(&self, req: Request) -> impl std::future::Future<Output = Vec<u8>> + Send {
        let inbound = self.inbound.clone();
        async move {
            // Decode the wire envelope. A malformed payload gets an empty reply (the
            // requester's `request` resolves rather than blocking to timeout).
            let env: Envelope = match decode(&req.payload) {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!(from = %req.from, error = %e, "dropping undecodable RPC");
                    return Vec::new();
                }
            };

            // Hand it to the tick loop and await its authoritative reply. We never touch the
            // sim here — the consumer that owns it produces the reply.
            let (tx, rx) = oneshot::channel();
            if inbound
                .send(Inbound::Request {
                    from: req.from,
                    topic: req.topic,
                    env,
                    reply: tx,
                })
                .await
                .is_err()
            {
                // The tick loop is gone (shutting down): nothing to answer with.
                return Vec::new();
            }

            match rx.await {
                Ok(reply_env) => encode(&reply_env).unwrap_or_default(),
                Err(_) => Vec::new(), // the consumer dropped the reply channel
            }
        }
    }
}
