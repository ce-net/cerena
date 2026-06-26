# CE Arena — Raw Vision (Leif's words, verbatim)

> This file is the unedited source of truth for the game's vision. Captured exactly
> as Leif wrote it. Do not paraphrase, "improve", or reorganize the ideas in this
> file. Design docs interpret it elsewhere; this stays raw.

---

## 2026-06-26 — first framing

> Just build and dont compile and verify since our mac cpu is full - just write
> code and eye ball it and we can fix it later. Write our 1000s of people fps game
> we talk about in our web app. in rust, wasm, wgpu, distributed backend (you have
> to for this many players), 100% efficency, replicated state, secure, latency
> optimized, physics on server and everything on server AND on users devices - they
> ARE the server in ce-net since node is tied to identity we can trust people. the
> game will have a reporting system and karma system. start with the complex
> backend. Then build the fps game

## 2026-06-26 — the actual game

> use procedural graphics for the game - all assets procedurally genrerated textures
> and shapes, high detail and organic non sharp shapes. This is what the game is
> about: Mage game with advanced magic and spells and weapons and tech trees -
> massively multiplayer 10000 players on same server. tech trees. make your own
> spells. parkour. movement. abilities. mana.
> just lots and lots and lots of different combat, movement and tech systems. A
> procedural open magical mystery world with thousands of players. rpg with upgrades,
> items xp, advanced fun movement in first person. Keep all game systems hot
> reloadable for production so that i can develop the game, add featurs and items,
> tweek things, add items, expand tech tree, tweak shaders during ppeople are playing
> and it hot reloads for them while they are playing my changes are applied instantly.
> when you kill someone they drop their items and their xp and abilties so everyone
> collects stuff, does missions and slowly the 10000 people gets more and more
> powerful and soon the map is unrecognizable.
> everyone goes from novice to experts. Document these ideas somwhere raw exactly as
> i put them - dont fuck with my ideas.

---

## Extracted pillars (a checklist, not a rewrite — the words above rule)

- [ ] Procedural graphics: ALL assets procedurally generated textures + shapes. High
      detail. Organic, non-sharp shapes.
- [ ] Mage game: advanced magic, spells, weapons, tech trees.
- [ ] Make your own spells.
- [ ] Massively multiplayer: 10000 players on the same server.
- [ ] Tech trees.
- [ ] Parkour, movement, abilities, mana.
- [ ] Lots and lots of different combat, movement, and tech systems.
- [ ] Procedural open magical mystery world.
- [ ] RPG: upgrades, items, XP.
- [ ] Advanced, fun first-person movement.
- [ ] Hot reloadable game systems IN PRODUCTION: add features/items, tweak things,
      expand tech tree, tweak shaders WHILE people are playing — applied instantly,
      hot reloads for live players.
- [ ] Kill -> victim drops their items, their XP, and their abilities. Everyone
      collects stuff, does missions. The 10000 people get more and more powerful.
      The map becomes unrecognizable over time.
- [ ] Everyone goes from novice to expert.
- [ ] Backend first (it has to be distributed for this many players). Players ARE the
      server (CE node tied to identity = trust). 100% efficiency, replicated state,
      secure, latency optimized. Physics on server AND on clients.
- [ ] Reporting system + karma system.
