//! wgpu bring-up: instance, surface, adapter, device, queue, surface config.
//!
//! This is the platform seam for graphics. On native the surface is created from
//! the winit window; on wasm it is created from the same window, whose backing
//! `<canvas>` lives in the DOM. We deliberately request **downlevel WebGL2 limits**
//! so the client runs on the WebGPU-less browsers too: WebGPU is used when present,
//! and the GL fallback path still satisfies the (smaller) limit set. Anything that
//! would exceed WebGL2 (huge bind groups, storage buffers in the vertex stage, ...)
//! is therefore off the table by construction, which keeps the renderer portable.

use std::sync::Arc;

use winit::window::Window;

/// Owns every long-lived wgpu handle plus the live surface configuration. One of
/// these exists for the lifetime of the client; [`Gpu::resize`] keeps the surface
/// matched to the window/canvas size.
pub struct Gpu {
    /// Kept alive because the `'static` surface borrows the window handle through
    /// the `Arc`; dropping it out from under the surface would be unsound.
    pub window: Arc<Window>,
    pub instance: wgpu::Instance,
    pub surface: wgpu::Surface<'static>,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// The current surface configuration (format, size, present mode).
    pub config: wgpu::SurfaceConfiguration,
    /// Current drawable size in physical pixels.
    pub size: (u32, u32),
}

impl Gpu {
    /// Bring up wgpu against `window`. Async because adapter and device requests
    /// await on every backend (and must, on the web).
    pub async fn new(window: Arc<Window>) -> Gpu {
        let phys = window.inner_size();
        // Guard against a zero-sized canvas at first paint (common on wasm before
        // layout settles); the surface must be configured with a non-zero extent.
        let size = (phys.width.max(1), phys.height.max(1));

        // On wasm we want the GL backend available as a fallback; on native we let
        // wgpu pick the platform-best (Vulkan/Metal/DX12). PRIMARY | GL covers both.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..Default::default()
        });

        // An `Arc<Window>` converts into a `'static` surface target, so the surface
        // owns its keep-alive and we avoid threading a window lifetime everywhere.
        let surface = instance
            .create_surface(window.clone())
            .expect("create wgpu surface from window/canvas");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no suitable GPU adapter found");

        // Pick limits that also fit WebGL2: start from the WebGL2 downlevel defaults
        // and raise each to whatever the adapter actually offers (so a real WebGPU
        // device is not artificially capped, while a GL device stays within reach).
        let required_limits =
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("cerena-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits,
                },
                None,
            )
            .await
            .expect("request device");

        // Prefer an sRGB surface format so colours written by the shader are gamma
        // correct without us hand-encoding; fall back to whatever the surface lists.
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.0,
            height: size.1,
            // Fifo (vsync) is the universally supported present mode and the right
            // default for the browser; a native build could expose Mailbox later.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Gpu {
            window,
            instance,
            surface,
            adapter,
            device,
            queue,
            config,
            size,
        }
    }

    /// React to a window/canvas resize: reconfigure the surface. A zero dimension
    /// (minimised window) is ignored — reconfiguring to 0 is invalid.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.size = (width, height);
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Aspect ratio (width / height) for the projection matrix.
    pub fn aspect(&self) -> f32 {
        self.size.0 as f32 / self.size.1.max(1) as f32
    }

    /// The surface colour format, needed when building render pipelines.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }
}
