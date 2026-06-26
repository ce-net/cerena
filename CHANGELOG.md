# Cerena — Changelog

A running log of deliberate changes to Cerena. Newest first. Cerena is written in
"creativity mode" first cuts (write + eyeball, compile later), so entries describe
intent and surface area, and call out what is wired vs still a TODO.

---

## 2026-06-26 — Items system: upgrading, magical items, swords/armor/shoes with real impact

What I built — Cerena's item system

The expansion is ~2,100 new lines across **9 new files** plus wiring, built almost entirely as **hot-reloadable data** (the keystone), with a thin interpreter in `arena-sim`. Core law honored: **no inert gear** — the worst common drop still rolls a magic property; chase pieces rewrite a verb of play.

**Stats that are all felt** — `StatMods` widened from 10 → ~40 fields, every one mapped to a real seam in `rpg::derive`: crit chance/damage, lifesteal, mana-leech, armor (flat + %), block, thorns, tenacity, melee power/range/attack-speed, knockback, extra jumps + dash charges, projectile count + pierce, AoE/cast/range multipliers, magic-find/xp, summons, and a full per-element damage/resist block.

**The build layer (new content modules):**
- `affix.rs` — rollable prefix/suffix ranges ("Flaming … of Storms"), each can carry a proc
- `gem.rs` — gems that read differently in weapon vs armor vs jewellery, fusing chipped→perfect
- `itemset.rs` — set bonus ladders (2/3/5-piece)
- `enchant.rs` — permanent enchants + **runewords** (ordered rune sequences that replace gem stats with a stronger combined bonus)
- `forge.rs` — the upgrade/reforge economy config
- `gear.rs` — the showcase pack grafted on with zero new engine primitives

**The sim layer (new):**
- `item_instance.rs` — the per-copy state (base + upgrade + quality + affixes + sockets + enchant + runeword + kills) folded into one `effective_mods`/`effective_triggers`; references content by id so a hot-reload retunes your +9 sword live
- `forge.rs` — deterministic splitmix RNG: `roll_drop` (magic-find-aware), upgrade (safe early, risky late), reforge, imprint, socket, fuse, polish, enchant
- `item_procs.rs` — the proc engine (ProcEvent/Context/Outcome + internal-cooldown runtime), deterministic so shadow-validators agree

**Wired into combat** (`World`): procs fire on melee, on-hit, on-take-damage (with thorns reflect), on-kill (with weapon kill-credit + health/mana-on-kill), and a per-tick heartbeat for auras/trails. Temp stat surges fold into `combined_mods`.

**Signature gear:** *The Hungering Edge* (grows stronger per kill, rends on hit), *Ember Striders* (fire trail + igniting second dash), *Echo of Creation*, *Winterheart Mantle* (frost-nova when struck, overheal→ward), the Mythic *Signet of the Conjunction*; sets Pyrelord Regalia + Stoneward Bastion; runewords Stormbringer + Bulwark.

**One honest TODO:** `CastSpell`/`Summon` proc outcomes are intentional no-ops in the contained dispatch path — they need a full `CastContext` to route through the spell VM. Everything else (nova, chain bolt, status, heal, shield, stat surge) is applied through existing primitives.

Documented in `cerena/docs/DESIGN.md §4b`. When you want it compiled, the first move is still the workspace-wide `ce_rs` dep fix, then this'll surface its own type errors to clean up.

### Files

New (arena-content): `affix.rs`, `gem.rs`, `itemset.rs`, `enchant.rs`, `forge.rs`, `gear.rs`
New (arena-sim): `item_instance.rs`, `forge.rs`, `item_procs.rs`
Changed: `arena-content/src/item.rs` (StatMods + ElementMods + proc/upgrade types + ItemDef build-layer + Default), `ids.rs`, `pack.rs`, `registry.rs`, `lib.rs`, `default_pack.rs`, `expansion.rs`; `arena-sim/src/rpg.rs` (Derived), `inventory.rs` (instances + sets), `world.rs` (proc dispatch wiring), `lib.rs`; `docs/DESIGN.md` (§4b).
