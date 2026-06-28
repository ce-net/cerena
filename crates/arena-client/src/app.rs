//! The application: the event loop and the per-frame orchestration.
//!
//! This is where the netcode, prediction, rendering, input and hot-reload meet.
//! It runs the same logic on native (a winit window driven by `EventLoop::run`) and
//! in the browser (the same handler driven from `requestAnimationFrame` via
//! `EventLoopExtWebSys::spawn`); only window/transport construction differs.
//!
//! ## The loop, precisely
//!
//! Rendering happens every animation frame (`RedrawRequested`); simulation input is
//! produced on a **fixed-tick accumulator** at [`arena_protocol::TICK_HZ`] so
//! prediction matches the server's fixed step exactly. Each frame:
//!
//! 1. **pump the network** — drain [`ServerMsg`]s and apply them: `JoinAccept`
//!    seeds the local entity id + sim, `Snapshot` feeds [`ClientWorld::apply_snapshot`]
//!    (whose drained [`GameEvent`]s spawn particles and update the HUD), `Redirect`
//!    re-homes us to a new authority, `Pong` feeds clock sync, `Karma`/`Kick` adjust
//!    session state;
//! 2. **step input** — for every elapsed fixed tick, sample [`Input`] into one
//!    [`InputFrame`], push it into the predictor ([`ClientWorld::push_input`], applied
//!    immediately so movement is instant), and periodically ship an [`InputBatch`];
//! 3. **update the camera** — snap it onto the predicted local player (position) and
//!    the live look angles (so aim is smooth at render rate, not tick rate);
//! 4. **render** — interpolated remotes + the predicted local, then VFX + HUD.
//!
//! Content swaps are applied at the top of the frame (a safe boundary) via
//! [`HotReload::apply`], recompiling shaders and rebaking materials live.

use std::sync::Arc;

use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, WindowEvent};
use winit::event_loop::EventLoop;
use winit::keyboard::PhysicalKey;
use winit::window::Window;

use arena_content::ContentRegistry;
use arena_content::hotreload::ContentVersion;
use arena_content::pack::ContentPack;
use arena_net::{ClientWorld, SimReplay};
use arena_protocol::entity::EntityState;
use arena_protocol::input::InputFrame;
use arena_protocol::message::{ClientMsg, ServerMsg};
use arena_protocol::{EntityId, NodeId, TICK_DT};

use crate::camera::Camera;
use crate::feedback::{Feedback, LocalView};
use crate::gpu::Gpu;
use crate::hotreload::HotReload;
use crate::hud::HudState;
use crate::input::Input;
use crate::net::NetClient;
use crate::particles::ParticleSystem;
use crate::render::Renderer;

/// Closure that rebuilds a single-player replay world seeded at an authoritative
/// state. Boxed so the [`App`] is a concrete (non-generic) type; `Box<dyn Fn>`
/// satisfies the [`SimReplay`] `Fn` bound.
type RebuildFn = Box<dyn Fn(&EntityState) -> arena_sim::World>;

/// The concrete replay sim and client-world types this client uses.
type ReplaySim = SimReplay<RebuildFn>;
type Net = Box<dyn NetClient>;

/// Send an input batch this often (in ticks). Matching the snapshot cadence
/// (~20 Hz) keeps upstream bandwidth modest while re-sending unacked frames in each
/// batch makes a single dropped packet harmless.
const INPUT_SEND_EVERY_TICKS: u32 = arena_protocol::TICKS_PER_SNAPSHOT;

/// Send a liveness/clock ping this often, in milliseconds.
const PING_INTERVAL_MS: u64 = 1000;

/// Never advance more than this many fixed ticks in one frame, so a long stall (tab
/// backgrounded, GC pause) cannot trigger a death-spiral of catch-up simulation.
const MAX_CATCHUP_TICKS: u32 = 8;

