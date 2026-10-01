//! The host's shared 3D renderer: one [`renderbud`] PBR renderer on the
//! window's wgpu device, for apps that draw glTF models inside their UI.
//!
//! [`Notedeck`](crate::Notedeck) owns it when the window has a wgpu backend
//! and hands it to every frame as
//! [`AppContext::renderer3d`](crate::AppContext::renderer3d); headless and
//! glow hosts have none, and an app falls back to not drawing the model.
//!
//! The renderbud renderer itself (pipelines, the HDR environment it lights
//! with, which it convolves on the CPU) is built on the first
//! [`Renderer3d::scene`] call, not at startup, so a session that never shows a
//! model never pays for it. That call is slow, so make it from a worker: the
//! handle is `Send`, and [`Renderer3d::model_uploader`] is the usual way in.

use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui_wgpu::{self, wgpu};

/// Size the scene renderer is created at. It draws nothing at this size —
/// apps render models through their own offscreen views — so it only needs
/// to be valid.
const SCENE_SIZE: (u32, u32) = (256, 256);

/// The window's wgpu device plus a lazily built renderbud renderer on it.
///
/// Cheap to clone (the `Arc`s inside it and [`egui_wgpu::RenderState`]), so
/// an owner of GPU resources made through it can keep a handle to free them
/// on drop, and a worker can take one to load models.
#[derive(Clone)]
pub struct Renderer3d {
    render_state: egui_wgpu::RenderState,
    scene: Arc<OnceLock<renderbud::egui::EguiRenderer>>,
}

impl Renderer3d {
    /// A renderer on `render_state`'s device. Builds no GPU state yet.
    pub fn new(render_state: egui_wgpu::RenderState) -> Self {
        Self {
            render_state,
            scene: Arc::new(OnceLock::new()),
        }
    }

    /// The device every model, texture and view is created on.
    pub fn device(&self) -> &wgpu::Device {
        &self.render_state.device
    }

    /// The queue renders and uploads are submitted on.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.render_state.queue
    }

    /// The shared renderbud renderer, built on first use. The build is slow
    /// (it prefilters the lighting environment on the CPU) and blocks any
    /// other caller until it's done, so let a worker make the first call;
    /// once built this is a load.
    ///
    /// Deliberately not registered in egui's paint-callback resources (as
    /// [`EguiRenderer::new`](renderbud::egui::EguiRenderer::new) would): those
    /// are keyed by type, so it would fight any app that paints its own
    /// renderbud scene with [`SceneRender`](renderbud::egui::SceneRender).
    /// Apps draw through it offscreen instead.
    ///
    /// The `Mutex` is renderbud's. Hold it briefly: the UI thread locks it to
    /// render views, so a worker that holds it stalls a frame.
    pub fn scene(&self) -> &renderbud::egui::EguiRenderer {
        self.scene.get_or_init(|| {
            let rs = &self.render_state;
            let renderer =
                renderbud::Renderer::new(&rs.device, &rs.queue, rs.target_format, SCENE_SIZE);
            renderbud::egui::EguiRenderer {
                renderer: Arc::new(Mutex::new(renderer)),
            }
        })
    }

    /// A [`ModelUploader`](renderbud::ModelUploader) for the shared renderer,
    /// to parse and upload models on a worker without holding its lock.
    /// Builds the renderer if nothing has yet, so call it off the UI thread.
    pub fn model_uploader(&self) -> renderbud::ModelUploader {
        let rs = &self.render_state;
        let scene = self
            .scene()
            .renderer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scene.model_uploader(&rs.device, &rs.queue)
    }

    /// Register `view` with egui so a shape can paint it like any texture.
    /// Free it with [`free_texture`](Self::free_texture) when done.
    pub fn register_native_texture(&self, view: &wgpu::TextureView) -> egui::TextureId {
        self.render_state.renderer.write().register_native_texture(
            &self.render_state.device,
            view,
            wgpu::FilterMode::Linear,
        )
    }

    /// Release a texture from [`register_native_texture`](Self::register_native_texture).
    pub fn free_texture(&self, id: egui::TextureId) {
        self.render_state.renderer.write().free_texture(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker can take the handle (to build the renderer and upload models
    /// off the UI thread).
    #[test]
    fn renderer3d_crosses_threads() {
        fn assert_send<T: Send + Sync>() {}
        assert_send::<Renderer3d>();
    }
}
