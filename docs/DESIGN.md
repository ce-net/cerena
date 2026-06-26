# Cerena — Design (interpreting VISION-RAW.md)

This document is the engineering interpretation of `VISION-RAW.md`. When the two
disagree, the raw vision wins; fix this doc, not the vision.

Cerena is a **massively-multiplayer (10,000 concurrent) first-person procedural
mage RPG** that runs its authoritative simulation **on the CE mesh** — the players'
own nodes are the servers, trusted by cryptographic identity. Magic, items, the
tech tree, the world, and even the shaders are **data**, distributed as
content-addressed packs over the mesh and **hot-reloaded into a live match** so the
designer can change anything while 10,000 people are playing.

---

## 1. Why distributed, and how it holds 10,000 players

A single box cannot simulate 10k players at 64 Hz. The world is partitioned into a
grid of **zones** (`arena-protocol::world`), each ~128 m. Each zone is simulated by
**one authoritative CE node** chosen by stake-weighted rendezvous hashing
(`arena-mesh::authority`). Players see only their **area of interest** (own zone +
neighbours), so per-client bandwidth is flat regardless of total population.

- **Players ARE the servers.** A CE node id is an Ed25519 key with on-chain
  stake/karma, so a node hosting a zone is accountable. Honest, well-staked nodes
  win more zones; a malicious authority is caught by **cross-validation**
  (`arena-karma::crossval`): shadow simulators replay its ticks and vote.
- **Physics on server AND client.** The exact same deterministic simulation
  (`arena-sim`) runs on the authority (truth) and on the client (prediction). The
  client predicts locally for zero-latency feel and reconciles to the authority
  (`arena-net::predict`).
- **Hand-off** (`AuthorityMsg::AdoptPlayer`) moves a player between zone authorities
  seamlessly as they cross boundaries; the border overlap keeps cross-zone spells
  and rendering coherent.

10k players / ~50 players-per-zone ⇒ ~200 active zones, spread across ~200 well-
staked nodes. The map is effectively unbounded; new zones spin up as players spread.

### 1a. Redundancy — proximity replication (no lost state on a crash)

