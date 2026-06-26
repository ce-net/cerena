//! Game-feel feedback: the layer that turns *what happened* into *what the player
//! feels*.
//!
//! The authoritative sim emits a stream of [`GameEvent`]s every snapshot — a melee
//! arc connected, a force shoved someone, a fireball detonated, a heal ticked. This
//! module reads that stream from the **local player's** point of view and drives the
//! three channels that make combat read in the body, not just on the HUD:
//!
//! 1. **Camera shake** — a single scalar *trauma* (Squirrel Eiserloh's model): events
//!    add trauma, trauma decays every frame, and the actual shake is `trauma^2` so it
//!    falls off smoothly and never lingers as a low hum. Trauma drives angular jitter
//!    (yaw/pitch), a horizon *roll*, and a small positional offset, all from cheap
//!    deterministic noise so it costs nothing and never allocates.
//! 2. **View kick** — a critically-ish-damped spring on the look angles. Firing a
//!    spell, swinging a sword, or taking a knockback punches the kick; it snaps and
//!    settles. This is *directional* (a shove from the left kicks the view right),
//!    which is what sells "forces".
//! 3. **Screen flash + directional damage** — a decaying full-screen tint (red when
//!    hurt, green when healed, gold on a buff) plus a world-space direction the most
//!    recent damage came from, for a directional damage indicator.
//!
//! Crucially, none of this touches the *input* look angles the netcode sends: the
//! wobble lives only on [`crate::camera::Camera`]'s transient shake fields, so the
//! server still hit-tests the player's true aim. Feel is local; truth is authority.

use glam::{Vec2, Vec3};

use arena_protocol::EntityId;
use arena_protocol::snapshot::{GameEvent, MeleeKind};

use crate::camera::Camera;

// --- tuning ---------------------------------------------------------------------

/// Trauma bled off per second (a hit's shake lasts roughly `1 / DECAY` seconds).
const TRAUMA_DECAY: f32 = 1.5;
/// How fast the shake noise oscillates (higher = buzzier, lower = lurchier).
const SHAKE_FREQ: f32 = 20.0;
/// Peak angular jitter (radians) at full trauma.
const MAX_SHAKE_YAW: f32 = 0.055;
const MAX_SHAKE_PITCH: f32 = 0.055;
/// Peak horizon roll (radians) at full trauma.
const MAX_SHAKE_ROLL: f32 = 0.05;
/// Peak positional shake (metres) at full trauma.
const MAX_SHAKE_OFFSET: f32 = 0.16;

/// View-kick spring constants. Stiffness sets the snap-back rate; damping is a touch
/// under critical (`2*sqrt(k) ≈ 26.8`) so the kick overshoots once and settles —
/// crisp rather than mushy.
const KICK_STIFFNESS: f32 = 180.0;
const KICK_DAMPING: f32 = 22.0;

/// Screen flash fade rate (per second).
const FLASH_DECAY: f32 = 3.5;
/// How long the directional damage indicator lingers (seconds).
const DAMAGE_DIR_TTL: f32 = 1.0;

/// The local-player context an event is interpreted against.
#[derive(Debug, Clone, Copy)]
pub struct LocalView {
    pub id: EntityId,
    /// Eye position in world space (the camera origin).
    pub eye: Vec3,
    /// Look yaw (radians), for projecting world directions onto the screen.
    pub yaw: f32,
}

/// The whole game-feel state, advanced once per frame and stamped onto the camera.
#[derive(Debug, Clone)]
pub struct Feedback {
    /// Accumulated trauma in 0..1; shake = trauma^2.
    trauma: f32,
    /// Free-running clock (seconds) the shake noise is sampled against.
    time: f32,
    /// Spring-driven look-angle kick `(yaw, pitch)` in radians, and its velocity.
    kick: Vec2,
    kick_vel: Vec2,
    /// Positional lurch (world metres) and its spring velocity — drives the "shoved"
    /// feel from knockback, decoupled from the noisy trauma offset.
    lurch: Vec3,
    lurch_vel: Vec3,
    /// Screen flash strength 0..1 and its linear-RGB tint.
    flash: f32,
    flash_color: [f32; 3],
    /// Most-recent incoming-damage direction (world, horizontal, unit) + lifetime.
    damage_dir: Option<(Vec2, f32)>,
}

