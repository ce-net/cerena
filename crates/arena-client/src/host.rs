//! Browser hosting — **playing IS hosting**. A tab runs the *same*
//! [`arena_replica::Replica`] engine a headless donor node runs (see
//! `arena_server::replica_host`), for every zone in its area of interest. It reaches
//! the mesh only through the ce-serve bridge (`window.__ceNode`), exchanging
//! tick-tagged inputs on each zone's `/in` topic and reconciling with the other
//! replicas (other browsers and nodes) by the periodic state-hash quorum on `/proof`.
//! There is no trusted server: the players present in a zone are its servers.
//!
//! This module is the browser transport + the host loop glue; ALL the simulation and
//! consensus logic is the shared, platform-free `arena-replica`/`arena-sim` code, so a
//! browser and a relay compute bit-for-bit the same world from the same inputs. The
//! geometry each seeds is [`arena_sim::map::build_zone_geometry`] — the one shared,
//! deterministic source the server uses too.
//!
//! State lives behind an `Rc<RefCell<_>>` so the synchronous per-frame [`BrowserHost::advance`]
//! and the async mesh I/O it spawns (publishes, snapshot fetches) share it safely on the
//! browser's single thread, the same pattern spacegame-wasm uses.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use arena_content::registry::ContentRegistry;
use arena_content::ContentPack;
use arena_protocol::auth::SessionId;
use arena_protocol::entity::EntityState;
use arena_protocol::input::InputFrame;
use arena_protocol::message::{topic, Envelope};
use arena_protocol::replica::{ReplicaInput, ReplicaMsg, SnapshotAd, StateProof};
use arena_protocol::world::{ZoneId, AOI_ZONE_RADIUS};
use arena_protocol::{decode, encode, EntityId, NodeId, Tick};
use arena_replica::{agree, tick_at, Replica, Verdict};
use arena_sim::map::build_zone_geometry;
use arena_sim::{World, ZoneSnapshot};