/// The whole client.
pub struct App {
    renderer: Renderer,
    camera: Camera,
    input: Input,
    hud: HudState,
    particles: ParticleSystem,
    /// Game-feel: trauma camera shake, view kick, screen flash (driven by events).
    feedback: Feedback,

    /// Live content + the hot-reload driver.
    registry: ContentRegistry,
    hotreload: HotReload,

    /// The network transport (browser WebSocket / native stub).
    net: Net,

    /// The predicted+interpolated world. Rebuilt on join/redirect with the assigned
    /// local entity id.
    cw: ClientWorld<ReplaySim>,
    /// Our CE node id (identity) and our per-zone entity id once joined.
    local_node: NodeId,
    local_id: EntityId,
    joined: bool,
    /// The static collision map driving local prediction. A placeholder test arena
    /// until the real content-addressed [`arena_sim::MapDef`] is fetched on join.
    map: arena_sim::MapDef,

    // --- timing ---
    last_ms: u64,
    /// Fixed-tick accumulator, seconds.
    accumulator: f32,
    tick_counter: u32,
    last_ping_ms: u64,

    /// Browser hosting: when present (wasm, served through ce-serve), the tab runs the
    /// authoritative replica engine itself — playing IS hosting — instead of the
    /// predict-against-a-remote-authority path. `None` falls back to that legacy path.
    #[cfg(target_arch = "wasm32")]
    host: Option<crate::host::BrowserHost>,

    /// Hosted-render bookkeeping: the zones whose terrain is already resident on the
    /// GPU. Each hosted frame uploads at most one not-yet-resident zone (local zone
    /// first), so the terrain streams in instead of freezing the first frames.
    #[cfg(target_arch = "wasm32")]
    uploaded_zones: std::collections::HashSet<(i32, i32)>,

    /// Free-running hosted-frame counter, used to throttle the periodic re-join.
    #[cfg(target_arch = "wasm32")]
    frame_count: u32,
}

/// Surface-extraction grid resolution per zone axis for the visible terrain. A balance
/// between terrain fidelity and the cost of the one-off CPU extraction at boot / each
/// time the player crosses into a new area of interest.
// Kept moderate on purpose: terrain is extracted on the main thread when the area of
// interest changes (9 zones at boot), so res^3 field evals must stay snappy on a
// browser. 48 booted smoothly across test hardware; higher (64) added a multi-second
// hitch. Revisit once extraction moves to a worker.
#[cfg(target_arch = "wasm32")]
const TERRAIN_RES: usize = 48;

/// Surface-extraction resolution for the shared player-avatar mesh. Built once at
/// start-up (not per frame), so this can be comfortably fine without a runtime cost.
const ENTITY_MESH_RES: usize = 22;

impl App {
    /// Build and run the client: window + gpu bring-up, then the event loop.
    /// Async because gpu setup awaits; the loop itself is synchronous.
    pub async fn run() {
        let event_loop = EventLoop::new().expect("create event loop");
        let window = build_window(&event_loop);
        let gpu = Gpu::new(window.clone()).await;
        #[allow(unused_mut)]
        let mut app = App::new(gpu);

        // In the browser, connect the hosting engine: this tab becomes a replica of the
        // zones around the player and reconciles with the rest by the state-hash quorum.
        #[cfg(target_arch = "wasm32")]
        {
            use arena_protocol::auth::SessionId;
            use arena_protocol::world::ZoneId;
            let session = SessionId("cerena-dev".to_string());
            match crate::host::BrowserHost::connect(
                session,
                arena_content::default_pack(),
                1,
                ZoneId::new(0, 0),
            )
            .await
            {
                Ok(h) => {
                    tracing::info!("browser host connected — this tab is now hosting its zones");
                    app.local_node = h.node_id_string();
                    app.host = Some(h);
                    app.joined = true;
                    // Let cosmetic particles collide with the same procedural ground the
                    // sim and terrain mesh use, so sparks settle on the surface.
                    app.particles.set_terrain(arena_content::default_pack().worldgen);
                }
                Err(e) => tracing::error!("browser host connect failed (is this served via ce-serve?): {e:?}"),
            }
        }

        run_event_loop(event_loop, window, app);
    }

