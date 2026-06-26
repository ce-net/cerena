# Cerena

A **10,000-player first-person procedural mage RPG** that runs its authoritative
simulation **on the CE mesh** — the players' own nodes are the servers, trusted by
cryptographic identity. Magic, items, the tech tree, the world, and even the shaders
are **data**, distributed as content-addressed packs and **hot-reloaded into a live
match** so the designer can change anything while thousands of people are playing.

> Read [`docs/VISION-RAW.md`](docs/VISION-RAW.md) for the unedited vision in Leif's
> own words, and [`docs/DESIGN.md`](docs/DESIGN.md) for the engineering interpretation.

Status: first cut, written but **not yet compiled** (built for review, fix-as-we-go).

## Crates

| Crate | Role |
|---|---|
| `arena-protocol` | Wire contract: zones, AOI, entity/snapshot/input, auth tickets, karma shapes. wasm-clean. |
| `arena-content` | Hot-reloadable game data: the **spell VM**, items, tech tree, abilities, movement modes, statuses, materials/shaders, worldgen, mobs, missions, game modes, loot tables, spawn rules, a data-driven **trigger engine**, global tuning — plus content packs, the registry, and the hot-reload protocol. |
| `arena-procgen` | Deterministic procedural generation: organic SDF/surface-nets meshes, procedural textures, world gen, creatures, spell VFX. wasm-clean, no GPU. |
| `arena-sim` | Deterministic fixed-timestep sim: parkour movement, collision, the spell/ability interpreter, mana, XP/levels, inventory, **loot-on-death**, damage, respawn, anti-cheat telemetry, content hot-swap. |
| `arena-net` | Netcode: delta snapshots vs acked baseline, client prediction + reconciliation, entity interpolation, clock sync, lag-comp timing. |
| `arena-karma` | Reporting, statistical anti-cheat, **authority cross-validation**, durable karma ledger. |
| `arena-mesh` | CE mesh transport adapters, **stake-weighted zone-authority assignment**, ticket verification, node discovery. |
| `arena-server` | The authority orchestrator: per-zone sim, AOI snapshots, hand-off, content hot-reload distribution, coordinator/matchmaking, anti-cheat hooks. Actor design: mesh tasks enqueue, one tick loop owns the sim. |
| `arena-server-bin` | Binary entrypoint (`arena-server`) to run an authority/coordinator on a CE node. |
| `arena-client` | wgpu + wasm first-person client: procedural assets, **hot-recompilable WGSL shaders**, prediction loop, HUD. |
| `cerena-e2e` | End-to-end tests: real-VM at-scale deployment, headless load generation (thousands of bots), and fault-tolerance scenarios (authority crash/failover, netsplit, malicious authority, mass disconnect). |

## How it scales to 10,000

The world is a grid of ~128 m **zones**; each is simulated by one authoritative node
chosen by stake-weighted rendezvous hashing. Players receive only their
**area-of-interest** (own zone + neighbours), so per-client bandwidth is flat
regardless of total population. Boundary crossings hand a player to the next
authority; a crashed authority fails over to the next-ranked node deterministically;
a *lying* authority is caught by cross-validation voting and slashed.

## Security model

Server-authoritative: the client asserts **intent only** (move/look/cast); all
outcomes are derived by the authority and validated (mana, cooldown, range, bounded
lag-comp). Statistical anti-cheat + player reports feed a durable karma score keyed
to the scarce CE node id, so bans stick. See [`docs/DESIGN.md`](docs/DESIGN.md) §7.

## Building (when CPU is free)

```
cargo build                       # default-members: everything except the wgpu client
cargo build -p arena-client       # the client (native; also targets wasm32)
cargo test -p arena-sim           # deterministic sim tests
cargo test -p cerena-e2e          # pure harness tests (scale/fault tests are #[ignore])
```
