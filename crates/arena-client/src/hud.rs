//! HUD state: the diegetic numbers a mage needs at a glance.
//!
//! This module owns the *data* of the heads-up display; drawing it is a screen-space
//! pass in [`crate::render`] (textured quads for bars/icons, a crosshair, and — later
//! — text). [`HudState`] is rebuilt each snapshot from the authoritative
//! [`LocalPlayerState`] and fed the tick's [`GameEvent`]s so the kill feed and
//! hitmarkers stay in sync with what actually happened.
//!
//! Health and armor come straight from the authoritative entity. Mana, stamina, XP
//! and level are first-class to the mage RPG but not yet carried on
//! [`LocalPlayerState`]; until the protocol grows those fields we surface the
//! ammo/charge values the wire does carry and leave the richer resources as
//! client-side placeholders (clearly marked).
//!
//! Later this is also where the **spell-crafting UI** lives — composing
//! [`arena_content`] `EffectOp`s into a spell, previewing it, and binding it to an
//! ability slot — so the HUD module is intentionally the home of player-facing game
//! UI, not just the combat overlay.

use arena_protocol::NodeId;
use arena_protocol::snapshot::{GameEvent, LocalPlayerState};

use crate::input::ABILITY_SLOTS;

/// How long a kill-feed entry lingers before fading out, seconds.
const KILL_FEED_TTL: f32 = 6.0;
/// Max kill-feed lines kept on screen.
const KILL_FEED_MAX: usize = 6;
/// How long a hitmarker flashes, seconds.
const HITMARKER_TTL: f32 = 0.15;

/// A single bar (health/mana/stamina): current and max, both already in display
/// units. The renderer scales the fill quad by `fraction()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bar {
    pub current: f32,
    pub max: f32,
}

impl Bar {
    pub fn new(current: f32, max: f32) -> Self {
        Self { current, max }
    }
    /// Fill fraction in 0..1 (0 if `max` is non-positive).
    pub fn fraction(&self) -> f32 {
        if self.max <= 0.0 {
            0.0
        } else {
            (self.current / self.max).clamp(0.0, 1.0)
        }
    }
}

/// One ability slot's HUD state (the bound spell's readiness).
#[derive(Debug, Clone, Default)]
pub struct AbilitySlot {
    /// Content spell id bound here (empty = unbound). Cosmetic label for the icon.
    pub spell_id: String,
    /// 0..1 cooldown remaining (0 = ready). Drives a radial sweep on the icon.
    pub cooldown_frac: f32,
    /// Whether this is the currently-selected slot (highlighted).
    pub selected: bool,
}

/// A kill-feed line: "killer [weapon] victim", with a fade timer.
#[derive(Debug, Clone)]
pub struct KillFeedEntry {
    pub killer: NodeId,
    pub victim: NodeId,
    pub weapon: u8,
    /// Seconds of life remaining; the line fades as this approaches 0.
    pub ttl: f32,
}

/// The complete HUD snapshot for a frame.
#[derive(Debug, Clone)]
pub struct HudState {
    pub health: Bar,
    pub armor: Bar,
    /// Mana — placeholder until the protocol carries it; see module docs.
    pub mana: Bar,
    /// Stamina — placeholder (sprint/dodge budget); see module docs.
    pub stamina: Bar,
    /// Focus charges (mapped from the wire ammo fields for now): in-hand / reserve.
    pub charges: (u16, u16),
    pub level: u32,
    /// XP progress toward the next level, 0..1.
    pub xp_frac: f32,
    pub abilities: Vec<AbilitySlot>,
    /// Selected ability slot index.
    pub selected_slot: u8,
    pub kill_feed: Vec<KillFeedEntry>,
    /// Hitmarker flash timer (>0 = show); set when the local player lands a hit.
    pub hitmarker: f32,
    /// True while dead/awaiting respawn (drives the respawn overlay + timer).
    pub dead: bool,
    /// Smoothed round-trip time for the netgraph readout.
    pub rtt_ms: f32,
}

impl Default for HudState {
    fn default() -> Self {
        Self {
            health: Bar::new(100.0, 100.0),
            armor: Bar::new(0.0, 100.0),
            mana: Bar::new(100.0, 100.0),
            stamina: Bar::new(100.0, 100.0),
            charges: (0, 0),
            level: 1,
            xp_frac: 0.0,
            abilities: vec![AbilitySlot::default(); ABILITY_SLOTS as usize],
            selected_slot: 0,
            kill_feed: Vec::new(),
            hitmarker: 0.0,
            dead: false,
            rtt_ms: 0.0,
        }
    }
}

impl HudState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh the player-state-derived fields from an authoritative snapshot. Bars
    /// that the protocol does not yet carry are left untouched (their placeholder
    /// values persist). `local_node` lets us recognise our own kills for hitmarkers.
    pub fn apply_local(&mut self, local: &LocalPlayerState) {
        self.health = Bar::new(local.state.health.max(0) as f32, 100.0);
        self.armor = Bar::new(local.state.armor.max(0) as f32, 100.0);
        self.charges = (local.ammo_in_mag, local.ammo_reserve);
        self.dead = !local.state.is_alive() || local.respawn_at_tick != 0;
        // TODO: mana/stamina/xp/level once the wire carries them (extend
        //       LocalPlayerState or read active StatusEffects from the WorldView).
    }

    /// Update the selected slot highlight (driven by [`crate::input::Input::slot`]).
    pub fn set_selected_slot(&mut self, slot: u8) {
        self.selected_slot = slot;
        for (i, a) in self.abilities.iter_mut().enumerate() {
            a.selected = i as u8 == slot;
        }
    }

    /// Set the netgraph RTT readout.
    pub fn set_rtt(&mut self, rtt_ms: f32) {
        self.rtt_ms = rtt_ms;
    }

    /// Fold a tick's events into the HUD: kill feed, hitmarkers, etc. `local_node`
    /// is this client's CE node id, used to flash a hitmarker only on our own hits.
    pub fn ingest_events(&mut self, events: &[GameEvent], local_node: &NodeId) {
        for ev in events {
            match ev {
                GameEvent::Death {
                    weapon,
                    victim_node,
                    killer_node,
                    ..
                } => {
                    self.kill_feed.push(KillFeedEntry {
                        killer: killer_node.clone(),
                        victim: victim_node.clone(),
                        weapon: *weapon,
                        ttl: KILL_FEED_TTL,
                    });
                    // Trim oldest lines beyond the cap.
                    let overflow = self.kill_feed.len().saturating_sub(KILL_FEED_MAX);
                    if overflow > 0 {
                        self.kill_feed.drain(0..overflow);
                    }
                }
                // Flash a hitmarker only when *we* are the attacker. We compare via
                // the owner node on the attacker entity, which the renderer-side
                // caller resolves; here we accept the conservative approximation of
                // flashing on any hit whose attacker owner matches us once entity
                // lookup is threaded in. (TODO: pass attacker NodeId on Hit.)
                GameEvent::Hit { .. } => {
                    let _ = local_node; // see TODO above
                    self.hitmarker = HITMARKER_TTL;
                }
                _ => {}
            }
        }
    }

    /// Age out time-based HUD elements (kill feed fade, hitmarker flash). Call once
    /// per rendered frame with the frame delta in seconds.
    pub fn tick(&mut self, dt: f32) {
        self.hitmarker = (self.hitmarker - dt).max(0.0);
        for k in &mut self.kill_feed {
            k.ttl -= dt;
        }
        self.kill_feed.retain(|k| k.ttl > 0.0);
    }
}
