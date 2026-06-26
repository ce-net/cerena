//! First-person camera and its GPU uniform.
//!
//! The camera is a position plus a yaw/pitch look direction — the exact same
//! representation the netcode stamps on an [`arena_protocol::input::InputFrame`],
//! so the view the player sees and the intent the server receives never disagree.
//! Each frame the camera is snapped onto the *predicted* local player entity (see
//! [`Camera::follow`]) so movement feels instant, and its matrices are packed into
//! a [`CameraUniform`] uploaded to the renderer's per-frame bind group.
//!
//! Convention: right-handed, y-up, -Z forward at yaw 0 (matching
//! [`arena_protocol::entity::EntityState::view_dir`]).

use glam::{Mat4, Vec3};

use arena_protocol::entity::EntityState;

/// A first-person camera.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Eye position in world space.
    pub pos: Vec3,
    /// Horizontal look angle, radians (wrapped to [-pi, pi] by the input layer).
    pub yaw: f32,
    /// Vertical look angle, radians (clamped to ~[-pi/2, pi/2]).
    pub pitch: f32,
    /// Vertical field of view, radians.
    pub fov: f32,
    /// Near/far planes, metres. Far is generous — Cerena zones are 128 m and the
    /// AOI spans neighbours, so the visible world can be a few hundred metres deep.
    pub znear: f32,
    pub zfar: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            fov: crate::DEFAULT_FOV_Y,
            znear: 0.05,
            zfar: 1000.0,
        }
    }
}

impl Camera {
    /// The unit forward (look) direction from yaw/pitch. Mirrors
    /// [`EntityState::view_dir`] so the camera ray equals the server's fire ray.
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(sy * cp, sp, -cy * cp).normalize_or_zero()
    }

    /// Snap the camera onto the predicted local player for this frame. The look
    /// angles come from the player's own input-driven state, and the eye sits a
    /// little above the capsule centre. Called every render frame with the entity
    /// the predictor produced for "now".
    pub fn follow(&mut self, local: &EntityState) {
        self.pos = local.pos + Vec3::Y * crate::EYE_OFFSET_M;
        self.yaw = local.yaw;
        self.pitch = local.pitch;
    }

    /// Right-handed view matrix (world -> view).
    pub fn view(&self) -> Mat4 {
        Mat4::look_to_rh(self.pos, self.forward(), Vec3::Y)
    }

    /// Right-handed perspective projection (view -> clip) for the given aspect.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov, aspect.max(0.0001), self.znear, self.zfar)
    }

    /// Combined view-projection.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }

    /// Pack into the GPU uniform. `time` drives shader animation (waving foliage,
    /// pulsing emissive, water) and is the wall-clock seconds since client start.
    pub fn uniform(&self, aspect: f32, time: f32) -> CameraUniform {
        CameraUniform {
            view_proj: self.view_proj(aspect).to_cols_array_2d(),
            cam_pos: [self.pos.x, self.pos.y, self.pos.z, 1.0],
            time,
            _pad: [0.0; 3],
        }
    }
}

/// The per-frame camera uniform, laid out for std140-friendly upload. `repr(C)`
/// plus explicit padding keeps it `Pod` and matches the WGSL `Camera` struct.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    /// Column-major view-projection matrix.
    pub view_proj: [[f32; 4]; 4],
    /// Eye position (xyz) + 1.0; used for view-dependent shading (specular, fresnel).
    pub cam_pos: [f32; 4],
    /// Seconds since start, for procedural animation in shaders.
    pub time: f32,
    /// Pads the struct to a 16-byte boundary so the uniform block is well-formed.
    pub _pad: [f32; 3],
}
