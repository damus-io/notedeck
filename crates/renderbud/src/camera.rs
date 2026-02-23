use glam::{Mat4, Vec3};

#[derive(Debug, Copy, Clone)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,

    pub fov_y: f32,
    pub znear: f32,
    pub zfar: f32,
}

/// Arcball camera controller for orbital navigation around a target point.
#[derive(Debug, Clone)]
pub struct ArcballController {
    pub target: Vec3,
    pub distance: f32,
    pub yaw: f32,   // radians, around Y axis
    pub pitch: f32, // radians, up/down
    pub sensitivity: f32,
    pub zoom_sensitivity: f32,
    pub min_distance: f32,
    pub max_distance: f32,
}

impl Default for ArcballController {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 5.0,
            yaw: 0.0,
            pitch: 0.3,
            sensitivity: 0.005,
            zoom_sensitivity: 0.1,
            min_distance: 0.1,
            max_distance: 1000.0,
        }
    }
}

impl ArcballController {
    /// Initialize from an existing camera.
    pub fn from_camera(camera: &Camera) -> Self {
        let offset = camera.eye - camera.target;
        let distance = offset.length();

        // Compute yaw (rotation around Y) and pitch (elevation)
        let yaw = offset.x.atan2(offset.z);
        let pitch = (offset.y / distance).asin();

        Self {
            target: camera.target,
            distance,
            yaw,
            pitch,
            ..Default::default()
        }
    }

    /// Handle mouse drag delta (in pixels).
    pub fn on_drag(&mut self, delta_x: f32, delta_y: f32) {
        self.yaw -= delta_x * self.sensitivity;
        self.pitch += delta_y * self.sensitivity;

        // Clamp pitch to avoid gimbal lock
        let limit = std::f32::consts::FRAC_PI_2 - 0.01;
        self.pitch = self.pitch.clamp(-limit, limit);
    }

    /// Handle scroll for zoom (positive = zoom in).
    pub fn on_scroll(&mut self, delta: f32) {
        self.distance *= 1.0 - delta * self.zoom_sensitivity;
        self.distance = self.distance.clamp(self.min_distance, self.max_distance);
    }

    /// Compute the camera eye position from current orbit state.
    pub fn eye(&self) -> Vec3 {
        let x = self.distance * self.pitch.cos() * self.yaw.sin();
        let y = self.distance * self.pitch.sin();
        let z = self.distance * self.pitch.cos() * self.yaw.cos();
        self.target + Vec3::new(x, y, z)
    }

    /// Update a camera with the current arcball state.
    pub fn update_camera(&self, camera: &mut Camera) {
        camera.eye = self.eye();
        camera.target = self.target;
    }
}

/// FPS-style fly camera controller for free movement through the scene.
#[derive(Debug, Clone)]
pub struct FlyController {
    pub position: Vec3,
    pub yaw: f32,   // radians, around Y axis
    pub pitch: f32, // radians, up/down
    pub speed: f32,
    pub sensitivity: f32,
}

impl Default for FlyController {
    fn default() -> Self {
        Self {
            position: Vec3::new(0.0, 2.0, 5.0),
            yaw: 0.0,
            pitch: 0.0,
            speed: 5.0,
            sensitivity: 0.003,
        }
    }
}

impl FlyController {
    /// Initialize from an existing camera.
    pub fn from_camera(camera: &Camera) -> Self {
        let dir = (camera.target - camera.eye).normalize();
        let yaw = dir.x.atan2(dir.z);
        let pitch = dir.y.asin();

        Self {
            position: camera.eye,
            yaw,
            pitch,
            ..Default::default()
        }
    }

    /// Handle mouse movement for looking around.
    pub fn on_mouse_look(&mut self, delta_x: f32, delta_y: f32) {
        self.yaw -= delta_x * self.sensitivity;
        self.pitch -= delta_y * self.sensitivity;

        let limit = std::f32::consts::FRAC_PI_2 - 0.01;
        self.pitch = self.pitch.clamp(-limit, limit);
    }

    /// Forward direction (horizontal plane + pitch).
    pub fn forward(&self) -> Vec3 {
        Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        )
        .normalize()
    }

    /// Right direction (always horizontal).
    pub fn right(&self) -> Vec3 {
        Vec3::new(self.yaw.cos(), 0.0, -self.yaw.sin()).normalize()
    }

    /// Move the camera. forward/right/up are signed: positive = forward/right/up.
    pub fn process_movement(&mut self, forward: f32, right: f32, up: f32, dt: f32) {
        let velocity = self.speed * dt;
        self.position += self.forward() * forward * velocity;
        self.position += self.right() * right * velocity;
        self.position += Vec3::Y * up * velocity;
    }

    /// Adjust speed with scroll wheel.
    pub fn on_scroll(&mut self, delta: f32) {
        self.speed *= 1.0 + delta * 0.1;
        self.speed = self.speed.clamp(0.5, 100.0);
    }

    /// Update a camera with the current fly state.
    pub fn update_camera(&self, camera: &mut Camera) {
        camera.eye = self.position;
        camera.target = self.position + self.forward();
    }
}

/// Third-person camera controller that orbits around a movable avatar.
///
/// WASD moves the avatar on the ground plane (camera-relative).
/// Mouse drag orbits the camera around the avatar.
/// Scroll zooms in/out.
#[derive(Debug, Clone)]
pub struct ThirdPersonController {
    /// Avatar world position (Y stays at ground level)
    pub avatar_position: Vec3,
    /// Avatar facing direction in radians (around Y axis)
    pub avatar_yaw: f32,
    /// Height offset for the camera look-at target above avatar_position
    pub avatar_eye_height: f32,