    /// Assemble the client around an initialised [`Gpu`].
    fn new(gpu: Gpu) -> App {
        let mut renderer = Renderer::new(gpu);

        // Build the one shared player-avatar mesh and make it resident. Without this
        // the renderer has no entity geometry, so every player, mob, projectile and
        // pickup the sim spawns is invisible — the world looks empty even when the
        // backend is fully populated. The mesh is grown procedurally (matched to the
        // sim's collision capsule) once at start-up; per-entity colour/scale comes
        // from the instance tint in `render.rs`, not the mesh.
        let avatar = arena_procgen::creature::player_mesh(ENTITY_MESH_RES);
        let avatar_gpu =
            crate::mesh_gpu::GpuMesh::upload(&renderer.gpu.device, &avatar);
        renderer.set_entity_mesh(avatar_gpu);

        // Start with an empty content registry; the real pack is fetched on the
        // first ContentVersion. The map is the bundled test arena until join.
        let registry = ContentRegistry::bootstrap();
        let map = arena_sim::MapDef::test_arena();

        // A placeholder local id (0) until JoinAccept assigns the real one. The
        // predictor stays dormant (renders nothing for the local player) until the
        // first reconcile seeds authoritative truth.
        let cw = make_client_world(0, &map);

        // Native uses the loopback stub; the browser opens a real mesh-bridge socket.
        let net: Net = make_net_client();

        App {
            renderer,
            camera: Camera::default(),
            input: Input::new(),
            hud: HudState::new(),
            particles: ParticleSystem::new(),
            feedback: Feedback::new(),
            registry,
            hotreload: HotReload::new(),
            net,
            cw,
            local_node: NodeId::new(),
            local_id: 0,
            joined: false,
            map,
            last_ms: now_ms(),
            accumulator: 0.0,
            tick_counter: 0,
            last_ping_ms: 0,
            #[cfg(target_arch = "wasm32")]
            host: None,
            #[cfg(target_arch = "wasm32")]
            uploaded_zones: std::collections::HashSet::new(),
            #[cfg(target_arch = "wasm32")]
            frame_count: 0,
        }
    }

    /// Handle window/canvas resize.
    fn resize(&mut self, width: u32, height: u32) {
        self.renderer.resize(width, height);
    }

    // -----------------------------------------------------------------------
    // The frame
    // -----------------------------------------------------------------------

