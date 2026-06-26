//! Headless load generation: simulated players that exercise the *real* wire path.
//!
//! A [`Bot`] joins a session, then every tick sends an [`InputBatch`] (random-walk
//! movement plus occasional casts) on the zone input topic and consumes the
//! snapshots the authority unicasts back. It measures **snapshot round-trip
//! latency** by stamping each input `seq` on send and matching it against
//! `Snapshot.local.last_input_seq` on receive — i.e. "how long until the server
//! reflects my input?", the number that actually governs how the game feels.
//!
//! Bots talk to a node's CE API via `ce_rs` and bincode `Envelope`s, so they go
//! through the genuine mesh transport, snapshot encoder, and sim — not a mock.
//!
//! At-scale note: 10,000 *real* CE identities can't be minted from one test
//! process, so bots present **test session tickets** and the authority must be run
//! with `--e2e-insecure` (ticket signature check relaxed). Real-identity scale
//! tests distribute bots across many machines, each a real node.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arena_protocol::{
    Tick, decode, encode,
    auth::{SessionId, SessionTicket},
    input::{Buttons, InputBatch, InputFrame},
    message::{ClientMsg, Envelope, ServerMsg, topic},
    world::{MapId, Team, ZoneId},
};
use glam::Vec3;

use crate::Result;
use crate::metrics::Latencies;

/// Per-bot tunables.
#[derive(Clone)]
pub struct BotConfig {
    pub session: SessionId,
    pub map: MapId,
    /// Casts per second the bot attempts (load on the magic VM + anti-cheat gates).
    pub cast_rate: f32,
    /// How aggressively the bot roams (m/s of wish-velocity).
    pub move_speed: f32,
}

/// A single simulated player.
pub struct Bot {
    id: String,
    ce: ce_rs::CeClient,
    cfg: BotConfig,
    authority: Option<String>,
    zone: ZoneId,
    seq: u32,
    /// seq -> monotonic send time (ms) for RTT matching.
    inflight: HashMap<u32, f64>,
    pos: Vec3,
    yaw: f32,
    rng: u64,
    /// Shared sink for measured snapshot RTTs.
    lat: Arc<Latencies>,
    /// Bytes of snapshot payload received (bandwidth accounting).
    bytes_in: u64,
    last_acked_seq: u32,
    dropped: u64,
}

impl Bot {
    pub fn new(id: String, ce: ce_rs::CeClient, cfg: BotConfig, lat: Arc<Latencies>) -> Self {
        let seed = fnv(&id);
        Bot {
            id,
            ce,
            cfg,
            authority: None,
            zone: ZoneId::new(0, 0),
            seq: 0,
            inflight: HashMap::new(),
            pos: Vec3::ZERO,
            yaw: 0.0,
            rng: seed | 1,
            lat,
            bytes_in: 0,
            last_acked_seq: 0,
            dropped: 0,
        }
    }