// ---------------------------------------------------------------------------
// The ce-serve mesh bridge (window.__ceNode). Tiny JS shims; all logic in Rust.
// ---------------------------------------------------------------------------
#[wasm_bindgen(inline_js = r#"
function bridge() {
  const n = globalThis.__ceNode;
  if (!n || typeof n.request !== 'function') throw new Error('ce mesh bridge not present (serve through ce-serve)');
  return n;
}
function toHex(u8) { let s=''; for (const b of u8) s += b.toString(16).padStart(2,'0'); return s; }
export async function ch_status() {
  const r = await bridge().request('GET', '/status');
  const b = (typeof r.body === 'string') ? JSON.parse(r.body) : r.body;
  return b && b.node_id ? b.node_id : '';
}
export async function ch_subscribe(topic) {
  await bridge().request('POST', '/mesh/subscribe', { body: { topic: topic } });
}
export async function ch_publish(topic, hex) {
  await bridge().request('POST', '/mesh/publish', { body: { topic: topic, payload_hex: hex } });
}
// Fetch a blob by CID and return it hex-encoded (normalising whatever body shape the
// bridge returns: a binary string, an array, or an ArrayBuffer).
export async function ch_get_blob(cid) {
  const r = await bridge().request('GET', '/blobs/' + cid);
  const b = r.body;
  if (b instanceof ArrayBuffer) return toHex(new Uint8Array(b));
  if (b && b.buffer instanceof ArrayBuffer) return toHex(new Uint8Array(b.buffer));
  if (Array.isArray(b)) return toHex(Uint8Array.from(b));
  if (typeof b === 'string') { const u=new Uint8Array(b.length); for (let i=0;i<b.length;i++) u[i]=b.charCodeAt(i)&0xff; return toHex(u); }
  return '';
}
export function ch_run_inbox(cb) {
  (async () => {
    for (;;) {
      try { for await (const chunk of bridge().stream('/mesh/messages/stream')) { try { cb(chunk); } catch (e) {} } }
      catch (e) {}
      await new Promise((r) => setTimeout(r, 500));
    }
  })();
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn ch_status() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch)]
    async fn ch_subscribe(topic: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    async fn ch_publish(topic: &str, hex: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    async fn ch_get_blob(cid: &str) -> Result<JsValue, JsValue>;
    fn ch_run_inbox(cb: &Closure<dyn FnMut(String)>);
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(h: &str) -> Vec<u8> {
    let h = h.trim();
    let b = h.as_bytes();
    let mut out = Vec::with_capacity(h.len() / 2);
    let mut i = 0;
    while i + 1 < b.len() {
        match ((b[i] as char).to_digit(16), (b[i + 1] as char).to_digit(16)) {
            (Some(a), Some(c)) => out.push((a * 16 + c) as u8),
            _ => break,
        }
        i += 2;
    }
    out
}

/// Per-zone host state inside the shared cell.
struct ZoneReplica {
    replica: Replica,
    proofs: HashMap<NodeId, [u8; 32]>,
    latest_snapshot: Option<(Tick, String)>,
}

/// The mutable host state, shared between the frame loop and spawned async I/O.
struct HostState {
    me: NodeId,
    session: SessionId,
    pack: ContentPack,
    epoch: u64,
    local_zone: ZoneId,
    zones: HashMap<ZoneId, ZoneReplica>,
    input_topic_zone: HashMap<String, ZoneId>,
    /// Inbound replica traffic from the bridge pump: `(from, topic, msg)`.
    inbound: VecDeque<(NodeId, String, ReplicaMsg)>,
    /// Snapshot blobs fetched async, awaiting synchronous import on the next frame:
    /// `(zone, bytes)`.
    pending_reseed: VecDeque<(ZoneId, Vec<u8>)>,
    seq: u32,
    proof_interval: Tick,
}

impl HostState {
    fn content(&self) -> ContentRegistry {
        ContentRegistry::new(self.epoch, self.pack.clone())
            .unwrap_or_else(|_| ContentRegistry::bootstrap())
    }
}

/// The browser host handle. Cheap to clone (shares the state cell).
#[derive(Clone)]
pub struct BrowserHost {
    state: Rc<RefCell<HostState>>,
}

/// Browser wall-clock milliseconds since the Unix epoch, for the shared tick clock.
/// (`performance.now()` is monotonic-since-load, not epoch-anchored, so we use
/// `Date.now()` here to match every other replica's epoch tick.)
fn now_ms() -> f64 {
    js_sys::Date::now()
}

impl BrowserHost {
    /// Connect to the local node over the bridge, then host the area of interest around
    /// `local_zone` and join it. Returns once the node id is known and the inbox pump is
    /// running; hosting proceeds as [`BrowserHost::advance`] is called each frame.
    pub async fn connect(
        session: SessionId,
        pack: ContentPack,
        epoch: u64,
        local_zone: ZoneId,
    ) -> Result<BrowserHost, JsValue> {
        let me = ch_status().await?.as_string().unwrap_or_default();
        if me.is_empty() {
            return Err(JsValue::from_str("status returned no node_id"));
        }

        let state = Rc::new(RefCell::new(HostState {
            me,
            session,
            pack,
            epoch,
            local_zone,
            zones: HashMap::new(),
            input_topic_zone: HashMap::new(),
            inbound: VecDeque::new(),
            pending_reseed: VecDeque::new(),
            seq: 0,
            proof_interval: 32,
        }));
        let host = BrowserHost { state };

        // Start the inbound pump: decode each bridge chunk into a routed ReplicaMsg.
        host.start_inbox();
        // Ensure the AOI replicas exist + subscribed, then announce ourselves.
        host.ensure_interest().await?;
        host.publish_join().await;
        Ok(host)
    }

    fn start_inbox(&self) {
        let state = self.state.clone();
        let cb = Closure::wrap(Box::new(move |chunk: String| {
            if let Some((from, topic, msg)) = parse_chunk(&chunk) {
                state.borrow_mut().inbound.push_back((from, topic, msg));
            }
        }) as Box<dyn FnMut(String)>);
        ch_run_inbox(&cb);
        cb.forget();
    }

    /// Build + subscribe any interest zone (own zone + neighbours) not yet hosted.
    async fn ensure_interest(&self) -> Result<(), JsValue> {
        let (session, interest, to_build): (SessionId, Vec<ZoneId>, Vec<ZoneId>) = {
            let s = self.state.borrow();
            let interest = s.local_zone.aoi(AOI_ZONE_RADIUS);
            let to_build: Vec<ZoneId> =
                interest.iter().copied().filter(|z| !s.zones.contains_key(z)).collect();
            (s.session.clone(), interest, to_build)
        };
        let _ = interest;
        for zone in to_build {
            let in_topic = topic::zone_input(&session, zone);
            ch_subscribe(&in_topic).await?;
            ch_subscribe(&topic::zone_proof(&session, zone)).await?;
            ch_subscribe(&topic::zone_state(&session, zone)).await?;

            let mut s = self.state.borrow_mut();
            let geometry = build_zone_geometry(&s.pack.worldgen, zone);
            let mut world = World::new(geometry, s.content());
            world.set_tick(tick_at(now_ms()));
            s.zones.insert(
                zone,
                ZoneReplica { replica: Replica::new(world), proofs: HashMap::new(), latest_snapshot: None },
            );
            s.input_topic_zone.insert(in_topic, zone);
        }
        Ok(())
    }

    /// Announce this player into its local zone (spawns the body on every replica).
    async fn publish_join(&self) {
        let (topic_name, hex) = {
            let mut s = self.state.borrow_mut();
            let zone = s.local_zone;
            let seq = next_seq(&mut s);
            let me = s.me.clone();
            let join = ReplicaInput::Join { team_pref: None, name: String::new() };
            let ti = match s.zones.get_mut(&zone) {
                Some(zr) => zr.replica.schedule_local(me, seq, join),
                None => return,
            };
            (topic::zone_input(&s.session, zone), encode_msg(&ReplicaMsg::Input(ti)))
        };
        let _ = ch_publish(&topic_name, &hex).await;
    }

    /// Submit this player's input for the current tick: schedule it locally at the
    /// canonical future tick and broadcast it so every replica applies it identically.
    pub fn submit_input(&self, frame: InputFrame) {
        let (topic_name, hex) = {
            let mut s = self.state.borrow_mut();
            let zone = s.local_zone;
            let seq = next_seq(&mut s);
            let me = s.me.clone();
            let ti = match s.zones.get_mut(&zone) {
                Some(zr) => zr.replica.schedule_local(me, seq, ReplicaInput::Input(frame)),
                None => return,
            };
            (topic::zone_input(&s.session, zone), encode_msg(&ReplicaMsg::Input(ti)))
        };
        spawn_publish(topic_name, hex);
    }

    /// One frame: drain inbound traffic + any fetched snapshots, advance every interest
    /// replica to the shared wall-clock tick, then on cadence publish a proof and run the
    /// quorum. Mesh writes and snapshot fetches are fired async; the sim work is sync.
    pub fn advance(&self) {
        let target = tick_at(now_ms());

        // 1. Apply fetched snapshots (merge-to-quorum / late join) synchronously.
        // 2. Route inbound inputs/proofs/snapshot-adverts.
        // 3. Advance, and collect the proof publishes + reconciles to fire after.
        let mut to_publish: Vec<(String, String)> = Vec::new();
        let mut to_fetch: Vec<(ZoneId, String)> = Vec::new();
        {
            let mut s = self.state.borrow_mut();

            while let Some((zone, bytes)) = s.pending_reseed.pop_front() {
                if let Ok(snap) = bincode::deserialize::<ZoneSnapshot>(&bytes) {
                    let geometry = build_zone_geometry(&s.pack.worldgen, zone);
                    let content = s.content();
                    if let Some(zr) = s.zones.get_mut(&zone) {
                        zr.replica.reseed(World::import_snapshot(geometry, content, snap));
                        zr.replica.set_tick(target);
                    }
                }
            }

            while let Some((from, in_topic, msg)) = s.inbound.pop_front() {
                route(&mut s, from, &in_topic, msg);
            }

            let session = s.session.clone();
            let me = s.me.clone();
            let proof_interval = s.proof_interval;
            let zones: Vec<ZoneId> = s.zones.keys().copied().collect();
            for zone in zones {
                let tick = {
                    let zr = s.zones.get_mut(&zone).unwrap();
                    zr.replica.advance_to(target);
                    zr.replica.tick()
                };
                if tick % proof_interval == 0 {
                    let hash = s.zones[&zone].replica.state_hash();
                    s.zones.get_mut(&zone).unwrap().proofs.insert(me.clone(), hash);
                    to_publish.push((
                        topic::zone_proof(&session, zone),
                        encode_msg(&ReplicaMsg::Proof(StateProof { zone, tick, hash })),
                    ));
                    // Quorum: tally and, if we're out-voted, queue a snapshot fetch.
                    let (verdict, snapshot) = {
                        let zr = s.zones.get_mut(&zone).unwrap();
                        let proofs: Vec<(NodeId, [u8; 32])> =
                            zr.proofs.iter().map(|(n, h)| (n.clone(), *h)).collect();
                        let v = agree(&proofs).verdict(&me);
                        zr.proofs.clear();
                        (v, zr.latest_snapshot.clone())
                    };
                    if let Verdict::ResyncTo(_) = verdict {
                        if let Some((_t, cid)) = snapshot {
                            to_fetch.push((zone, cid));
                        }
                    }
                }
            }
        }

        for (topic_name, hex) in to_publish {
            spawn_publish(topic_name, hex);
        }
        for (zone, cid) in to_fetch {
            self.spawn_fetch_snapshot(zone, cid);
        }
    }

    /// Fetch a snapshot blob async and queue its bytes for synchronous import next frame.
    fn spawn_fetch_snapshot(&self, zone: ZoneId, cid: String) {
        let state = self.state.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(hexval) = ch_get_blob(&cid).await {
                if let Some(hex) = hexval.as_string() {
                    let bytes = from_hex(&hex);
                    if !bytes.is_empty() {
                        state.borrow_mut().pending_reseed.push_back((zone, bytes));
                    }
                }
            }
        });
    }

    /// The renderable world: every entity across the interest-set replicas. The caller
    /// drops the local body (first person) and follows it with the camera.
    pub fn render_entities(&self) -> Vec<EntityState> {
        let s = self.state.borrow();
        let mut out = Vec::new();
        for zr in s.zones.values() {
            out.extend(zr.replica.world().entities().values().cloned());
        }
        out
    }

    /// This node's authenticated id (the player identity).
    pub fn node_id_string(&self) -> NodeId {
        self.state.borrow().me.clone()
    }

    /// This player's local entity id, once the join has been simulated.
    pub fn local_entity(&self) -> Option<EntityId> {
        let s = self.state.borrow();
        let me = s.me.clone();
        s.zones.get(&s.local_zone).and_then(|zr| zr.replica.world().player_entity(&me))
    }
}

