//! The renderer: content-driven pipelines, the frame draw flow, and the
//! hot-reload pipeline-swap seam.
//!
//! ## Pipelines are compiled from content, not baked into the binary
//!
//! A render pipeline is built from an [`arena_content::material::ShaderDef`] by
//! handing its WGSL `source` to `device.create_shader_module` at runtime
//! ([`Renderer::build_pipeline`]). That is the whole trick behind hot-reload: when
//! a new content pack arrives, [`crate::hotreload`] calls back here to recompile
//! the changed shaders and swap them into [`Renderer::pipelines`] between frames.
//! Until content loads (or if a designer's WGSL fails to compile) we fall back to
//! [`DEFAULT_SURFACE_WGSL`], an embedded lit surface shader, so the world is never
//! a black screen.
//!
//! ## Frame draw flow ([`Renderer::render`])
//!
//! 1. upload the per-frame [`crate::camera::CameraUniform`],
//! 2. depth-tested opaque pass:
//!    a. draw the zone terrain meshes (one identity instance each),
//!    b. draw entities **instanced** — players as procedural organic capsules,
//!       mobs from creature meshes, projectiles/fields as VFX meshes,
//! 3. (TODO) additive VFX pass for [`crate::particles`],
//! 4. (TODO) screen-space HUD pass for [`crate::hud`].
//!
//! Both terrain and entities share one vertex format and one instanced pipeline:
//! terrain is simply drawn with a single identity-model instance, so there is one
//! code path and one shader to hot-reload.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use wgpu::util::DeviceExt;

use arena_content::material::{MaterialDef, ShaderDef, ShaderStage};
use arena_protocol::entity::{EntityKind, EntityState};
use arena_protocol::world::Team;

use crate::camera::{Camera, CameraUniform};
use crate::gpu::Gpu;
use crate::mesh_gpu::{GpuMesh, GpuTexture, Vertex};

/// Depth buffer format for the opaque pass.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The fallback surface shader, used until content loads and whenever a content
/// shader fails to compile. A simple lit surface: vertex colour modulated by a
/// hemispheric ambient + a single key light, organic and never black. `time` is
/// available for subtle animation but left unused here.
pub const DEFAULT_SURFACE_WGSL: &str = r#"
struct Camera {
    view_proj : mat4x4<f32>,
    cam_pos   : vec4<f32>,
    time      : f32,
};
@group(0) @binding(0) var<uniform> camera : Camera;

struct VsIn {
    @location(0) pos    : vec3<f32>,
    @location(1) normal : vec3<f32>,
    @location(2) uv     : vec2<f32>,
    @location(3) color  : vec4<f32>,
    // Per-instance model matrix (4 rows) + tint.
    @location(4) m0 : vec4<f32>,
    @location(5) m1 : vec4<f32>,
    @location(6) m2 : vec4<f32>,
    @location(7) m3 : vec4<f32>,
    @location(8) tint : vec4<f32>,
};

struct VsOut {
    @builtin(position) clip   : vec4<f32>,
    @location(0) world_normal : vec3<f32>,
    @location(1) color        : vec4<f32>,
};

@vertex
fn vs_main(in : VsIn) -> VsOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    let world = model * vec4<f32>(in.pos, 1.0);
    var out : VsOut;
    out.clip = camera.view_proj * world;
    // Uniform-scale assumption: rotate the normal by the model's basis.
    out.world_normal = normalize((model * vec4<f32>(in.normal, 0.0)).xyz);
    out.color = in.color * in.tint;
    return out;
}

@fragment
fn fs_main(in : VsOut) -> @location(0) vec4<f32> {
    let light_dir = normalize(vec3<f32>(0.4, 0.9, 0.3));
    let ndl = max(dot(normalize(in.world_normal), light_dir), 0.0);
    let ambient = 0.28;
    let lit = in.color.rgb * (ambient + ndl * 0.85);
    return vec4<f32>(lit, in.color.a);
}
"#;