    /// One animation frame: pump net, step fixed-tick input/prediction, update the
    /// camera, and render. `now` is the current local time in ms.
    fn frame(&mut self) {
        // Browser hosting path: the tab IS the server for its zones. Runs the replica
        // engine instead of predicting against a remote authority.
        #[cfg(target_arch = "wasm32")]
        if self.host.is_some() {
            self.frame_hosted();
            return;
        }

        let now = now_ms();
        let dt = ((now.saturating_sub(self.last_ms)) as f32 / 1000.0).min(0.25);
        self.last_ms = now;

        // --- 0. apply any staged content at this safe boundary (live shader/material
        //        swap). A no-op unless a ContentVersion staged a new pack. ---
        if self.registry.has_pending() {
            self.hotreload.apply(&mut self.registry, &mut self.renderer);
        }

        // --- 1. pump the network ---
        for msg in self.net.poll_messages() {
            self.handle_server_msg(msg, now);
        }

        // --- 2. fixed-tick input + prediction ---
        self.accumulator += dt;
        let mut steps = 0;
        while self.accumulator >= TICK_DT && steps < MAX_CATCHUP_TICKS {
            self.accumulator -= TICK_DT;
            steps += 1;
            self.tick_counter = self.tick_counter.wrapping_add(1);

            // The server tick we believe is current (stamped on the frame for
            // lag-comp); 0 until the clock has synced.
            let client_tick = if self.joined {
                self.cw.estimated_server_tick(now)
            } else {
                0
            };
            let frame: InputFrame = self.input.end_tick(client_tick);
            self.hud.set_selected_slot(frame.weapon_slot);

            if self.joined {
                // Apply locally *now* (instant feel) and remember it for reconcile.
                self.cw.push_input(frame);

                // Ship a batch (with the latest ack + all unacked frames) on cadence.
                if self.tick_counter % INPUT_SEND_EVERY_TICKS == 0 {
                    let batch = self.cw.make_input_batch(self.cw.ack_tick());
                    self.net.send(ClientMsg::Input(batch));
                }
            }
        }
        // If we exhausted the catch-up budget, drop the backlog rather than chase it.
        if steps == MAX_CATCHUP_TICKS {
            self.accumulator = 0.0;
        }

        // --- liveness / clock ping ---
        if self.joined && now.saturating_sub(self.last_ping_ms) >= PING_INTERVAL_MS {
            self.last_ping_ms = now;
            self.net.send(ClientMsg::Ping { client_time_ms: now });
        }

        // --- 3. gather the renderable world + update the camera ---
        let mut entities = self.cw.render_entities(now);

        // Camera position follows the predicted local player; look angles come from
        // live input so aiming is smooth between ticks. Then drop the local body
        // from the draw list (we are inside its head in first person).
        if let Some(local) = entities.iter().find(|e| e.id == self.local_id).cloned() {
            self.camera.follow(&local);
        }
        let (yaw, pitch) = self.input.look();
        self.camera.yaw = yaw;
        self.camera.pitch = pitch;
        entities.retain(|e| e.id != self.local_id);

        // Advance the feel springs and stamp shake/kick/lurch onto the camera *after*
        // it has been snapped onto the player, so the wobble rides on top of aim.
        self.feedback.update(dt);
        self.feedback.apply(&mut self.camera);

        // --- 4. advance cosmetics, then draw ---
        self.particles.update(dt);
        self.hud.set_rtt(self.cw.rtt_ms());
        self.hud.tick(dt);

        // TODO: thread `&self.hud` and `&self.particles` into the renderer's HUD and
        //       VFX passes (the draw flow seams exist in render.rs).
        self.renderer.render(&entities, &self.camera);
    }