    /// Camera orbit distance from avatar
    pub distance: f32,
    /// Camera orbit yaw (horizontal angle around avatar)
    pub yaw: f32,
    /// Camera orbit pitch (vertical angle, positive = looking down)
    pub pitch: f32,

    /// Avatar movement speed (units per second)
    pub speed: f32,
    /// Mouse orbit sensitivity
    pub sensitivity: f32,
    /// Scroll zoom sensitivity
    pub zoom_sensitivity: f32,
    /// Minimum orbit distance
    pub min_distance: f32,
    /// Maximum orbit distance
    pub max_distance: f32,
}

impl Default for ThirdPersonController {
    fn default() -> Self {
        Self {
            avatar_position: Vec3::ZERO,
            avatar_yaw: 0.0,
            avatar_eye_height: 1.5,
            distance: 8.0,
            yaw: 0.0,
            pitch: 0.4,
            speed: 5.0,
            sensitivity: 0.005,
            zoom_sensitivity: 0.1,
            min_distance: 2.0,
            max_distance: 30.0,
        }
    }
}

impl ThirdPersonController {
    /// Initialize from an existing camera, inferring orbit parameters.
    pub fn from_camera(camera: &Camera) -> Self {
        let offset = camera.eye - camera.target;
        let distance = offset.length().max(2.0);
        let yaw = offset.x.atan2(offset.z);
        let pitch = (offset.y / distance).asin().max(0.05);

        Self {
            avatar_position: Vec3::new(camera.target.x, 0.0, camera.target.z),
            avatar_eye_height: camera.target.y.max(1.0),
            distance,
            yaw,
            pitch,
            ..Default::default()
        }
    }

    /// Handle mouse drag to orbit camera around avatar.
    pub fn on_mouse_look(&mut self, delta_x: f32, delta_y: f32) {
        self.yaw -= delta_x * self.sensitivity;
        self.pitch += delta_y * self.sensitivity;

        let limit = std::f32::consts::FRAC_PI_2 - 0.05;
        self.pitch = self.pitch.clamp(0.05, limit);
    }

    /// Handle scroll to zoom in/out.
    pub fn on_scroll(&mut self, delta: f32) {
        self.distance *= 1.0 - delta * self.zoom_sensitivity;
        self.distance = self.distance.clamp(self.min_distance, self.max_distance);
    }

    /// Camera forward direction projected onto the ground plane.
    fn camera_forward_flat(&self) -> Vec3 {
        Vec3::new(self.yaw.sin(), 0.0, self.yaw.cos()).normalize()
    }

    /// Camera right direction (always horizontal).
    fn camera_right(&self) -> Vec3 {
        Vec3::new(self.yaw.cos(), 0.0, -self.yaw.sin()).normalize()
    }

    /// Move avatar on the ground plane (camera-relative WASD).
    /// `_up` is ignored -- avatar stays on the ground.
    pub fn process_movement(&mut self, forward: f32, right: f32, _up: f32, dt: f32) {
        let velocity = self.speed * dt;
        let move_dir = self.camera_forward_flat() * forward + self.camera_right() * right;

        if move_dir.length_squared() > 0.001 {
            let move_dir = move_dir.normalize();
            self.avatar_position += move_dir * velocity;
            self.avatar_yaw = move_dir.x.atan2(move_dir.z);
        }
    }

    /// Camera look-at target (avatar position + eye height offset).
    pub fn target(&self) -> Vec3 {
        self.avatar_position + Vec3::new(0.0, self.avatar_eye_height, 0.0)
    }

    /// Compute camera eye position from orbit state.
    pub fn eye(&self) -> Vec3 {
        let target = self.target();
        let x = self.distance * self.pitch.cos() * self.yaw.sin();
        let y = self.distance * self.pitch.sin();
        let z = self.distance * self.pitch.cos() * self.yaw.cos();
        target + Vec3::new(x, y, z)
    }

    /// Update a Camera struct from current orbit + avatar state.
    pub fn update_camera(&self, camera: &mut Camera) {
        camera.eye = self.eye();
        camera.target = self.target();
    }
}

impl Camera {
    pub fn new(eye: Vec3, target: Vec3) -> Self {
        Self {
            eye,
            target,
            up: Vec3::Y,
            fov_y: 45_f32.to_radians(),
            znear: 0.1,
            zfar: 1000.0,
        }
    }

    fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye, self.target, self.up)
    }

    fn proj(&self, width: f32, height: f32) -> Mat4 {
        let aspect = width / height.max(1.0);
        Mat4::perspective_rh(self.fov_y, aspect, self.znear, self.zfar)
    }

    pub fn view_proj(&self, width: f32, height: f32) -> Mat4 {
        self.proj(width, height) * self.view()
    }

    pub fn fit_to_aabb(
        bounds_min: Vec3,
        bounds_max: Vec3,
        aspect: f32,
        fov_y: f32,
        padding: f32,
    ) -> Self {
        let center = (bounds_min + bounds_max) * 0.5;
        let radius = ((bounds_max - bounds_min) * 0.5).length().max(1e-4);

        // horizontal fov derived from vertical fov + aspect
        let half_fov_y = fov_y * 0.5;
        let half_fov_x = (half_fov_y.tan() * aspect).atan();

        // fit in both directions
        let limiting_half_fov = half_fov_y.min(half_fov_x);
        let dist = (radius / limiting_half_fov.tan()) * padding;

        // choose a viewing direction
        let view_dir = Vec3::new(0.0, 0.35, 1.0).normalize();
        let eye = center + view_dir * dist;

        // near/far based on distance + radius
        let znear = (dist - radius * 2.0).max(0.01);
        let zfar = dist + radius * 50.0;

        Self {
            eye,
            target: center,
            up: Vec3::Y,
            fov_y,
            znear,
            zfar,
        }
    }
}