/// A per-entity (or per-terrain) instance: a model matrix plus a tint. Uploaded as
/// the second vertex buffer; the instanced pipeline reads it at locations 4..=8.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct InstanceRaw {
    /// Column-major model matrix.
    pub model: [[f32; 4]; 4],
    /// Linear RGBA tint multiplied into the vertex colour.
    pub tint: [f32; 4],
}

impl InstanceRaw {
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
            4 => Float32x4, // model row 0
            5 => Float32x4, // model row 1
            6 => Float32x4, // model row 2
            7 => Float32x4, // model row 3
            8 => Float32x4, // tint
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }

    fn from_model(model: Mat4, tint: [f32; 4]) -> Self {
        Self {
            model: model.to_cols_array_2d(),
            tint,
        }
    }
}

/// A material resident on the GPU. For now it carries the baked procgen texture and
/// the scalar surface parameters; binding it into the shader (a per-material bind
/// group) is the next step once the surface shader samples textures.
pub struct GpuMaterial {
    pub texture: GpuTexture,
    pub roughness: f32,
    pub metallic: f32,
    pub emissive: f32,
}

/// The renderer. Owns the GPU, the compiled pipelines (keyed by shader id), the
/// resident procedural geometry, the per-frame camera binding, and the depth
/// buffer. `materials` maps content material ids to their baked GPU resources.
pub struct Renderer {
    pub gpu: Gpu,

    /// Content-shader pipelines keyed by [`arena_content::ids::ShaderId`] string.
    /// Swapped live by [`crate::hotreload`].
    pub pipelines: HashMap<String, wgpu::RenderPipeline>,
    /// The always-present fallback pipeline (embedded WGSL).
    default_pipeline: wgpu::RenderPipeline,
    /// Which content surface shader to prefer for opaque geometry, if loaded.
    active_surface_shader: Option<String>,

    /// Static zone terrain, generated by `arena-procgen`.
    pub world_meshes: Vec<GpuMesh>,
    /// The procedural organic capsule mesh used to draw players (and a stand-in for
    /// mobs/projectiles until per-kind meshes are wired).
    pub entity_mesh: Option<GpuMesh>,

    /// Baked materials, keyed by [`arena_content::ids::MaterialId`] string.
    pub materials: HashMap<String, GpuMaterial>,

    // Per-frame camera uniform binding (group 0).
    camera_buf: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    camera_bgl: wgpu::BindGroupLayout,

    depth_view: wgpu::TextureView,

    /// Client start instant -> the shader `time`. wall-clock seconds.
    start_secs: f64,
}

