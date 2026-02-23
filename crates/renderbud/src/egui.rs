use std::sync::Arc;
use std::sync::Mutex;

use crate::Renderer;

#[derive(Clone)]
pub struct EguiRenderer {
    pub renderer: Arc<Mutex<Renderer>>,
}

/// Marker type for the egui paint callback that renders the full scene.
#[derive(Copy, Clone)]
pub struct SceneRender;

#[cfg(feature = "egui")]
impl EguiRenderer {
    pub fn new(rs: &egui_wgpu::RenderState, size: (u32, u32)) -> Self {
        let renderer = Renderer::new(&rs.device, &rs.queue, rs.target_format, size);
        let egui_renderer = Self {
            renderer: Arc::new(Mutex::new(renderer)),
        };

        rs.renderer
            .write()
            .callback_resources
            .insert(egui_renderer.clone());

        egui_renderer
    }
}

#[cfg(feature = "egui")]
impl egui_wgpu::CallbackTrait for SceneRender {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let egui_renderer: &EguiRenderer = resources.get().unwrap();

        let mut renderer = egui_renderer.renderer.lock().unwrap();

        renderer.resize(device);
        renderer.update();
        renderer.prepare(queue);

        // Render shadow depth pass into a separate command buffer
        // that executes before the main egui render pass.
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        renderer.render_shadow(&mut encoder);

        vec![encoder.finish()]
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'_>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let egui_renderer: &EguiRenderer = resources.get().unwrap();

        egui_renderer
            .renderer
            .lock()
            .unwrap()
            .render_pass(render_pass)
    }
}
