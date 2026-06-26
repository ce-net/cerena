//! GPU residency for procedural geometry and textures.
//!
//! `arena-procgen` produces CPU-side, platform-independent [`arena_procgen::mesh::Mesh`]
//! and [`arena_procgen::texture::TextureData`] (organic surfaces grown from the same
//! content definitions the server uses). This module is the one place those become
//! wgpu resources: interleaved vertex/index buffers and sampled textures. Nothing
//! here reads a file — every byte originates from procgen, so the look of the world
//! is fully reproducible from content + seed.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use arena_procgen::mesh::Mesh;
use arena_procgen::texture::TextureData;

/// One interleaved vertex. `repr(C)` + `Pod` lets us upload the slice straight to a
/// buffer. Layout matches the `@location` bindings in the WGSL surface shader.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    /// Object-space position.
    pub pos: [f32; 3],
    /// Object-space normal (for lighting / triplanar blending).
    pub normal: [f32; 3],
    /// Texture / triplanar coordinate.
    pub uv: [f32; 2],
    /// Per-vertex linear RGBA tint (procgen bakes organic colour variation here).
    pub color: [f32; 4],
}

impl Vertex {
    /// The vertex buffer layout matching the shader's `@location` slots:
    /// 0 = position, 1 = normal, 2 = uv, 3 = color.
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
            0 => Float32x3, // pos
            1 => Float32x3, // normal
            2 => Float32x2, // uv
            3 => Float32x4, // color
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }
    }
}

/// A mesh resident on the GPU: its vertex and index buffers plus the draw count.
pub struct GpuMesh {
    pub vbuf: wgpu::Buffer,
    pub ibuf: wgpu::Buffer,
    pub n_indices: u32,
}

impl GpuMesh {
    /// Interleave a procgen [`Mesh`]'s parallel attribute arrays into [`Vertex`]es
    /// and upload them, with the index buffer, to the GPU.
    ///
    /// procgen meshes store attributes as parallel `Vec`s (positions, normals, uvs,
    /// colors all indexed by vertex). Any attribute array shorter than the position
    /// array is padded with a sensible default (up normal, zero uv, white) so a mesh
    /// that omits, say, vertex colours still renders.
    pub fn upload(device: &wgpu::Device, mesh: &Mesh) -> GpuMesh {
        let n = mesh.positions.len();
        let mut verts = Vec::with_capacity(n);
        for i in 0..n {
            verts.push(Vertex {
                pos: mesh.positions[i],
                normal: mesh.normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]),
                uv: mesh.uvs.get(i).copied().unwrap_or([0.0, 0.0]),
                color: mesh.colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0, 1.0]),
            });
        }

        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("procgen-vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("procgen-ibuf"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        GpuMesh {
            vbuf,
            ibuf,
            n_indices: mesh.indices.len() as u32,
        }
    }
}

/// A sampled texture on the GPU: the texture, a default view, and a sampler.
pub struct GpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

/// Upload a procgen [`TextureData`] (tightly-packed RGBA8) as a sampled 2D texture.
/// We use the sRGB variant so the baked colours read correctly through the sRGB
/// surface, and a repeat+linear sampler suited to organic, tiling triplanar maps.
pub fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    data: &TextureData,
) -> GpuTexture {
    let size = wgpu::Extent3d {
        width: data.width,
        height: data.height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("procgen-texture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data.rgba,
        wgpu::ImageDataLayout {
            offset: 0,
            // Tightly packed: 4 bytes/pixel, no row padding from procgen.
            bytes_per_row: Some(4 * data.width),
            rows_per_image: Some(data.height),
        },
        size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("procgen-sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    GpuTexture {
        texture,
        view,
        sampler,
    }
}