A zone authority is a single point of failure for the players in its zone. Losing
it must lose **nothing**. So each player's *full* authoritative state — transform,
health, **and** the sim-owned progression (XP, level, mana, inventory, statuses,
unlocked tech) — is checkpointed every ~1 s and replicated to the **K nearest other
players** (default K=3). Those peers are already exchanging packets with you (you're
in each other's AOI), so it's cheap, and they fail *independently* of the authority.

The mechanism (`arena-server::replication`, wire types in
`arena-protocol::message`):

- The authority emits `ReplicateCheckpoint` to each chosen holder; a holder stores
  the newest checkpoint per (player, zone) in a `ReplicaStore` and `ReplicaStored`-acks
  so the authority can confirm the replication factor is actually met.
- **Every** participating node runs a `ReplicaStore`, even one that owns no zones —
  including light/browser peers. Your backup is whoever is standing next to you.
- On failover, the successor authority (next in the rendezvous ranking) broadcasts
  `RequestReplicas`; surviving holders reply with `ReplicaBundle`s; the successor
  imports the newest checkpoint per player (`arena_sim::World::import_player`) and
  **rebuilds the zone losslessly**, then `Redirect`s those players to itself.

This is the same proximity-replica trick spacegame uses ("a standby adopts the
replicated sector snapshot"), generalized from per-sector snapshots to per-player
checkpoints. It composes with §1's failover: rendezvous hashing picks *who* takes
over; proximity replicas provide *what* state they take over with. The checkpoint
payload is opaque to holders (`bincode` of a sim type), so holding a replica needs
no game logic — and the same `import_player` primitive powers seamless hand-off.

---

## 2. Everything is hot-reloadable content (the keystone)

> "tweak things... expand tech tree, tweak shaders during ppeople are playing and it
> hot reloads for them while they are playing my changes are applied instantly."

The rule: **game systems are DATA, not code.** Code is a fixed *interpreter*; the
designer ships *definitions*. `arena-content` holds the entire game-design surface:

- `SpellDef` — a **spell graph** of primitive `EffectOp`s (projectile, beam, AoE,
  apply-status, heal, shield, summon, teleport, movement-impulse, chain, delay,
  repeat, conditional, spawn-field...). "Make your own spells" = compose primitives.
  New spells need **no code deploy**.
- `ItemDef`, `AbilityDef`, `StatusEffectDef`, `MovementModeDef` (parkour: wall-run,
  dash, double-jump, grapple, glide, blink), `TechTree` (nodes + prereqs + unlocks),
  `MaterialDef`/`ShaderDef` (WGSL source + params), `WorldGenParams`, `MobDef`,
  `MissionDef`.
- A **ContentPack** is a versioned, content-addressed bundle of all definitions
  (its id is its blob hash, like everything in ce-net). The live `ContentVersion`
  is a monotonically increasing epoch the coordinator publishes.

**Hot-reload protocol** (`arena-content::hotreload`, transported by `arena-server`):
1. Designer edits definitions → builds a new `ContentPack` → publishes the blob.
2. Coordinator broadcasts `ContentVersion(epoch, pack_hash)` on the session control
   plane.
3. Authorities fetch the pack (mesh blob), validate, and **swap at a tick boundary**
   (`ContentRegistry::swap_at(tick, pack)`), so the simulation never tears.
4. Clients fetch the same pack and hot-swap: re-resolve item/spell behavior by
   stable id, **recompile changed WGSL pipelines live**, regenerate procedural
   assets whose params changed.

Because live *state* references content **by stable id** (an inventory holds
`ItemId`s, a cast references a `SpellId`), swapping the definition changes behavior
without disturbing identity or saved progress. This is what makes "applied
instantly while people play" safe.

The interpreter (`arena-sim`) is the *only* thing that requires a binary deploy.
Everything a designer touches day-to-day is content.

### The shipped default pack

`arena_content::default_pack()` builds one coherent, validated, playable game and is
the reference for how much a single pack can express with **zero engine changes**. It
is assembled in two layers, both pure data over the same closed primitive sets:

- **Starter** (`default_pack.rs`): the four foundational schools — Pyromancy,
  Cryomancy, Arcana, Mobility — plus the base statuses, parkour kit, gear, tech tree,
  materials/shaders, mobs, missions, loot, spawn rules, triggers, and three game modes.
- **Expansion** (`expansion.rs`, *Tempest, Verdance & the Hollow Dead*): four more
  schools — Stormcalling, Verdancy, Necromancy, Chronomancy (plus a short Radiance
  prestige branch) — adding ~25 spells, ~18 statuses, new parkour, bosses (the Bone
  Colossus, the Tempest Djinn, the Grove Warden), two biomes (the Mire, the
  Stormpeaks), boss-hunt missions, and two game modes (Survival Horde, Relic Royale).
  It is grafted on by `expansion::apply(&mut pack)` and adds **no** new `EffectOp`,
  `StatusKind`, or `MovementKind` — proof that the primitive sets are expressive
  enough to triple the game as data alone.

`starter_pack()` builds the foundational layer alone; both packs satisfy
`ContentPack::validate()` (every cross-reference resolves).

---

## 3. The spell / ability VM

`SpellDef` is a small tree of `EffectOp`s evaluated by `arena-sim::magic` against the
world when a cast fires. Each op reuses sim primitives:

- shape ops resolve targets (ray/beam → `collision::ray_capsule`; AoE → sphere query;
  projectile → spawn a sim projectile carrying the *rest of the graph* as its
  on-impact continuation).
- effect ops mutate the world (damage with element + falloff, heal, apply a
  `StatusEffectDef` instance, push/pull impulse, shield HP, teleport/blink, summon a
  `MobDef`, spawn a persistent field entity).
- control ops compose (sequence, parallel, delay N ticks, repeat, chance, branch on
  target tag/element/threshold).

Mana, cast time, cooldown, and scaling-with-skill live on the `SpellDef`. Casting is
an *intent* (`input` carries the selected spell + aim); the authority validates mana
and cooldown exactly like a fire-rate gate, so cheats can't cast for free. Telemetry
(impossible cast cadence, mana underflow attempts) feeds `arena-karma`.

A player's "weapon" is just an equipped `AbilityDef` that wraps a `SpellDef`. Guns
from the original framing become a damage element/spell shape — no special case.

**Gravity spells** are expressed with the `EffectOp::Vortex { strength, vertical_bias }`
op: a radial force relative to the *current cast centre* (a projectile's impact, a
field's centre, the caster's aim). Positive `strength` pulls foes inward (a gravity
well), negative shoves them outward (an explosive blast). `vertical_bias` always
*lifts* (its magnitude scales with `|strength|`), so a positive bias throws foes up
whether the field pulls or pushes, and a small negative bias on a well pins them to
the floor. This single primitive composes the shipped gravity kit:

- **Gravity Well** — `Field { tick: Vortex(+) + Damage + Slow }`: a lingering pit that
  hauls foes to its heart and grinds them while they are held.
- **Singularity** — a slow projectile that on impact `Area { Vortex(+) }` implodes the
  crowd inward, then after a `Delay` `Area { Damage + Vortex(-) }` detonates outward.
  A gravity spell *with* an explosion — the showcase of Vortex in both directions.
- **Repulsion Nova** — an instant self-centred `Area { Damage + Vortex(-) }` panic
  button that flings everything nearby off its feet.

---

## 3a. Combat feel — movement, melee, and the feedback layer

The combat *systems* are deep; the goal of this layer is that they also **feel** good
and give the player feedback for everything. All of it stays server-authoritative and
deterministic (so prediction matches), with the *cosmetic* read derived on the client.

**Movement kit** (`MovementKind` in content, interpreted by `arena-sim::movement`):
the parkour set — dash, double-jump, wall-run, grapple, glide, blink, **wall-climb**,
ground-slam, slide, sprint — plus two new flow tools:

- **Flight** (`Fly`): hold the fly intent (V) with a flight mode equipped for full 3D
  control — look-ray steering, jump/crouch for vertical, gravity suppressed — paying a
  per-tick mana upkeep that cuts flight out the instant the pool runs dry.
- **Momentum Surge** (`MomentumBoost`): amplifies your *existing* horizontal velocity
  and adds a flat burst, but only above a minimum speed — it rewards flow (chaining off
  a slide, wall-run, or grapple) rather than a standing start.

Inputs are split cleanly: **MELEE (Q)** swings the weapon, **MOVE_ABILITY (F)** fires
the selected burst movement ability (dash/blink/grapple/surge), and the continuous
modes (sprint/glide/wall-run/fly/climb) ride their own held intents.

**Sword fighting** (`World::melee_attack`): a real melee weapon strike — a damaging arc
swept in front of the attacker with a **combo** that escalates Slash → Thrust → Spin as
swings chain inside a combo window, growing in damage, arc width and knockback. Gated by
a per-swing recovery (attack speed). Damage scales with the wielder's `power`, so a
battlemage with the **Runeblade** cuts as readily as they cast. All tunables live in
`TuningConfig` (`melee_*`), so melee is rebalanced by hot-reload like everything else.

**The feedback channel.** `GameEvent` carries, beyond the existing Shot/Hit/Explosion,
a set of feedback events: `Melee` (the swing arc + connect), `Knockback` (every force a
body feels — melee shove, impulse, vortex pull/push), `Buff` (a status landed, with a
beneficial flag), `Heal` (a restore tick), and `Shake` (an authored camera-shake hint
for set-pieces). Forces are *always* reported, so the client never has to re-derive a
shove from raw state.

**Client game-feel** (`arena-client::feedback`) reads that stream from the local
player's vantage and drives three channels onto the (otherwise pure) camera:

1. **Trauma camera shake** (Eiserloh model): events add trauma, trauma decays every
   frame, and the actual shake is `trauma²` — angular jitter, a horizon roll, and a
   positional offset from cheap deterministic noise. Explosions and the `Shake` hint
   scale by proximity.
2. **View kick** — a critically-damped-ish spring on the look angles, punched
   *directionally* by firing, swinging (heavier on the Spin finisher), and knockback
   (a side-shove kicks the view sideways; a forward shove snaps the head back).
3. **Screen flash + directional damage** — a decaying full-screen tint (red hurt,
   green heal, gold buff, violet debuff) plus the world direction the last damage came
   from, for a damage-indicator arrow.

Critically, none of this touches the *input* look angles the netcode sends — the wobble
lives only on transient `Camera` shake fields, so the authority still hit-tests the
player's true aim. **Feel is local; truth is authority.** `arena-client::particles`
adds the matching VFX bursts (a spark arc on a melee swing, impact/heal/explosion
sprays).

---

## 4. RPG: XP, levels, items, loot drop, tech tree, missions

> "when you kill someone they drop their items and their xp and abilties... everyone
> collects stuff, does missions and slowly the 10000 people gets more and more
> powerful and soon the map is unrecognizable. everyone goes from novice to experts."

- **Progression** (`arena-sim::rpg`): every entity has XP, level, attributes (power,
  focus, agility, vitality), mana pool, and a set of **unlocked tech-tree nodes**.
- **Tech tree** (`TechTree` in content): a DAG of nodes; spending XP/skill points
  unlocks nodes that grant abilities, spell ops, item recipes, movement modes, stat
  multipliers. Novice → expert is literally depth in this DAG.
- **Items** (`arena-sim::inventory`): equippable/consumable, modify stats, grant
  spell ops or movement modes, craftable from drops. Identity-stable `ItemId`s so
  hot-reload can rebalance an item under a player without losing it.
- **Loot on death**: on a confirmed kill the victim spawns a **loot entity** carrying
  a slice of their items, a chunk of their XP, and (configurable) one of their
  unlocked abilities. Anyone can collect it. This is the core economy loop that makes
  the population diverge and the world "unrecognizable" — power flows from the killed
  to the killer/collectors. Anti-grief: karma penalties for spawn-camping; a fraction
  of XP is bound (not fully lost) so a death is a setback, not a wipe.
- **World mutation**: players build/transform structures; world-gen seed + a
  persistent **edit log** per zone means the terrain and structures accumulate change.
  The map at hour 1000 is not the map at hour 0.
- **Missions** (`MissionDef`): procedural objectives that grant XP/items, seeding the
  collection loop.

---

## 4b. Items, upgrading, and impact — every piece does something

> "the items system with upgrading and magical items and swords and everything and
> armor and shoes — everything should have a noticeable effect and impact."

The law: there is no inert gear. The worst common drop still rolls a magic property; the
chase pieces rewrite a verb of how you play. This is built almost entirely as **data**
(`arena-content`), interpreted by a thin instance/forge/proc layer (`arena-sim`), so a
designer can triple the loot table live with no code deploy.

**The base vs the instance.** A base `ItemDef` (shared, hot-reloadable) holds stats,
signature procs, an `UpgradeProfile`, socket count, affix tags, and set membership. A
per-character `ItemInstance` (`arena_sim::item_instance`) layers on what is *rolled or
earned*: upgrade level (+N), quality (0..100), frozen affix rolls, socketed gems, an
applied enchant, an active runeword, and live progression (kills for "growing" items).
Instances reference content by **stable id**, so a hot-reload that re-tunes a base, an
affix, or a gem changes every instance live — without disturbing identity, sockets, or
upgrade level. `effective_mods` folds all layers into one `StatMods`; `effective_triggers`
folds all procs into one list. The rest of the sim consumes those exactly like plain gear.

**Stats that are all felt.** `StatMods` carries ~40 fields, each mapped to a real seam in
`arena_sim::rpg::derive` and the combat/movement hooks: crit chance/damage, lifesteal and
mana-leech, armor (flat + %), block, **thorns** (reflected in `spell_damage`), tenacity,
melee power/range/attack-speed, knockback, extra jumps and dash charges, projectile count
and pierce, AoE/cast/range multipliers, magic-find/gold-find/xp, summon power, and a full
per-element damage/resist block (`ElementMods`). If a number is on an item, a player feels it.

**The proc engine** (`arena_sim::item_procs`) is what makes "noticeable impact" literal. A
proc is a chance- and internal-cooldown-gated effect on an event (`OnHit`, `OnCrit`,
`OnKill`, `OnTakeDamage`, `OnLowHealth`, `OnDash`, `Aura`, `Interval`). The sim raises the
event at the natural moment — a swing connects, a kill lands, the per-tick heartbeat — and
the engine returns `ProcOutcome`s (nova, chain bolt, status, heal, shield, timed stat
surge) that `World::apply_proc_outcome` applies through existing primitives. It is
deterministic in the tick (rolls come from a `ForgeRng`, never ambient RNG), so an
authority and its shadow validators fire the same procs. A reentrancy guard stops a
damage proc from looping.

**Upgrading and the forge** (`arena_sim::forge`, rules in `arena_content::forge`): spend
**essences** to raise +N (safe early, risk of failure/downgrade later); **reforge** to
reroll affixes (chaos shard), **imprint** to lock one (binding sigil), **bore sockets**,
**fuse gems** up a tier, **polish** quality, **apply enchants**. All deterministic, all
reagent-gated, all hot-reloadable economy.

**Sockets, gems, runewords, sets.** Gems read differently in weapons vs armor vs jewellery,
so a socket is a real decision; a fixed ordered rune sequence activates a **runeword** that
*replaces* the individual gem bonuses with a far stronger combined one. **Sets** grant
escalating bonuses by equipped-piece count (the 2/3/5-piece ladder), culminating in a
build-defining proc.

**The shipped gear pack** (`arena_content::gear::apply`) demonstrates the whole loop with
zero new engine primitives: a deep affix pool, five fusing gem families, runewords,
enchants, two full sets (Pyrelord Regalia, Stoneward Bastion), and signature legendaries —
*The Hungering Edge* (grows per kill, rends on hit), *Ember Striders* (fire trail + a second
dash that ignites), *Echo of Creation*, *Winterheart Mantle* (frost-nova when struck,
overheal becomes ward), and the Mythic *Signet of the Conjunction*.

---

## 5. Procedural graphics — organic, non-sharp, high detail

> "all assets procedurally genrerated textures and shapes, high detail and organic
> non sharp shapes."

`arena-procgen` (pure, wasm-clean, no wgpu) generates geometry and textures as data;
`arena-client` uploads them to wgpu. Determinism by seed so every node/client agrees.

- **Organic shapes via SDF / metaballs + marching cubes / dual contouring**: the
  world, creatures, structures, and spell VFX are signed-distance fields blended with
  smooth-min, surface-extracted to meshes. No hard polygonal edges — smoothness is
  inherent to the SDF blend. Detail comes from layered domain-warped noise on the
  field and on displacement.
- **World gen**: domain-warped fractal noise → continents, caves (3D noise carving),
  biomes; placed structures and resource nodes. Same seed → same world on server
  (collision brushes derived from the SDF) and client (rendered mesh).
- **Procedural textures**: noise-based material synthesis (fbm, Worley, flow noise,
  triplanar) evaluated either CPU-side into texture data or in WGSL at runtime for
  "infinite detail." Material params live in `MaterialDef` content, so textures are
  hot-reloadable too.
- **Spell/VFX meshes**: generated from the same SDF toolkit, parameterized by the
  spell's elements — organic tendrils, blooms, fields.

Shaders are content (`ShaderDef` WGSL source); the client recompiles pipelines on
hot-reload so the designer can "tweak shaders while people play."

### 5a. Animation, rigs, and the asset pipeline

Procedural bodies need procedural *motion* and a way to become *drawable*. Two
wasm-clean crates (no wgpu, no I/O, deterministic) sit between content/procgen and the
renderer so the client stays a thin uploader:

- **`arena-anim`** — the animation system. A `Skeleton` is a flat, role-tagged joint
  hierarchy (`JointRole`: spine, head, leg, arm, tail), a `Pose` is its per-joint local
  transforms, and an `Animator` is a deterministic blend state-machine. Because
  creatures are generated with a *variable* body (2–5 limbs), motion is **procedural and
  role-driven**: the gait swings whatever joints are tagged legs, arms counter-swing,
  the spine breathes, tails spring — so a wisp and a wraith animate with the same code
  and *zero authored clips* (keyframed `AnimationClip`s are supported for anything that
  does ship authored motion). Also provides two-bone IK + look-at and
  critically-damped springs (reused for secondary motion, camera, reconciliation).
- **`arena-assets`** — the bake/cache layer that *makes rendering trivial*. It bakes a
  `ContentPack` into an `AssetBundle`: skinned meshes (procgen mesh + a rig built to sit
  inside it + derived skin weights), textures (procgen material synthesis), materials,
  shaders, and rigs — all indexed by lightweight handles. An `AssetServer` resolves an
  entity to its draw set (`mob_visual("mob.wisp")` → mesh + material + rig handles) and
  owns the **hot-reload swap**: a new pack bakes a fresh bundle off the hot path, stages
  it, and goes live between frames behind stable handles. The client just uploads each
  baked asset once and draws by handle; nothing in the bundle touches `wgpu`, so server,
  tests, and browser bake identical bytes.

The general **physics** primitives live in `arena-sim::physics` (deterministic,
wasm-clean): a uniform-grid spatial hash for neighbour/AoI/area-effect queries, a
kinematic body integrator (mobs, pickups) and a ballistic projectile step (the
substrate under the spell `Projectile` op), steering helpers for mob AI, and
framerate-independent smoothing — all built on the existing `arena-sim::collision`
capsule/raycast layer and reused throughout the sim.

### 5b. The living world (`arena-mythos`)

A world of ten thousand mages should feel alive when you stand still — and the things
players *do* should become **myth**. `arena-mythos` is that living soul: deterministic
and wasm-clean like the rest, so every authority dreams the same dream of the world.

- **Calendar** — a wheel of five seasons (Kindling, Highsun, Emberfall, Duskwane, the
  Long Dark), three moons (the Pale, the Ember, the Hollow) on coprime cycles, festivals
  that hang off them, and the once-an-age Grand Conjunction when all three moons go full
  and the Weave runs wild.
- **Leylines** — mana as a *substance that flows through the land*: wells of power you
  can drain with a big cast, claim with a tower, and fight over, connected by a diffusion
  sim that breathes with the seasons. Drain a region and it falls to the Doldrums; a
  charged region breeds mana storms.
- **Aetherweather** — magical weather (mana storms, blightfog, starfall, aurora, the
  dreaded Doldrums) driven by season + local leyline charge; it warps spell power,
  visibility, and what spawns.
- **Ecology** — creatures as a predator–prey food web that blooms, crashes, migrates,
  and mutates (seeding named foes), feeding the spawn system real population pressure.
- **Chronicle** (the heart) — a **myth engine**: it watches deeds (slayings, last
  stands, discoveries, forgings, duels), keeps a renown ledger with facets
  (valor/cunning/grace/dread/wisdom), and when a life crosses the bar — or fells a
  legend — it **mints a Legend**: a procedural mythic name + epithet, a saga line, and a
  world-imprint.
- **Firmament** — bright legends are hung in the night sky as named constellations that
  **boon their school when risen**; the sky becomes a readable history of the age.
- **Familiars** — a soul-bonded companion with a rolled personality, a deepening bond,
  tricks learned by your side, evolutions at bond milestones, and a voice.
- **Omens** — cryptic prophecies that foreshadow the world's great turns, so seers can
  prepare; **Attunement** — how a mage slowly *becomes* the magic they practise, with
  corruption the dark schools cost.

`WorldSoul` binds them and answers the one question combat asks —
`spell_power_at(element, pos)` — by folding season, weather, leyline charge, and the
risen stars into a single multiplier. The world you shape measurably changes your magic.

**Wired into the sim.** `arena-sim::living::LivingWorld` adapts the soul to the
simulation and `World` owns one (seeded from the active pack's bestiary + a ring of
leyline wells). Each tick the world soul advances and its news (season turns, festivals,
ecology blooms) surfaces as `GameEvent::Chat` from "the World". The integration is
deliberately a single deep hook plus a few shallow ones: `World::spell_damage_mult`
multiplies *every* spell by `living.spell_power(element, pos, owner)` (so the living
world + the caster's per-element attunement reach all magic through one seam); each cast
calls `living.on_cast` to deepen attunement (and court corruption in the dark schools);
each kill in `process_deaths` feeds a Slay deed to the Chronicle — felling a renowned
player carries their renown, so beating a champion is how you become a legend — and any
minted legend is heralded and its imprint queued; `realise_world_imprints` then raises
rising named foes, drops relic caches, and herald haunts. It is deterministic in the tick
+ the deeds fed in, so an authority and its shadow validators dream the same myth. (Each
zone keeps its own soul; the tick-derived layers agree across zones via the shared seed,
while cross-zone legend gossip is a future coordinator job.)

---

## 6. Crate map (additions in **bold**)

| Crate | Role |
|---|---|
| `arena-protocol` | Wire contract, zones, AOI, entity/snapshot/input, auth, karma shapes. |
| **`arena-content`** | **Hot-reloadable game data: spell VM, items, tech tree, abilities, movement modes, materials/shaders, worldgen params; content packs + registry + hot-reload protocol.** |
| **`arena-anim`** | **Deterministic, wasm-clean animation: role-tagged skeletons, poses, clips, two-bone IK + look-at, springs, and procedural role-driven locomotion (gait/idle/cast/hit/death) via a blend state-machine. Shared by client, assets, and sim.** |
| **`arena-procgen`** | **Deterministic procedural geometry (SDF/metaball/marching-cubes, organic) + procedural textures + world generation. wasm-clean, no wgpu.** |
| **`arena-assets`** | **Bakes content + procgen + rigs into GPU-ready assets (skinned meshes, textures, materials, shaders, rigs) indexed by handle, with a hot-reloadable `AssetServer`. wasm-clean, no wgpu — makes the renderer a thin uploader.** |
| **`arena-mythos`** | **The living world: deterministic seasons + three moons + festivals, leyline mana flow, aetherweather, a predator-prey ecology, and a myth engine (the Chronicle) that turns player deeds into named legends, constellations, omens, familiars, and personal attunement. `WorldSoul` folds it all into `spell_power_at`. wasm-clean, no I/O.** |
| `arena-sim` | Deterministic sim: movement+parkour (**dash/wall-run/grapple/glide/blink/wall-climb/slam/slide/sprint + flight + momentum-surge**), collision, **reusable physics (spatial hash, body/projectile integration, steering, smoothing)**, the spell/ability VM executor (**incl. the `Vortex` gravity op**), **combo melee combat**, mana, XP/level, inventory, loot, damage, death/respawn, anti-cheat telemetry. **Emits a rich feedback event stream (melee/knockback/buff/heal/shake).** |
| `arena-net` | Snapshot deltas, prediction+reconciliation, interpolation, clock, lag-comp timing. |
| `arena-karma` | Reporting, statistical anti-cheat, authority cross-validation, durable karma ledger. |
| `arena-mesh` | CE mesh transport adapters, stake-weighted zone authority, ticket verify, discovery. |
| `arena-server` | The authority orchestrator: tick loop, AOI snapshots, hand-off, content hot-reload distribution, anti-cheat hooks, coordinator/matchmaking. |
| `arena-server-bin` | Binary entrypoint to run an authority/coordinator on a node. |
| `arena-client` | wgpu+wasm renderer with procedural assets and hot-reloadable shaders, prediction loop, HUD, spell-crafting UI, **and the game-feel feedback layer (trauma camera shake, directional view kick, screen flash + damage indicator) driven by the sim's feedback events**. |

---

## 7. Security recap (server-authoritative, identity-bound)

- Client asserts **intent only** (move/look/cast/use). All outcomes derived server-
  side. Mana/cooldown/range/line-of-sight validated by the authority.
- Lag compensation bounded to `MAX_REWIND_MS`; out-of-range = anti-cheat signal.
- Statistical anti-cheat + player reports → durable karma; identity is a scarce CE
  node id so bans/penalties stick.
- Malicious *authorities* caught by cross-validation voting + bonded-stake slashing.
- Hot-reload packs are content-addressed and version-gated; only the session
  coordinator (a capability-holder) may publish a new `ContentVersion`.