impl Default for Feedback {
    fn default() -> Self {
        Self {
            trauma: 0.0,
            time: 0.0,
            kick: Vec2::ZERO,
            kick_vel: Vec2::ZERO,
            lurch: Vec3::ZERO,
            lurch_vel: Vec3::ZERO,
            flash: 0.0,
            flash_color: [0.0, 0.0, 0.0],
            damage_dir: None,
        }
    }
}

impl Feedback {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current screen-flash strength (0..1) and tint — for the renderer's HUD pass to
    /// composite a full-screen vignette/wash.
    pub fn flash(&self) -> (f32, [f32; 3]) {
        (self.flash.clamp(0.0, 1.0), self.flash_color)
    }

    /// The direction (world-space, horizontal unit vector) the most recent damage
    /// came from, if it is still fresh — for a directional damage indicator.
    pub fn damage_from(&self) -> Option<Vec2> {
        self.damage_dir.map(|(d, _)| d)
    }

    /// Current normalised trauma (diagnostics / tests).
    pub fn trauma(&self) -> f32 {
        self.trauma
    }

    /// Add trauma, clamped to 1. The public hook for set-pieces and external systems.
    pub fn add_trauma(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }

    /// Punch the view-kick spring by an angular impulse `(yaw, pitch)` in radians.
    pub fn add_kick(&mut self, yaw: f32, pitch: f32) {
        self.kick_vel += Vec2::new(yaw, pitch);
    }

    /// Shove the positional lurch spring (world metres/second of impulse).
    pub fn add_lurch(&mut self, impulse: Vec3) {
        self.lurch_vel += impulse;
    }

    /// Trigger a screen flash of `color` at `strength` (kept if brighter than the
    /// current flash, so a big hit is not washed out by a trailing faint one).
    pub fn add_flash(&mut self, color: [f32; 3], strength: f32) {
        if strength >= self.flash {
            self.flash = strength.clamp(0.0, 1.0);
            self.flash_color = color;
        }
    }

    /// Interpret one [`GameEvent`] from the local player's vantage and update the
    /// feel channels. Events that do not involve the local player still contribute
    /// proximity-scaled world shake (a nearby explosion rattles you).
    pub fn ingest(&mut self, event: &GameEvent, local: &LocalView) {
        match event {
            // A detonation rattles the camera, scaled by blast size and proximity.
            GameEvent::Explosion { center, radius } => {
                let prox = proximity(*center, local.eye, radius * 6.0);
                self.add_trauma((0.12 + 0.06 * radius) * prox);
                if prox > 0.6 {
                    self.add_flash([1.0, 0.5, 0.2], 0.25 * prox);
                }
            }
            // An authored shake hint, falling off over a generous radius.
            GameEvent::Shake { center, trauma } => {
                self.add_trauma(trauma * proximity(*center, local.eye, 45.0));
            }
            GameEvent::Hit { attacker, victim, point, damage, .. } => {
                if *victim == local.id {
                    // We got hit: shake, a red flash + a recoil kick, and remember the
                    // direction so the HUD can point a damage arrow at the attacker.
                    let sev = (damage / 60.0).clamp(0.1, 1.0);
                    self.add_trauma(0.2 + 0.4 * sev);
                    self.add_flash([0.8, 0.05, 0.05], 0.3 + 0.4 * sev);
                    self.add_kick(0.0, -0.05 * sev);
                    self.remember_damage_dir(*point, local);
                } else if *attacker == local.id {
                    // We connected: a tiny confirming nudge (hit-feel without spam).
                    self.add_kick(0.0, 0.008);
                }
            }
            // A force shoved us: lurch the camera along the impulse and kick the look
            // opposite the shove (your head snaps back when you are pushed forward).
            GameEvent::Knockback { entity, impulse } => {
                if *entity == local.id {
                    let mag = impulse.length();
                    self.add_trauma((mag / 30.0).clamp(0.0, 0.5));
                    // Project the shove onto screen yaw so a side-shove kicks sideways.
                    let yaw_kick = side_component(*impulse, local.yaw) * 0.01;
                    self.add_kick(yaw_kick, -(impulse.y.abs() * 0.004));
                    self.add_lurch(*impulse * 0.06);
                }
            }
            // Our own swing: a directional weapon kick, heavier on the spin finisher.
            GameEvent::Melee { attacker, victim, dir, kind, .. } => {
                if *attacker == local.id {
                    let (yaw_k, pitch_k) = match kind {
                        MeleeKind::Slash => (0.05, 0.015),
                        MeleeKind::Thrust => (0.0, 0.04),
                        MeleeKind::Spin => (0.09, 0.01),
                    };
                    self.add_kick(yaw_k, pitch_k);
                    if victim.is_some() {
                        // Connecting "thunk": a touch of trauma so a clean hit lands.
                        self.add_trauma(if *kind == MeleeKind::Spin { 0.22 } else { 0.12 });
                    }
                    let _ = dir;
                }
            }
            // Our own cast leaving the staff: a small upward recoil.
            GameEvent::Shot { shooter, .. } => {
                if *shooter == local.id {
                    self.add_kick(0.0, 0.02);
                }
            }
            // Restorative tick on us: a soft green wash.
            GameEvent::Heal { target, amount } => {
                if *target == local.id {
                    self.add_flash([0.1, 0.7, 0.25], (amount / 60.0).clamp(0.05, 0.3));
                }
            }
            // A status landed on us: gold bloom for a buff, sickly violet for a debuff.
            GameEvent::Buff { entity, beneficial } => {
                if *entity == local.id {
                    if *beneficial {
                        self.add_flash([0.85, 0.7, 0.2], 0.22);
                    } else {
                        self.add_flash([0.4, 0.1, 0.5], 0.22);
                    }
                }
            }
            // Our death: a hard jolt and a dark-red bleed-out wash.
            GameEvent::Death { victim, .. } => {
                if *victim == local.id {
                    self.add_trauma(1.0);
                    self.add_flash([0.45, 0.0, 0.0], 0.85);
                }
            }
            // Our (re)spawn: wipe any residual feel so we start clean.
            GameEvent::Spawn { entity, .. } => {
                if *entity == local.id {
                    self.reset();
                }
            }
            _ => {}
        }
    }

