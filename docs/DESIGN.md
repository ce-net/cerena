# CE Arena — Design (interpreting VISION-RAW.md)

This document is the engineering interpretation of `VISION-RAW.md`. When the two
disagree, the raw vision wins; fix this doc, not the vision.

CE Arena is a **massively-multiplayer (10,000 concurrent) first-person procedural
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

---

## 6. Crate map (additions in **bold**)

| Crate | Role |
|---|---|
| `arena-protocol` | Wire contract, zones, AOI, entity/snapshot/input, auth, karma shapes. |
| **`arena-content`** | **Hot-reloadable game data: spell VM, items, tech tree, abilities, movement modes, materials/shaders, worldgen params; content packs + registry + hot-reload protocol.** |
| **`arena-procgen`** | **Deterministic procedural geometry (SDF/metaball/marching-cubes, organic) + procedural textures + world generation. wasm-clean, no wgpu.** |
| `arena-sim` | Deterministic sim: movement+parkour, collision, the spell/ability VM executor, mana, XP/level, inventory, loot, damage, death/respawn, anti-cheat telemetry. |
| `arena-net` | Snapshot deltas, prediction+reconciliation, interpolation, clock, lag-comp timing. |
| `arena-karma` | Reporting, statistical anti-cheat, authority cross-validation, durable karma ledger. |
| `arena-mesh` | CE mesh transport adapters, stake-weighted zone authority, ticket verify, discovery. |
| `arena-server` | The authority orchestrator: tick loop, AOI snapshots, hand-off, content hot-reload distribution, anti-cheat hooks, coordinator/matchmaking. |
| `arena-server-bin` | Binary entrypoint to run an authority/coordinator on a node. |
| `arena-client` | wgpu+wasm renderer with procedural assets and hot-reloadable shaders, prediction loop, HUD, spell-crafting UI. |

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