    /// The browser-hosting frame: sample fixed-tick input and feed it to the local
    /// replica engine (which broadcasts it so every replica applies it identically),
    /// advance the hosted zones to the shared wall-clock tick, then render straight from
    /// the authoritative replica — no prediction/reconciliation, because this tab holds
    /// real authority, shared by quorum with the other replicas.
    #[cfg(target_arch = "wasm32")]
    fn frame_hosted(&mut self) {
        let now = now_ms();
        let dt = ((now.saturating_sub(self.last_ms)) as f32 / 1000.0).min(0.25);
        self.last_ms = now;

        if self.registry.has_pending() {
            self.hotreload.apply(&mut self.registry, &mut self.renderer);
        }

        let host = self.host.clone().expect("frame_hosted only runs with a host");

        // Fixed-tick input: one InputFrame per elapsed tick, handed to the engine.
        self.accumulator += dt;
        let mut steps = 0;
        while self.accumulator >= TICK_DT && steps < MAX_CATCHUP_TICKS {
            self.accumulator -= TICK_DT;
            steps += 1;
            self.tick_counter = self.tick_counter.wrapping_add(1);
            let frame: InputFrame = self.input.end_tick(self.tick_counter);
            self.hud.set_selected_slot(frame.weapon_slot);
            host.submit_input(frame);
        }
        if steps == MAX_CATCHUP_TICKS {
            self.accumulator = 0.0;
        }

        // Make sure we actually have a body in the world. Re-issued until the spawn
        // takes (throttled so we don't spam the zone while the Join's INPUT_DELAY ticks
        // elapse), then a no-op for the rest of the session.
        if self.frame_count % 20 == 0 {
            host.ensure_joined();
        }

        // Advance the hosted replicas (drain peer inputs, step, publish proofs, merge).
        host.advance();

        // Stream the visible terrain in one zone per frame. The host seeds its zones
        // asynchronously (after the first mesh round-trips) and the local zone is
        // extracted first, so the world appears almost immediately and the neighbours
        // fill in over the next frames — instead of one multi-second extraction of all
        // nine zones that froze the frame loop (and starved the spawn). Extracting just
        // the missing zone each frame keeps every frame responsive.
        for zone in host.hosted_zones_local_first() {
            if self.uploaded_zones.insert((zone.x, zone.z)) {
                let mesh = host.zone_render_mesh(zone, TERRAIN_RES);
                if !mesh.is_empty() {
                    self.renderer.push_world_mesh(&mesh);
                    tracing::info!("uploaded terrain for zone {},{}", zone.x, zone.z);
                }
                break; // at most one heavy extraction per frame
            }
        }

        // Render straight from the authoritative replica world.
        let mut entities = host.render_entities();
        let local_id = host.local_entity().unwrap_or(0);
        self.local_id = local_id;
        if let Some(local) = entities.iter().find(|e| e.id == local_id).cloned() {
            self.camera.follow(&local);
        }
        // Reflect the authoritative state into the page HUD a few times a second, so
        // the frontend overlay tracks the backend (health, position, nearby players)
        // without flooding the DOM every animation frame.
        if self.frame_count % 6 == 0 {
            host.publish_hud();
        }
        self.frame_count = self.frame_count.wrapping_add(1);
        if self.frame_count % 180 == 0 {
            tracing::info!("MP zones={} entities={} local_id={}", host.hosted_zone_count(), entities.len(), local_id);
        }
        let (yaw, pitch) = self.input.look();
        self.camera.yaw = yaw;
        self.camera.pitch = pitch;
        entities.retain(|e| e.id != local_id);

        self.feedback.update(dt);
        self.feedback.apply(&mut self.camera);
        self.particles.update(dt);
        self.hud.tick(dt);
        self.renderer.render(&entities, &self.camera);
    }

    /// Apply one authoritative server message.
    fn handle_server_msg(&mut self, msg: ServerMsg, now: u64) {
        match msg {
            ServerMsg::JoinAccept { entity, .. } => {
                // We are in. Adopt the assigned entity id and (re)build the predicted
                // world around it. A real build would also fetch the announced
                // content-addressed `map` here; we keep the test arena for now.
                tracing::info!("join accepted: local entity {entity}");
                self.local_id = entity;
                self.cw = make_client_world(entity, &self.map);
                self.joined = true;
            }
            ServerMsg::JoinReject { reason } => {
                tracing::warn!("join rejected: {reason}");
                self.joined = false;
            }
            ServerMsg::Snapshot(snap) => {
                // Keep the authoritative local state for the HUD before the snapshot
                // is consumed by the netcode.
                let local = snap.local.clone();
                let events = self.cw.apply_snapshot(snap, now);

                self.hud.apply_local(&local);
                self.hud.ingest_events(&events, &self.local_node);
                // Feel: interpret events from the local player's vantage (eye = pos +
                // eye offset) into camera shake / kick / flash.
                let view = LocalView {
                    id: local.entity,
                    eye: local.state.pos + glam::Vec3::Y * crate::EYE_OFFSET_M,
                    yaw: local.state.yaw,
                };
                for ev in &events {
                    self.particles.spawn_from_event(ev);
                    self.feedback.ingest(ev, &view);
                }
            }
            ServerMsg::Redirect { entity, .. } => {
                // Zone change / authority failover: re-home to the new entity id and
                // rebuild prediction. The input stream now targets the new authority
                // (the transport handles the topic switch).
                tracing::info!("redirected to new authority; local entity {entity}");
                self.local_id = entity;
                self.cw = make_client_world(entity, &self.map);
                self.joined = true;
            }
            ServerMsg::Pong {
                client_time_ms, ..
            } => {
                // Round-trip sample for clock/RTT estimation.
                self.cw.on_pong(client_time_ms, now);
            }
            ServerMsg::Karma(update) => {
                tracing::info!("karma update: {update:?}");
            }
            ServerMsg::Kick { reason } => {
                tracing::warn!("kicked: {reason}");
                self.joined = false;
            }
        }
    }