    /// Advance every channel by `dt` seconds. Call once per rendered frame.
    pub fn update(&mut self, dt: f32) {
        self.time += dt;
        self.trauma = (self.trauma - TRAUMA_DECAY * dt).max(0.0);
        self.flash = (self.flash - FLASH_DECAY * dt).max(0.0);

        // Critically-damped-ish springs return the kick/lurch to rest.
        let kick_acc = -KICK_STIFFNESS * self.kick - KICK_DAMPING * self.kick_vel;
        self.kick_vel += kick_acc * dt;
        self.kick += self.kick_vel * dt;

        let lurch_acc = -KICK_STIFFNESS * self.lurch - KICK_DAMPING * self.lurch_vel;
        self.lurch_vel += lurch_acc * dt;
        self.lurch += self.lurch_vel * dt;

        if let Some((_, ttl)) = &mut self.damage_dir {
            *ttl -= dt;
            if *ttl <= 0.0 {
                self.damage_dir = None;
            }
        }
    }

    /// Stamp the combined shake + kick + lurch onto `camera`'s transient fields. Call
    /// after [`Feedback::update`] and after the camera has been snapped onto the
    /// predicted player for the frame.
    pub fn apply(&self, camera: &mut Camera) {
        // Trauma drives smooth noise; squaring makes it taper off cleanly.
        let shake = self.trauma * self.trauma;
        let t = self.time * SHAKE_FREQ;

        camera.shake_yaw = self.kick.x + MAX_SHAKE_YAW * shake * noise(1.0, t);
        camera.shake_pitch = self.kick.y + MAX_SHAKE_PITCH * shake * noise(2.0, t);
        camera.shake_roll = MAX_SHAKE_ROLL * shake * noise(3.0, t);
        camera.shake_pos = self.lurch
            + Vec3::new(
                MAX_SHAKE_OFFSET * shake * noise(4.0, t),
                MAX_SHAKE_OFFSET * shake * noise(5.0, t),
                MAX_SHAKE_OFFSET * shake * noise(6.0, t),
            );
    }

    /// Clear all transient feel (used on respawn).
    pub fn reset(&mut self) {
        self.trauma = 0.0;
        self.kick = Vec2::ZERO;
        self.kick_vel = Vec2::ZERO;
        self.lurch = Vec3::ZERO;
        self.lurch_vel = Vec3::ZERO;
        self.flash = 0.0;
        self.damage_dir = None;
    }

