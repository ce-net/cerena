//! # arena-client — the first-person wgpu client for Cerena
//!
//! Cerena is a 10,000-player procedural mage RPG whose authoritative simulation
//! runs *on the CE mesh* (see `arena-sim` / `arena-server`). This crate is the
//! thing a player actually looks through: a first-person wgpu renderer that runs
//! both **natively** (a winit window, `pollster` driving the async setup) and in
//! the **browser** (`wasm32`, a canvas surface, a WebSocket to the ce-net relay).
//!
//! ## What makes this client unusual
//!
//! - **Everything is procedural.** Meshes and textures are grown by `arena-procgen`
//!   from the same content definitions the server uses; the GPU buffers in
//!   [`mesh_gpu`] are filled from those, never from authored art files.
//! - **Shaders are hot-reloadable.** A designer's WGSL edit ships as a new
//!   [`arena_content::pack::ContentPack`]; when it arrives ([`net`]) we recompile
//!   the affected wgpu pipelines live ([`hotreload`]) with no client restart. This
//!   is "tweak the look of the world while ten thousand people are playing in it".
//! - **Movement and casting feel instant.** The client predicts the local player
//!   locally with the *exact* `arena-sim` movement code ([`arena_net::SimReplay`])
//!   and reconciles to the authority's snapshots — input is applied this frame,
//!   corrections are smoothed over a few frames instead of popping.
//!
//! ## Module map
//!
//! - [`app`]        — the event loop (winit native / RAF-driven wasm) that ties
//!   the gpu, renderer, camera, input, netcode and hot-reload together.
//! - [`gpu`]        — wgpu instance/adapter/device/surface bring-up + resize.
//! - [`render`]     — the renderer: pipelines compiled from content shaders, the
//!   terrain + entity + HUD draw flow, the hot-reload pipeline swap seam.
//! - [`mesh_gpu`]   — `arena_procgen` mesh/texture -> wgpu buffers and textures.
//! - [`camera`]     — first-person camera -> view/projection + a GPU uniform.
//! - [`input`]      — mouse-look + keyboard/mouse -> one `InputFrame` per tick.
//! - [`hud`]        — health/mana/stamina/abilities/kill-feed/crosshair state.
//! - [`net`]        — the single network seam: `NetClient` trait + wasm WebSocket
//!   transport (the relay `/mesh-bridge`) and a native stub.
//! - [`hotreload`]  — stage + apply a new content pack: regen assets, recompile
//!   shaders.
//! - [`particles`]  — CPU particle system fed by procgen emitters + spell events.
//!
//! This crate is deliberately the *only* place graphics, windowing and the browser
//! transport live; the simulation, netcode and content crates stay platform-free.

pub mod app;
pub mod camera;
pub mod gpu;
pub mod hotreload;
pub mod hud;
pub mod input;
pub mod mesh_gpu;
pub mod net;
pub mod particles;
pub mod render;

/// Eye height (metres) above an entity's capsule centre. `arena-sim` places a
/// player's `pos` at the capsule centre (a standing half-height above its feet);
/// the camera sits a little above that so the view matches a human eyeline.
pub const EYE_OFFSET_M: f32 = 0.6;

/// Default vertical field of view, radians (~90 deg horizontal on 16:9).
pub const DEFAULT_FOV_Y: f32 = std::f32::consts::FRAC_PI_2 * 0.85;

// ---------------------------------------------------------------------------
// Native entry point
// ---------------------------------------------------------------------------

/// Native entry point: build a winit window, bring up wgpu, and run the loop.
///
/// Setup is async (adapter/device requests await), so we block on it with
/// `pollster` once at startup; the per-frame path is fully synchronous.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() {
    // Standard env-filter logging on native (the wasm path uses tracing-wasm).
    let _ = env_logger::try_init();
    pollster::block_on(app::App::run());
}

// ---------------------------------------------------------------------------
// wasm entry point
// ---------------------------------------------------------------------------

/// Browser entry point. Wired as the wasm-bindgen `start` function so simply
/// loading the module boots the client: install a readable panic hook, route
/// `tracing` to the devtools console, then kick off the async bring-up on the
/// canvas. Returning immediately hands control back to the browser event loop;
/// `App::run` drives rendering from `requestAnimationFrame` thereafter.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();
    // `App::run` is async on wasm; spawn it on the browser's microtask queue.
    wasm_bindgen_futures::spawn_local(app::App::run());
}
