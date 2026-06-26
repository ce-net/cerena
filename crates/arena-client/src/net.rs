//! The network seam — the one place the client touches the wire.
//!
//! Everything above this module deals in decoded [`arena_protocol`] messages; this
//! module turns them into bytes and back and moves them over a transport. The
//! transport itself differs by platform:
//!
//! - **Browser (`wasm32`)**: a `WebSocket` to the ce-net relay's `/mesh-bridge`
//!   endpoint (wss). The mesh-bridge is how a browser tab reaches the libp2p mesh
//!   without speaking libp2p: the relay translates the WebSocket frames onto the
//!   per-zone mesh topics where the zone authority lives. Frames are binary and
//!   carry a bincode [`Envelope`] (see `arena_protocol::{encode, decode}`).
//! - **Native**: today a [`StubNetClient`] loopback so the client builds and runs
//!   offline. A real native build connects through the local CE node's mesh API
//!   (HTTP/ws to `127.0.0.1:8844`), the same `/mesh-bridge` shape — TODO.
//!
//! Both implement [`NetClient`]: poll decoded server messages, send client ones.
//! The rest of the client is transport-agnostic.

use arena_protocol::message::{ClientMsg, Envelope, ServerMsg};

/// The transport abstraction the client drives. Non-blocking by contract:
/// [`NetClient::poll_messages`] returns whatever has arrived since the last call.
pub trait NetClient {
    /// Drain all server messages received since the last poll (never blocks).
    fn poll_messages(&mut self) -> Vec<ServerMsg>;
    /// Queue a client message for delivery (fire-and-forget; dropped silently if
    /// the socket is not open — the netcode re-sends unacked inputs anyway).
    fn send(&mut self, msg: ClientMsg);
}

/// Encode a client message as a length-bounded bincode [`Envelope`] for the wire.
fn encode_client(msg: ClientMsg) -> Option<Vec<u8>> {
    match arena_protocol::encode(&Envelope::Client(msg)) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            tracing::warn!("failed to encode client message: {e}");
            None
        }
    }
}

/// Decode a wire frame into a [`ServerMsg`], dropping anything that is not a
/// server-directed envelope (the client never processes client/authority frames).
fn decode_server(bytes: &[u8]) -> Option<ServerMsg> {
    match arena_protocol::decode::<Envelope>(bytes) {
        Ok(Envelope::Server(msg)) => Some(msg),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("failed to decode server frame: {e}");
            None
        }
    }
}

// ===========================================================================
// Native stub transport
// ===========================================================================

/// A loopback/no-op transport for native builds and tests. It holds an inbox that
/// test harnesses (or a future real native connector) can push [`ServerMsg`]s into,
/// and simply logs anything sent. This keeps the whole client buildable and
/// runnable without a live mesh.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub struct StubNetClient {
    inbox: std::collections::VecDeque<ServerMsg>,
    /// Count of messages "sent" (for diagnostics).
    pub sent: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl StubNetClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inject a server message as if it had arrived from the mesh (tests / a local
    /// in-process authority loopback).
    pub fn inject(&mut self, msg: ServerMsg) {
        self.inbox.push_back(msg);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl NetClient for StubNetClient {
    fn poll_messages(&mut self) -> Vec<ServerMsg> {
        self.inbox.drain(..).collect()
    }

    fn send(&mut self, msg: ClientMsg) {
        // A real native transport would forward the encoded envelope to the local
        // CE node's mesh-bridge; here we just account for it.
        self.sent += 1;
        if let Some(bytes) = encode_client(msg) {
            tracing::trace!("stub net send: {} bytes (dropped)", bytes.len());
        }
    }
}

// ===========================================================================
// Browser WebSocket transport (relay /mesh-bridge)
// ===========================================================================

#[cfg(target_arch = "wasm32")]
pub use wasm_ws::WsNetClient;

#[cfg(target_arch = "wasm32")]
mod wasm_ws {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use web_sys::{BinaryType, MessageEvent, WebSocket};

    use arena_protocol::message::{ClientMsg, ServerMsg};

    use super::{NetClient, decode_server, encode_client};

    /// A WebSocket transport to the relay mesh-bridge. Decoded server messages land
    /// in a shared `inbox` from the `onmessage` callback; the render loop drains it
    /// each frame via [`NetClient::poll_messages`].
    pub struct WsNetClient {
        ws: WebSocket,
        inbox: Rc<RefCell<VecDeque<ServerMsg>>>,
        // The JS callbacks must outlive the function that registered them, so we own
        // them here. Dropping `WsNetClient` drops the socket and the callbacks.
        _on_message: Closure<dyn FnMut(MessageEvent)>,
        _on_open: Closure<dyn FnMut()>,
        _on_error: Closure<dyn FnMut(JsValue)>,
    }

    impl WsNetClient {
        /// Connect to `url` (e.g. `wss://relay.ce-net.com/mesh-bridge?session=...`).
        /// Returns immediately; messages start flowing once the socket opens.
        pub fn connect(url: &str) -> Result<Self, JsValue> {
            let ws = WebSocket::new(url)?;
            // We exchange raw bincode, so binary frames as ArrayBuffer.
            ws.set_binary_type(BinaryType::Arraybuffer);

            let inbox: Rc<RefCell<VecDeque<ServerMsg>>> = Rc::new(RefCell::new(VecDeque::new()));

            // onmessage: decode the binary frame into a ServerMsg and enqueue it.
            let inbox_cb = inbox.clone();
            let on_message = Closure::wrap(Box::new(move |evt: MessageEvent| {
                if let Ok(buf) = evt.data().dyn_into::<js_sys::ArrayBuffer>() {
                    let bytes = js_sys::Uint8Array::new(&buf).to_vec();
                    if let Some(msg) = decode_server(&bytes) {
                        inbox_cb.borrow_mut().push_back(msg);
                    }
                }
            }) as Box<dyn FnMut(MessageEvent)>);
            ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

            let on_open = Closure::wrap(Box::new(move || {
                tracing::info!("mesh-bridge websocket open");
            }) as Box<dyn FnMut()>);
            ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

            let on_error = Closure::wrap(Box::new(move |e: JsValue| {
                tracing::warn!("mesh-bridge websocket error: {e:?}");
            }) as Box<dyn FnMut(JsValue)>);
            ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

            Ok(Self {
                ws,
                inbox,
                _on_message: on_message,
                _on_open: on_open,
                _on_error: on_error,
            })
        }

        /// Whether the socket is in the OPEN ready-state.
        fn is_open(&self) -> bool {
            self.ws.ready_state() == WebSocket::OPEN
        }
    }

    impl NetClient for WsNetClient {
        fn poll_messages(&mut self) -> Vec<ServerMsg> {
            self.inbox.borrow_mut().drain(..).collect()
        }

        fn send(&mut self, msg: ClientMsg) {
            if !self.is_open() {
                return; // dropped; netcode re-sends unacked inputs next batch
            }
            if let Some(bytes) = encode_client(msg) {
                if let Err(e) = self.ws.send_with_u8_array(&bytes) {
                    tracing::warn!("websocket send failed: {e:?}");
                }
            }
        }
    }
}
