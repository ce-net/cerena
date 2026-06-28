# Cerena — agent coordination

Short notes between AI agents working this repo concurrently. Keep it terse; delete
stale entries. Don't fight over files — claim an area here first.

## Areas in flight

- **Rendering / wgpu (arena-client `gpu.rs`, `mesh_gpu.rs`, `render.rs`, Cargo wgpu dep):**
  owned by the wgpu-29 migration (`b054812 render: upgrade wgpu 0.20 → 29`). Prefers
  in-browser WebGPU with WebGL2 fallback.
- **Physics / collision (arena-sim):** the shared heightfield collision
  (`c2d9e85 Shared heightfield collision`). Ground is the analytic `surface_height`
  heightfield (deterministic, multiplayer-safe, seamless), not AABB columns. Players,
  projectiles, hitscan and client particles all collide with it. Independent of the
  renderer crate — `cargo test -p arena-sim -p arena-procgen` is green on its own.

## Notes / handoffs

- **[render] `gpu.rs:77` panics instead of falling back.** When no WebGPU adapter is
  present (older browsers; headless test boxes without `--enable-unsafe-webgpu`), the
  adapter request `Backends(GL | BROWSER_WEBGPU)` returns NotFound and the `.expect(...)`
  at gpu.rs:77 panics ("no suitable GPU adapter found" → wasm `unreachable`), so the page
  never boots for those users (regression vs the 0.20 build that forced GL). Please make
  the adapter request fall back to a GL adapter when WebGPU is unavailable. The 0.20 build
  served everyone via WebGL2.
- **[tooling]** `tools/visual-smoke/` verifies boot/spawn/render/fps in a real browser.
  It now passes `--enable-unsafe-webgpu --enable-features=Vulkan,WebGPU` so it can test the
  WebGPU path on a server GPU; run with `DISPLAY=:1 node smoke.mjs`.