    /// Stage a content update. `ContentVersion` is *not* a [`ServerMsg`] — it rides
    /// the session control plane (mesh) out of band; the transport delivers it here
    /// once the referenced pack blob has been fetched and verified. The actual swap
    /// happens at the top of the next frame in [`App::frame`].
    pub fn on_content_version(&mut self, version: ContentVersion, pack: ContentPack) {
        match self.hotreload.stage(&mut self.registry, version.epoch, pack) {
            Ok(()) => tracing::info!(
                "staged content epoch {} ({})",
                version.epoch,
                version.label
            ),
            Err(e) => tracing::warn!("failed to stage content: {e}"),
        }
    }
}

/// Build the predicted client world for `local_id` over collision map `map`.
///
/// The replay sim's `rebuild` closure reconstructs a stripped single-player world
/// seeded at the authoritative state each reconcile (see [`SimReplay`]).
///
/// NOTE / known gap: `arena_sim::World` currently exposes only `spawn_player`
/// (which mints a fresh id) and no way to place a player at a *given* id/state. So
/// the rebuild below cannot yet guarantee the spawned id equals `local_id`, which
/// prediction needs. This is an `arena-sim` API gap — it should grow a
/// `seed_player(id, &EntityState)` (or `insert_entity`) so replay starts from exact
/// authoritative truth. The seam is wired; only that helper is missing.
fn make_client_world(local_id: EntityId, map: &arena_sim::MapDef) -> ClientWorld<ReplaySim> {
    let map_for_rebuild = map.clone();
    let rebuild: RebuildFn = Box::new(move |auth: &EntityState| {
        // The prediction sim needs the same content (movement tunables, spells) as
        // the authority. We build a fresh registry from the default pack here; a
        // follow-up should thread the *current* hot-reloaded pack/epoch in so the
        // predicted feel tracks live tweaks exactly. Prediction error self-corrects
        // via reconciliation regardless, so a stale pack only affects feel briefly.
        let content = ContentRegistry::new(1, arena_content::default_pack())
            .unwrap_or_else(|_| ContentRegistry::bootstrap());
        let mut world = arena_sim::World::new(map_for_rebuild.clone(), content);
        // TODO(arena-sim): replace with `world.seed_player(auth.id, auth)` so the
        // local entity exists at exactly `auth`. Until then we best-effort spawn.
        let _ = world.spawn_player(auth.owner.clone(), auth.team);
        world
    });
    let initial_content = ContentRegistry::new(1, arena_content::default_pack())
        .unwrap_or_else(|_| ContentRegistry::bootstrap());
    let initial = arena_sim::World::new(map.clone(), initial_content);
    ClientWorld::new(local_id, SimReplay::new(initial, rebuild))
}

// ===========================================================================
// Platform glue: window construction, the event-loop driver, the clock,
// and the network client.
// ===========================================================================

