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
// Subscribe to many topics concurrently (one Promise.all instead of N awaited round
// trips). Subscribing the whole area of interest sequentially cost ~one relay round
// trip per topic — tens of seconds before the player could spawn; fanning them out
// collapses that to a single round trip's latency.
export async function ch_subscribe_many(topicsJson) {
  let topics; try { topics = JSON.parse(topicsJson); } catch (e) { return; }
  await Promise.all(topics.map((t) => bridge().request('POST', '/mesh/subscribe', { body: { topic: t } })));
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
// Reflect the live, authoritative game state into the page's HUD overlay (the
// elements in index.html). Called from the host frame loop so the frontend always
// shows what the backend simulation actually holds — health, position, who is
// nearby, how many zones this tab is authoritative for — rather than a static label.
export function ch_hud(json) {
  let h; try { h = JSON.parse(json); } catch (e) { return; }
  const txt = (id, v) => { const el = document.getElementById(id); if (el && v != null) el.textContent = v; };
  const wid = (id, f) => { const el = document.getElementById(id); if (el) el.style.width = (Math.max(0, Math.min(1, f)) * 100).toFixed(1) + '%'; };
  txt('hud-status', h.status);
  txt('hp-label', h.hp);
  wid('hp-fill', h.hp_frac);
  wid('ar-fill', h.ar_frac);
  txt('hud-coord', h.coord);
  txt('hud-peers', h.peers);
  const dead = document.getElementById('dead');
  if (dead) dead.style.display = h.dead ? 'grid' : 'none';
  const ar = document.getElementById('ar-row');
  if (ar) ar.style.display = (h.ar_frac > 0 ? 'flex' : 'none');
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn ch_status() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch)]
    async fn ch_subscribe_many(topics_json: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    async fn ch_publish(topic: &str, hex: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    async fn ch_get_blob(cid: &str) -> Result<JsValue, JsValue>;
    fn ch_run_inbox(cb: &Closure<dyn FnMut(String)>);
    fn ch_hud(json: &str);
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
        let node = ch_status().await?.as_string().unwrap_or_default();
        if node.is_empty() {
            return Err(JsValue::from_str("status returned no node_id"));
        }
        // Per-tab game identity. Every browser reaches the mesh through the SAME ce node
        // (the relay's, via the ce-serve bridge), so `ch_status` returns one shared
        // node id for all tabs. If we used it directly as the player/quorum identity,
        // every player — you and each buddy — would collapse onto one body and one
        // quorum voice. So we derive a per-tab id: the node id plus a random suffix,
        // making each tab a distinct player while keeping the originating node visible.
        let suffix = (js_sys::Math::random() * (u32::MAX as f64)) as u32;
        let me = format!("{node}-{suffix:08x}");

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

        // Subscribe every new zone's topics in one concurrent fan-out (not 3 awaited
        // round trips per zone), so the player can join almost immediately instead of
        // after tens of seconds of serial subscribes.
        let mut topics: Vec<String> = Vec::with_capacity(to_build.len() * 3);
        for &zone in &to_build {
            topics.push(topic::zone_input(&session, zone));
            topics.push(topic::zone_proof(&session, zone));
            topics.push(topic::zone_state(&session, zone));
        }
        if !topics.is_empty() {
            let json = serde_json::to_string(&topics).unwrap_or_else(|_| "[]".to_string());
            ch_subscribe_many(&json).await?;
        }

        for zone in to_build {
            let in_topic = topic::zone_input(&session, zone);
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

    /// Ensure the local player is actually present, (re)issuing the Join — scheduled at
    /// `current + INPUT_DELAY` off the *live* tick and published to the zone — until the
    /// sim has spawned the body. A no-op once joined (and apply() ignores a duplicate
    /// Join), so it's safe to call every frame.
    ///
    /// This self-heals the otherwise-flaky first spawn: the async connect (subscribing
    /// every AOI zone over the bridge) can take well over the `MAX_CATCHUP` window, so
    /// the one Join scheduled during `connect` is often fast-forwarded away before the
    /// first `advance`. Re-issuing from the frame loop schedules it against the current
    /// tick, where it lands inside the catch-up window and reliably spawns the player.
    pub fn ensure_joined(&self) {
        let (topic_name, hex) = {
            let mut s = self.state.borrow_mut();
            let zone = s.local_zone;
            let already = s
                .zones
                .get(&zone)
                .and_then(|zr| zr.replica.world().player_entity(&s.me))
                .is_some();
            if already {
                return;
            }
            let seq = next_seq(&mut s);
            let me = s.me.clone();
            let join = ReplicaInput::Join { team_pref: None, name: String::new() };
            let ti = match s.zones.get_mut(&zone) {
                Some(zr) => zr.replica.schedule_local(me, seq, join),
                None => return,
            };
            (topic::zone_input(&s.session, zone), encode_msg(&ReplicaMsg::Input(ti)))
        };
        spawn_publish(topic_name, hex);
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

    /// How many zones this tab currently hosts (its area of interest). The client uses
    /// this to notice when the hosted set changes and the visible terrain must rebuild.
    pub fn hosted_zone_count(&self) -> usize {
        self.state.borrow().zones.len()
    }

    /// The CPU-side render meshes for every hosted zone, extracted from the same
    /// world-space terrain SDF the collision is built from (so what you see is what you
    /// walk on). The client uploads these to the GPU via [`crate::mesh_gpu::GpuMesh`].
    /// `res` is the surface-extraction grid resolution per axis (higher = finer, costlier).
    pub fn zone_render_meshes(&self, res: usize) -> Vec<arena_procgen::mesh::Mesh> {
        let s = self.state.borrow();
        let mut zones: Vec<ZoneId> = s.zones.keys().copied().collect();
        // Deterministic order so successive rebuilds are stable.
        zones.sort_by_key(|z| (z.x, z.z));
        zones
            .iter()
            .map(|z| arena_procgen::world::generate_zone_mesh(&s.pack.worldgen, *z, res))
            .collect()
    }

    /// The hosted zones, the player's local zone first then the rest in a stable order.
    /// The client extracts terrain one zone per frame in this order, so the zone you
    /// stand in renders immediately and the neighbours stream in without one giant
    /// boot hitch (extracting all nine at once froze the first frames for seconds,
    /// which is what delayed the spawn).
    pub fn hosted_zones_local_first(&self) -> Vec<ZoneId> {
        let s = self.state.borrow();
        let local = s.local_zone;
        let mut zones: Vec<ZoneId> = s.zones.keys().copied().collect();
        zones.sort_by_key(|z| (*z != local, z.x, z.z));
        zones
    }

    /// Extract the render mesh for a single hosted zone (the heavy surface-nets pass for
    /// just that zone), from the same terrain field its collision is built from.
    pub fn zone_render_mesh(&self, zone: ZoneId, res: usize) -> arena_procgen::mesh::Mesh {
        let s = self.state.borrow();
        arena_procgen::world::generate_zone_mesh(&s.pack.worldgen, zone, res)
    }

    /// Push the live authoritative state into the page HUD overlay (see the `ch_hud`
    /// shim). This is the one place the frontend learns what the backend simulation
    /// holds: the local mage's health/armor, where they stand, how many other players
    /// are in view, and how many zones this tab is hosting. Cheap and side-effect-free
    /// on the sim, so the frame loop can call it on a light cadence.
    pub fn publish_hud(&self) {
        let s = self.state.borrow();
        let zones = s.zones.len();

        // The local player's authoritative entity, if it has spawned yet.
        let local = s.zones.get(&s.local_zone).and_then(|zr| {
            let w = zr.replica.world();
            w.player_entity(&s.me).and_then(|id| w.entities().get(&id).cloned())
        });

        // Count players visible across the hosted area of interest.
        let mut players = 0usize;
        for zr in s.zones.values() {
            for e in zr.replica.world().entities().values() {
                if e.kind == arena_protocol::entity::EntityKind::Player {
                    players += 1;
                }
            }
        }
        let peers = players.saturating_sub(if local.is_some() { 1 } else { 0 });

        let (status, hp, hp_frac, ar_frac, coord, dead) = match &local {
            Some(e) => {
                let hp = e.health.max(0);
                let coord = format!("{:.0}, {:.0}, {:.0}", e.pos.x, e.pos.y, e.pos.z);
                let dead = !e.is_alive();
                let status = if dead {
                    "you have fallen".to_string()
                } else if peers > 0 {
                    format!("hosting {zones} zones · {peers} nearby")
                } else {
                    format!("hosting {zones} zones · exploring")
                };
                (
                    status,
                    hp.to_string(),
                    (hp as f32 / 100.0).clamp(0.0, 1.0),
                    (e.armor.max(0) as f32 / 100.0).clamp(0.0, 1.0),
                    coord,
                    dead,
                )
            }
            None => (
                "entering Cerena…".to_string(),
                "—".to_string(),
                1.0,
                0.0,
                String::new(),
                false,
            ),
        };

        let payload = serde_json::json!({
            "status": status,
            "hp": hp,
            "hp_frac": hp_frac,
            "ar_frac": ar_frac,
            "coord": coord,
            "peers": if peers > 0 { format!("{peers} mage(s) near") } else { String::new() },
            "dead": dead,
        });
        ch_hud(&payload.to_string());
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