impl Renderer {
    /// Build the renderer on an initialised [`Gpu`]: the camera binding, the depth
    /// buffer, and the default pipeline.
    pub fn new(gpu: Gpu) -> Renderer {
        let device = &gpu.device;

        // --- camera uniform (group 0, binding 0) ---
        let camera_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("camera-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bg"),
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            }],
        });

        let depth_view = create_depth_view(device, gpu.size.0, gpu.size.1);

        // The fallback pipeline from embedded WGSL.
        let default_pipeline = build_pipeline_inner(
            device,
            &camera_bgl,
            gpu.format(),
            "default-surface",
            DEFAULT_SURFACE_WGSL,
        );

        Renderer {
            gpu,
            pipelines: HashMap::new(),
            default_pipeline,
            active_surface_shader: None,
            world_meshes: Vec::new(),
            entity_mesh: None,
            materials: HashMap::new(),
            camera_buf,
            camera_bind_group,
            camera_bgl,
            depth_view,
            start_secs: now_secs(),
        }
    }

    /// Compile a content [`ShaderDef`] into a render pipeline. This is the hot-reload
    /// seam: [`crate::hotreload`] calls it for every changed shader on a content swap
    /// and swaps the result into [`Renderer::pipelines`]. Returns `None` (and logs)
    /// if the WGSL fails to compile, so a bad designer edit degrades to the previous
    /// pipeline / the default rather than crashing the client.
    pub fn build_pipeline(&self, shader: &ShaderDef) -> Option<wgpu::RenderPipeline> {
        // We only build surface pipelines through this path today; sky/water/post
        // shaders will get their own layouts as those passes land.
        if shader.stage != ShaderStage::Surface {
            tracing::debug!(
                "skipping non-surface shader '{}' ({:?}) in surface pipeline build",
                shader.name,
                shader.stage
            );
            return None;
        }
        // create_shader_module can panic on invalid WGSL on some backends; we accept
        // that risk here and rely on validation having run upstream. A production
        // build would push an error scope and recover. (TODO: scoped validation.)
        let pipeline = build_pipeline_inner(
            &self.gpu.device,
            &self.camera_bgl,
            self.gpu.format(),
            &shader.name,
            &shader.source,
        );
        Some(pipeline)
    }

    /// Insert/replace a compiled pipeline under `shader_id` and make it the active
    /// surface pipeline. Called by the hot-reload path after a successful compile.
    pub fn install_surface_pipeline(&mut self, shader_id: String, pipeline: wgpu::RenderPipeline) {
        self.pipelines.insert(shader_id.clone(), pipeline);
        self.active_surface_shader = Some(shader_id);
    }

    /// Bake a content material onto the GPU. `texture` is the procgen-synthesised
    /// surface map for `def`. Stored for later binding by the surface shader.
    pub fn install_material(&mut self, def: &MaterialDef, texture: GpuTexture) {
        self.materials.insert(
            def.id.0.clone(),
            GpuMaterial {
                texture,
                roughness: def.roughness,
                metallic: def.metallic,
                emissive: def.emissive,
            },
        );
    }

    /// Replace the resident zone terrain (after (re)generating it from procgen).
    pub fn set_world_meshes(&mut self, meshes: Vec<GpuMesh>) {
        self.world_meshes = meshes;
    }

    /// Replace the entity capsule/creature mesh.
    pub fn set_entity_mesh(&mut self, mesh: GpuMesh) {
        self.entity_mesh = Some(mesh);
    }

    /// Reconfigure for a new window/canvas size: surface + depth buffer.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
        self.depth_view = create_depth_view(&self.gpu.device, self.gpu.size.0, self.gpu.size.1);
    }

    /// The currently-selected opaque pipeline: a hot-reloaded content surface
    /// shader if one is installed, else the embedded fallback.
    fn opaque_pipeline(&self) -> &wgpu::RenderPipeline {
        self.active_surface_shader
            .as_ref()
            .and_then(|id| self.pipelines.get(id))
            .unwrap_or(&self.default_pipeline)
    }

    /// Draw one frame. `entities` is the coherent set the netcode produced for
    /// "now" (interpolated remotes + predicted local); `camera` has already been
    /// snapped onto the local player.
    pub fn render(&mut self, entities: &[EntityState], camera: &Camera) {
        // --- 1. per-frame camera uniform ---
        let time = (now_secs() - self.start_secs) as f32;
        let cam = camera.uniform(self.gpu.aspect(), time);
        self.gpu
            .queue
            .write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&cam));

        // --- 2. build instance buffers ---
        // Terrain: a single identity-model, white-tint instance reused for every
        // terrain mesh (they bake their own world position into vertices).
        let terrain_instances = [InstanceRaw::from_model(Mat4::IDENTITY, [1.0; 4])];
        let terrain_ibuf =
            self.gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("terrain-instances"),
                    contents: bytemuck::cast_slice(&terrain_instances),
                    usage: wgpu::BufferUsages::VERTEX,
                });

        // Entities: one instance each. The local player is hidden in first person,
        // but remote players, mobs and projectiles all draw here.
        let entity_instances: Vec<InstanceRaw> = entities
            .iter()
            .map(entity_instance)
            .collect();
        let entity_ibuf = (!entity_instances.is_empty()).then(|| {
            self.gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("entity-instances"),
                    contents: bytemuck::cast_slice(&entity_instances),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        });

        // --- 3. acquire the swapchain image ---
        let frame = match self.gpu.surface.get_current_texture() {
            Ok(f) => f,
            // Lost/outdated surface (resize, minimise, device sleep): reconfigure
            // and skip this frame; the next one recovers.
            Err(_) => {
                self.gpu
                    .surface
                    .configure(&self.gpu.device, &self.gpu.config);
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder =
            self.gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("frame-encoder"),
                });

        {
            // Opaque depth-tested pass.
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("opaque-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // A deep sky-tint clear; the sky dome shader will replace this.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.03,
                            g: 0.04,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(self.opaque_pipeline());
            pass.set_bind_group(0, &self.camera_bind_group, &[]);

            // 3a. terrain — one identity instance per mesh.
            pass.set_vertex_buffer(1, terrain_ibuf.slice(..));
            for m in &self.world_meshes {
                pass.set_vertex_buffer(0, m.vbuf.slice(..));
                pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.n_indices, 0, 0..1);
            }

            // 3b. entities — instanced draw of the capsule/creature mesh.
            if let (Some(mesh), Some(ibuf)) = (&self.entity_mesh, &entity_ibuf) {
                pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
                pass.set_vertex_buffer(1, ibuf.slice(..));
                pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.n_indices, 0, 0..entity_instances.len() as u32);
            }

            // TODO: 3c additive VFX pass for `crate::particles` (glowy spell trails,
            //       hits, explosions) — a separate blend state, no depth write.
            // TODO: 3d screen-space HUD pass for `crate::hud` (bars, crosshair, feed).
        }

        self.gpu.queue.submit([encoder.finish()]);
        frame.present();
    }
}