/// Build the window. On native a normal winit window; on wasm a window backed by a
/// `<canvas id="cerena-canvas">` appended to the document body.
fn build_window(event_loop: &EventLoop<()>) -> Arc<Window> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let window = winit::window::WindowBuilder::new()
            .with_title("Cerena")
            .build(event_loop)
            .expect("create window");
        Arc::new(window)
    }
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        use winit::platform::web::WindowBuilderExtWebSys;

        // Create (or reuse) a canvas in the DOM and hand it to winit.
        let doc = web_sys::window()
            .and_then(|w| w.document())
            .expect("no document");
        let canvas = doc
            .get_element_by_id("cerena-canvas")
            .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlCanvasElement>(e).ok())
            .unwrap_or_else(|| {
                let c = doc
                    .create_element("canvas")
                    .expect("create canvas")
                    .dyn_into::<web_sys::HtmlCanvasElement>()
                    .expect("canvas cast");
                c.set_id("cerena-canvas");
                c.set_width(1280);
                c.set_height(720);
                doc.body().expect("no body").append_child(&c).ok();
                c
            });

        let window = winit::window::WindowBuilder::new()
            .with_canvas(Some(canvas))
            .build(event_loop)
            .expect("create window from canvas");
        Arc::new(window)
    }
}

/// Drive the winit event loop. Native blocks on `run`; the browser hands the same
/// handler to `spawn` (which returns control to the JS runtime immediately).
fn run_event_loop(event_loop: EventLoop<()>, window: Arc<Window>, mut app: App) {
    let handler = move |event: Event<()>, elwt: &winit::event_loop::EventLoopWindowTarget<()>| {
        // Render continuously rather than only on OS-driven repaints.
        elwt.set_control_flow(winit::event_loop::ControlFlow::Poll);

        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => elwt.exit(),
                WindowEvent::Resized(size) => app.resize(size.width, size.height),
                WindowEvent::KeyboardInput {
                    event:
                        KeyEvent {
                            physical_key: PhysicalKey::Code(code),
                            state,
                            repeat,
                            ..
                        },
                    ..
                } => {
                    // Ignore auto-repeat: button intent is edge-driven, held state is
                    // tracked by the down/up pair.
                    if !repeat {
                        app.input.on_key(code, state == ElementState::Pressed);
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    app.input.on_mouse_button(button, state);
                    // First click captures the pointer so mouse-look engages.
                    if state == ElementState::Pressed {
                        app.input.pointer_locked = true;
                        #[cfg(target_arch = "wasm32")]
                        crate::input::request_pointer_lock();
                    }
                }
                WindowEvent::RedrawRequested => app.frame(),
                _ => {}
            },
            // Raw mouse motion (native, and wasm while pointer-locked) drives look.
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta: (dx, dy) },
                ..
            } => app.input.on_mouse_motion(dx as f32, dy as f32),
            // Nothing pending: ask for another frame, keeping the render loop alive.
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        }
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        event_loop.run(handler).expect("event loop run");
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn(handler);
    }
}

/// Construct the platform network client.
fn make_net_client() -> Net {
    #[cfg(target_arch = "wasm32")]
    {
        // Browsers reach the mesh through the relay's `/mesh-bridge` (see ce-net web
        // docs): the relay bridges these WebSocket frames onto the per-zone mesh
        // topics. The session/zone query string is filled in once matchmaking lands.
        match crate::net::WsNetClient::connect("wss://relay.ce-net.com/mesh-bridge") {
            Ok(c) => Box::new(c),
            Err(e) => {
                tracing::error!("mesh-bridge connect failed: {e:?}");
                // A dead transport: polls nothing, drops sends. The client still runs
                // (menus, settings) until a working connection is established.
                Box::new(DeadNet)
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Native loopback stub until the local-node mesh connector lands.
        Box::new(crate::net::StubNetClient::new())
    }
}

/// A do-nothing transport used on wasm when the socket fails to open, so the client
/// degrades to an offline state instead of panicking.
#[cfg(target_arch = "wasm32")]
struct DeadNet;

#[cfg(target_arch = "wasm32")]
impl NetClient for DeadNet {
    fn poll_messages(&mut self) -> Vec<ServerMsg> {
        Vec::new()
    }
    fn send(&mut self, _msg: ClientMsg) {}
}

/// Current local time in milliseconds. Browser performance clock on wasm, system
/// clock on native — both monotonic enough for frame deltas and ping RTT.
fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now() as u64)
            .unwrap_or(0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}