    /// Record where incoming damage came from, as a horizontal unit vector.
    fn remember_damage_dir(&mut self, from: Vec3, local: &LocalView) {
        let d = Vec2::new(from.x - local.eye.x, from.z - local.eye.z);
        if d.length_squared() > 1e-4 {
            self.damage_dir = Some((d.normalize(), DAMAGE_DIR_TTL));
        }
    }
}

/// 0 at/beyond `range`, 1 at zero distance — a linear proximity weight.
fn proximity(point: Vec3, eye: Vec3, range: f32) -> f32 {
    if range <= 0.0 {
        return 0.0;
    }
    (1.0 - point.distance(eye) / range).clamp(0.0, 1.0)
}

/// Signed left/right component of a world `impulse` relative to a viewer facing
/// `yaw` — positive when the shove pushes the view's right side. Used to make a
/// side-on knockback kick the camera sideways rather than always straight back.
fn side_component(impulse: Vec3, yaw: f32) -> f32 {
    let (sy, cy) = yaw.sin_cos();
    // Right vector for the yaw convention in `Camera::forward` (-Z forward at yaw 0).
    let right = Vec2::new(cy, sy);
    Vec2::new(impulse.x, impulse.z).dot(right)
}

/// Cheap deterministic value noise in roughly [-1, 1] from a seed + time. A sum of
/// incommensurate sines — smooth, allocation-free, and good enough for shake.
fn noise(seed: f32, t: f32) -> f32 {
    let a = (t * 1.0 + seed * 1.7).sin();
    let b = (t * 2.13 + seed * 3.1).sin();
    let c = (t * 4.73 + seed * 5.27).sin();
    a * 0.5 + b * 0.3 + c * 0.2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> LocalView {
        LocalView { id: 1, eye: Vec3::ZERO, yaw: 0.0 }
    }

    #[test]
    fn trauma_decays_to_zero() {
        let mut fb = Feedback::new();
        fb.add_trauma(1.0);
        assert!(fb.trauma() > 0.9);
        for _ in 0..120 {
            fb.update(1.0 / 60.0);
        }
        assert_eq!(fb.trauma(), 0.0, "trauma must bleed off completely");
    }

    #[test]
    fn taking_a_hit_shakes_flashes_and_points() {
        let mut fb = Feedback::new();
        // Hit from straight ahead (+Z is "behind" at yaw 0; use -Z = forward).
        let ev = GameEvent::Hit {
            attacker: 2,
            victim: 1,
            damage: 60.0,
            headshot: false,
            point: Vec3::new(0.0, 0.0, -5.0),
        };
        fb.ingest(&ev, &local());
        assert!(fb.trauma() > 0.0, "a hit on us must add trauma");
        let (flash, color) = fb.flash();
        assert!(flash > 0.0 && color[0] > color[1], "hurt flash is reddish");
        assert!(fb.damage_from().is_some(), "incoming damage records a direction");
    }

    #[test]
    fn knockback_kicks_the_view_and_settles() {
        let mut fb = Feedback::new();
        let ev = GameEvent::Knockback { entity: 1, impulse: Vec3::new(10.0, 2.0, 0.0) };
        fb.ingest(&ev, &local());
        // The spring has been punched: stepping it produces non-zero camera offset.
        fb.update(1.0 / 60.0);
        let mut cam = Camera::default();
        fb.apply(&mut cam);
        assert!(
            cam.shake_pos.length() > 0.0 || cam.shake_yaw.abs() > 0.0,
            "a knockback must visibly move the camera"
        );
        // ... and after a second it has settled back to rest.
        for _ in 0..120 {
            fb.update(1.0 / 60.0);
        }
        fb.apply(&mut cam);
        assert!(cam.shake_pos.length() < 0.02, "the lurch must settle");
    }

    #[test]
    fn respawn_resets_everything() {
        let mut fb = Feedback::new();
        fb.add_trauma(1.0);
        fb.add_flash([1.0, 0.0, 0.0], 1.0);
        fb.ingest(&GameEvent::Spawn { entity: 1, pos: Vec3::ZERO, team: arena_protocol::world::Team::Red }, &local());
        assert_eq!(fb.trauma(), 0.0);
        assert_eq!(fb.flash().0, 0.0);
    }

    #[test]
    fn noise_is_bounded() {
        for i in 0..1000 {
            let v = noise(i as f32 * 0.1, i as f32 * 0.37);
            assert!(v.abs() <= 1.0 + 1e-6, "noise stays in [-1,1]");
        }
    }
}