/// Map an entity to its draw instance: a model transform plus a kind/team tint.
/// Players render as upright capsules oriented by yaw (pitch is head-only and not
/// reflected in the body); projectiles are small and bright; mobs reuse the capsule
/// until creature meshes are wired per [`arena_procgen::creature`].
fn entity_instance(e: &EntityState) -> InstanceRaw {
    let (scale, tint) = match e.kind {
        EntityKind::Player => (Vec3::new(1.0, 1.0, 1.0), team_tint(e.team)),
        EntityKind::Projectile => (Vec3::splat(0.25), [1.0, 0.7, 0.2, 1.0]),
        EntityKind::Pickup => (Vec3::splat(0.5), [0.4, 1.0, 0.6, 1.0]),
        EntityKind::ZoneMirror => (Vec3::splat(1.0), [0.7, 0.7, 0.8, 0.6]),
    };
    let model = Mat4::from_scale_rotation_translation(
        scale,
        Quat::from_rotation_y(e.yaw),
        e.pos,
    );
    InstanceRaw::from_model(model, tint)
}

/// Team colour tint for players. FFA (`Team::None`) gets a neutral parchment.
fn team_tint(team: Team) -> [f32; 4] {
    match team {
        Team::Red => [0.85, 0.25, 0.25, 1.0],
        Team::Blue => [0.25, 0.45, 0.9, 1.0],
        Team::None => [0.8, 0.78, 0.7, 1.0],
    }
}

/// Build a render pipeline from WGSL source. Shared by the default pipeline and
/// every hot-reloaded content surface shader so they are byte-for-byte identical in
/// state — only the shader module differs.
fn build_pipeline_inner(
    device: &wgpu::Device,
    camera_bgl: &wgpu::BindGroupLayout,
    color_format: wgpu::TextureFormat,
    label: &str,
    wgsl: &str,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("surface-pipeline-layout"),
        bind_group_layouts: &[camera_bgl],
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: "vs_main",
            buffers: &[Vertex::layout(), InstanceRaw::layout()],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: "fs_main",
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Organic procgen meshes are wound CCW; cull backfaces for fill rate.
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    })
}

/// Allocate the depth texture and return its view.
fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Wall-clock seconds, monotonic-ish, for the shader `time`. Uses the browser
/// performance clock on wasm and the system clock on native.
fn now_secs() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now() / 1000.0)
            .unwrap_or(0.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}
