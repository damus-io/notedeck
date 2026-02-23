use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

struct Renderbud {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: renderbud::Renderer,
}

impl Renderbud {
    async fn new(window: Window) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window).unwrap();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .unwrap();

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .unwrap();

        let surface_caps = surface.get_capabilities(&adapter);
        let format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: surface_caps.present_modes[0],
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        let renderer =
            renderbud::Renderer::new(&device, &queue, format, (config.width, config.height));

        Self {
            config,
            surface,
            queue,
            device,
            renderer,
        }
    }

    fn update(&mut self) {
        self.renderer.update();
    }

    fn prepare(&self) {
        self.renderer.prepare(&self.queue);
    }

    fn resize(&mut self, new_size: (u32, u32)) {
        let width = new_size.0.max(1);
        let height = new_size.1.max(1);

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);

        self.renderer.set_target_size((width, height));
        self.renderer.resize(&self.device)
    }

    fn size(&self) -> (u32, u32) {
        self.renderer.size()
    }

    fn on_mouse_drag(&mut self, delta_x: f32, delta_y: f32) {
        self.renderer.on_mouse_drag(delta_x, delta_y);
    }

    fn on_scroll(&mut self, delta: f32) {
        self.renderer.on_scroll(delta);
    }

    fn load_gltf_model(
        &mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<renderbud::Model, gltf::Error> {
        self.renderer
            .load_gltf_model(&self.device, &self.queue, path)
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        self.renderer.render(&view, &mut encoder);
        self.queue.submit(Some(encoder.finish()));
        frame.present();

        Ok(())
    }
}

struct App {
    renderbud: Option<Renderbud>,
    mouse_pressed: bool,
    last_mouse_pos: Option<(f64, f64)>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            renderbud: None,
            mouse_pressed: false,
            last_mouse_pos: None,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        // Create the window *after* the event loop is running (winit 0.30+).
        let window: Window = el
            .create_window(WindowAttributes::default())
            .expect("create_window failed");

        let mut renderbud = pollster::block_on(Renderbud::new(window));

        // pick a path relative to crate root
        //let model_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/assets/ironwood.glb");
        let model_path = std::path::Path::new("/home/jb55/var/models/WaterBottle.glb");
        //let model_path = std::path::Path::new("/home/jb55/dev/github/KhronosGroup/glTF-Sample-Assets/Models/FlightHelmet/glTF/FlightHelmet.gltf");
        //let model_path = std::path::Path::new("/home/jb55/var/models/acnh-scuba.glb");
        //let model_path = std::path::Path::new("/home/jb55/var/models/ABeautifulGame.glb");
        renderbud.load_gltf_model(model_path).unwrap();

        self.renderbud = Some(renderbud);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(renderbud) = self.renderbud.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => el.exit(),

            WindowEvent::Resized(sz) => renderbud.resize((sz.width, sz.height)),

            WindowEvent::KeyboardInput { event, .. } => {
                if let KeyEvent {
                    physical_key: PhysicalKey::Code(code),
                    state: ElementState::Pressed,
                    ..
                } = event
                {
                    match code {
                        KeyCode::Space => {
                            // do something
                        }
                        _ => {}
                    }
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    self.mouse_pressed = state == ElementState::Pressed;
                    if !self.mouse_pressed {
                        self.last_mouse_pos = None;
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let pos = (position.x, position.y);
                if self.mouse_pressed {
                    if let Some(last) = self.last_mouse_pos {
                        let dx = (pos.0 - last.0) as f32;
                        let dy = (pos.1 - last.1) as f32;
                        renderbud.on_mouse_drag(dx, dy);
                    }
                }
                self.last_mouse_pos = Some(pos);
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32 * 0.01,
                };
                renderbud.on_scroll(scroll);
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(renderbud) = self.renderbud.as_mut() else {
            return;
        };

        // Continuous rendering.
        renderbud.update();
        renderbud.prepare();

        match renderbud.render() {
            Ok(_) => {}
            Err(wgpu::SurfaceError::Lost) => renderbud.resize(renderbud.size()),
            Err(wgpu::SurfaceError::OutOfMemory) => el.exit(),
            Err(_) => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;

    // Equivalent to your `elwt.set_control_flow(ControlFlow::Poll);`
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