/// Next monotonic input sequence for this node.
fn next_seq(s: &mut HostState) -> u32 {
    s.seq = s.seq.wrapping_add(1);
    s.seq
}

/// Route one inbound message into the host state (the author `from` is authenticated).
fn route(s: &mut HostState, from: NodeId, in_topic: &str, msg: ReplicaMsg) {
    match msg {
        ReplicaMsg::Input(ti) => {
            if let Some(&zone) = s.input_topic_zone.get(in_topic) {
                if let Some(zr) = s.zones.get_mut(&zone) {
                    zr.replica.schedule(from, ti);
                }
            }
        }
        ReplicaMsg::Proof(StateProof { zone, hash, .. }) => {
            if let Some(zr) = s.zones.get_mut(&zone) {
                zr.proofs.insert(from, hash);
            }
        }
        ReplicaMsg::Snapshot(SnapshotAd { zone, tick, cid }) => {
            if let Some(zr) = s.zones.get_mut(&zone) {
                if zr.latest_snapshot.as_ref().map_or(true, |(t, _)| tick >= *t) {
                    zr.latest_snapshot = Some((tick, cid));
                }
            }
        }
    }
}

/// Encode a replica message as the hex of its bincode envelope (the bridge wire form).
fn encode_msg(msg: &ReplicaMsg) -> String {
    match encode(&Envelope::Replica(msg.clone())) {
        Ok(bytes) => to_hex(&bytes),
        Err(_) => String::new(),
    }
}

/// Fire-and-forget a bridge publish.
fn spawn_publish(topic_name: String, hex: String) {
    if hex.is_empty() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let _ = ch_publish(&topic_name, &hex).await;
    });
}

/// Parse one bridge chunk `{from, topic, payload_hex}` into a routed replica message.
fn parse_chunk(chunk: &str) -> Option<(NodeId, String, ReplicaMsg)> {
    let v: serde_json::Value = serde_json::from_str(chunk).ok()?;
    let topic = v.get("topic")?.as_str()?.to_string();
    let from = v.get("from")?.as_str()?.to_string();
    let ph = v.get("payload_hex")?.as_str()?;
    let bytes = from_hex(ph);
    let env: Envelope = decode(&bytes).ok()?;
    match env {
        Envelope::Replica(msg) => Some((from, topic, msg)),
        _ => None,
    }
}
