# Cerena visual smoke test

An automated headless-browser check that the Cerena web client actually **renders the
game and agrees with the backend simulation** — not just that the page loads. Reach for
this whenever you touch the renderer, worldgen/spawn, or the HUD wiring, instead of
eyeballing it by hand (or worse, deploying blind).

It drives a real browser at a deployment, waits for the wasm client to boot and the
player to spawn, then asserts:

- the ce-serve mesh bridge connected and the tab is hosting its zones,
- the player **spawned** — the HUD shows a live position + health from the sim,
- the 3D view is **not a near-empty sky** (terrain is drawn and you are not stuck under
  the ground — the back-face-culled "all sky" underground-spawn failure), and
- no uncaught page errors.

Exits `0` on pass, non-zero on failure, and always writes a screenshot — so it is usable
by hand, from CI, or from another agent.

## Run

```bash
cd cerena/tools/visual-smoke
npm install                          # once — pulls puppeteer-core
node smoke.mjs                       # test https://cerena.ce-net.com
node smoke.mjs http://localhost:8790 # test another origin
```

Env knobs: `URL`, `WAIT_MS` (spawn-wait budget, default 70000), `OUT` (screenshot path),
`SHOW=1` (headful window), `CHROME` (browser binary path).

## GPU

The renderer needs a WebGL2 context. If an X display (`$DISPLAY`) and a DRI device
(`/dev/dri`) are present, the tool drives the **real GPU** via ANGLE — the terrain/entity
pixel checks only run in this mode. With no GPU it falls back to **SwiftShader**, which
mis-handles the depth attachment and won't draw geometry, so only the boot + spawn + HUD
checks are meaningful there. On a headless box, expose a GPU display (e.g. an Xorg on
`:1`) and run with `DISPLAY=:1`.

## What it does NOT cover

Input/movement, combat, multi-tab quorum, and content hot-reload. It is a *smoke* test —
boot, spawn, render. Extend the `CHECKS` in `smoke.mjs` as those paths matter.