    fn next_rand(&mut self) -> f32 {
        // xorshift64; deterministic per-bot so a failing run reproduces.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Join the session via the coordinator RPC, learning our entity + zone owner.
    pub async fn join(&mut self) -> Result<()> {
        let ticket = SessionTicket {
            player: self.id.clone(),
            session: self.cfg.session.clone(),
            map: self.cfg.map.clone(),
            expires_at: u64::MAX, // test ticket; --e2e-insecure relaxes sig check
            issuer: "e2e-coordinator".to_string(),
            sig: vec![0u8; 64],
            karma: arena_protocol::karma::KARMA_DEFAULT,
        };
        let join = Envelope::Client(ClientMsg::Join {
            protocol: arena_protocol::PROTOCOL_VERSION,
            ticket,
            team_pref: Some(Team::None),
            name: self.id.chars().take(8).collect(),
        });
        let reply = self
            .ce
            .request(
                "", // empty 'to' => the node we're attached to routes to the coordinator
                &topic::coordinator(&self.cfg.session),
                &encode(&join)?,
                4000,
            )
            .await?;
        if let Envelope::Server(ServerMsg::JoinAccept {
            zone, authority, spawn, ..
        }) = decode::<Envelope>(&reply)?
        {
            self.zone = zone;
            self.authority = Some(authority);
            self.pos = spawn.pos;
            self.yaw = spawn.yaw;
        } else {
            anyhow::bail!("join refused for {}", self.id);
        }
        // Subscribe so we receive this zone's broadcast events; personalized
        // snapshots arrive as direct messages on our stream.
        self.ce
            .subscribe(&topic::zone_state(&self.cfg.session, self.zone))
            .await
            .ok();
        Ok(())
    }

    /// Produce + send one tick of input. `now_ms` is a monotonic clock for RTT.
    pub async fn step(&mut self, server_tick: Tick, now_ms: f64) -> Result<()> {
        let Some(authority) = self.authority.clone() else {
            return Ok(());
        };
        // Random-walk: occasionally re-aim, always push forward, sometimes cast.
        if self.next_rand() < 0.05 {
            self.yaw += (self.next_rand() - 0.5) * 1.5;
        }
        let mut buttons = Buttons::default();
        buttons.set(Buttons::FORWARD, true);
        if self.next_rand() < 0.1 {
            buttons.set(Buttons::JUMP, true);
        }
        // Cast at the configured rate (FIRE = primary ability).
        let cast = self.next_rand() < self.cfg.cast_rate * arena_protocol::TICK_DT;
        buttons.set(Buttons::FIRE, cast);

        self.seq += 1;
        let frame = InputFrame {
            seq: self.seq,
            client_tick: server_tick,
            buttons,
            yaw: self.yaw,
            pitch: 0.0,
            weapon_slot: 0,
        };
        self.inflight.insert(self.seq, now_ms);
        // Bound inflight memory; if the server is far behind, count drops.
        if self.inflight.len() > arena_protocol::TICK_HZ as usize * 2 {
            self.dropped += 1;
            // Drop the oldest.
            if let Some(&oldest) = self.inflight.keys().min() {
                self.inflight.remove(&oldest);
            }
        }

        let batch = InputBatch {
            ack_tick: self.last_acked_seq,
            frames: vec![frame],
        };
        let env = Envelope::Client(ClientMsg::Input(batch));
        // Fire-and-forget on the zone input topic, addressed to the zone authority.
        self.ce
            .send_message(
                &authority,
                &topic::zone_input(&self.cfg.session, self.zone),
                &encode(&env)?,
            )
            .await
            .ok();
        Ok(())
    }

    /// Drain inbound messages, matching snapshots to inflight inputs for RTT and
    /// reacting to redirects (zone hand-off / authority failover). Call frequently.
    pub async fn pump(&mut self, now_ms: f64) -> Result<()> {
        let msgs = self.ce.messages().await.unwrap_or_default();
        for m in msgs {
            let Ok(bytes) = m.payload() else { continue };
            self.bytes_in += bytes.len() as u64;
            let Ok(Envelope::Server(sm)) = decode::<Envelope>(&bytes) else {
                continue;
            };
            match sm {
                ServerMsg::Snapshot(snap) => {
                    let acked = snap.local.last_input_seq;
                    self.last_acked_seq = acked;
                    if let Some(sent) = self.inflight.remove(&acked) {
                        self.lat.record(now_ms - sent);
                    }
                    // Forget anything older than the ack (server consumed past it).
                    self.inflight.retain(|&s, _| s > acked);
                }
                ServerMsg::Redirect { zone, authority, .. } => {
                    // Failover / boundary cross: re-home our input stream. The
                    // recovery scenarios assert this happens promptly.
                    self.zone = zone;
                    self.authority = Some(authority);
                    self.ce
                        .subscribe(&topic::zone_state(&self.cfg.session, self.zone))
                        .await
                        .ok();
                }
                ServerMsg::Kick { reason } => {
                    anyhow::bail!("bot {} kicked: {reason}", self.id);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn bytes_in(&self) -> u64 {
        self.bytes_in
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Orchestrates many bots against a fleet. Bots are sharded across the provided
/// node API urls round-robin (so the load is spread the way real players would be).
pub struct LoadGen {
    pub lat: Arc<Latencies>,
    node_urls: Vec<String>,
    cfg: BotConfig,
}

impl LoadGen {
    pub fn new(node_urls: Vec<String>, cfg: BotConfig) -> Self {
        Self {
            lat: Arc::new(Latencies::new()),
            node_urls,
            cfg,
        }
    }

    /// Ramp to `players` bots over `ramp`, hold for `hold`, then stop. Returns the
    /// aggregate bytes received and dropped-input count for the bandwidth/loss
    /// metrics. Each bot runs as its own task driving step()+pump() at TICK_HZ.
    pub async fn run(
        &self,
        players: usize,
        ramp: Duration,
        hold: Duration,
    ) -> Result<LoadResult> {
        use std::sync::atomic::{AtomicU64, Ordering};
        let bytes = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let started = Arc::new(AtomicU64::new(0));

        let per_bot_delay = if players > 0 {
            ramp.as_secs_f64() / players as f64
        } else {
            0.0
        };
        let mut tasks = Vec::with_capacity(players);
        let t0 = std::time::Instant::now();
        let deadline = t0 + ramp + hold;

        for i in 0..players {
            let url = self.node_urls[i % self.node_urls.len()].clone();
            let cfg = self.cfg.clone();
            let lat = self.lat.clone();
            let bytes = bytes.clone();
            let dropped = dropped.clone();
            let started = started.clone();
            let stagger = Duration::from_secs_f64(per_bot_delay * i as f64);

            tasks.push(tokio::spawn(async move {
                tokio::time::sleep(stagger).await;
                let ce = ce_rs::CeClient::new(&url);
                let mut bot = Bot::new(format!("bot-{i:06}"), ce, cfg, lat);
                if bot.join().await.is_err() {
                    return;
                }
                started.fetch_add(1, Ordering::Relaxed);
                let mut tick: Tick = 0;
                let dt = Duration::from_secs_f64(arena_protocol::TICK_DT as f64);
                let mut iv = tokio::time::interval(dt);
                while std::time::Instant::now() < deadline {
                    iv.tick().await;
                    tick = tick.wrapping_add(1);
                    let now = mono_ms();
                    if bot.step(tick, now).await.is_err() {
                        break;
                    }
                    if bot.pump(now).await.is_err() {
                        break;
                    }
                }
                bytes.fetch_add(bot.bytes_in(), Ordering::Relaxed);
                dropped.fetch_add(bot.dropped(), Ordering::Relaxed);
            }));
        }

        for t in tasks {
            t.await.ok();
        }
        let secs = (ramp + hold).as_secs_f64().max(1.0);
        Ok(LoadResult {
            joined: started.load(Ordering::Relaxed) as usize,
            bytes_in: bytes.load(Ordering::Relaxed),
            dropped: dropped.load(Ordering::Relaxed),
            wall_secs: secs,
            rtt: self.lat.summary(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct LoadResult {
    pub joined: usize,
    pub bytes_in: u64,
    pub dropped: u64,
    pub wall_secs: f64,
    pub rtt: crate::metrics::LatencySummary,
}

impl LoadResult {
    pub fn bytes_per_player_s(&self) -> f64 {
        if self.joined == 0 {
            0.0
        } else {
            self.bytes_in as f64 / self.joined as f64 / self.wall_secs
        }
    }
}

/// Monotonic milliseconds. `Instant` has no epoch, so we anchor to a process-start
/// `Instant` captured lazily.
fn mono_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let s = START.get_or_init(Instant::now);
    s.elapsed().as_secs_f64() * 1000.0
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
