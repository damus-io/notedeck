// Based on https://github.com/Smithay/client-toolkit/blob/master/examples/themed_window.rs.

use std::sync::Arc;
use std::time::Duration;
use std::{convert::TryInto, num::NonZeroU32};

use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, Proxy, QueueHandle,
};
use smithay_client_toolkit::reexports::csd_frame::{
    CursorIcon, DecorationsFrame, FrameAction, FrameClick, ResizeEdge,
};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge as XdgResizeEdge;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm, delegate_subcompositor, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{
            PointerData, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            window::{DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell, XdgSurface,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
    subcompositor::SubcompositorState,
};

use sctk_adwaita::{AdwaitaFrame, FrameConfig};

fn main() {
    let conn = Connection::connect_to_env().unwrap();

    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();
    let registry_state = RegistryState::new(&globals);
    let seat_state = SeatState::new(&globals, &qh);
    let output_state = OutputState::new(&globals, &qh);
    let compositor_state =
        CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let subcompositor_state =
        SubcompositorState::bind(compositor_state.wl_compositor().clone(), &globals, &qh)
            .expect("wl_subcompositor not available");
    let shm_state = Shm::bind(&globals, &qh).expect("wl_shm not available");
    let xdg_shell_state = XdgShell::bind(&globals, &qh).expect("xdg shell not available");

    let width = 256;
    let height = 256;
    let pool = SlotPool::new(width as usize * height as usize * 4, &shm_state)
        .expect("Failed to create pool");

    let window_surface = compositor_state.create_surface(&qh);

    let window =
        xdg_shell_state.create_window(window_surface, WindowDecorations::ServerDefault, &qh);
    window.set_title("A wayland window");
    // GitHub does not let projects use the `org.github` domain but the `io.github` domain is fine.
    window.set_app_id("simple-window");
    window.set_min_size(Some((2, 1)));

    // In order for the window to be mapped, we need to perform an initial commit with no attached buffer.
    // For more info, see WaylandSurface::commit
    //
    // The compositor will respond with an initial configure that we can then use to present to the window with
    // the correct options.
    window.commit();

    let mut simple_window = SimpleWindow {
        title: String::from("/usr/lib/xorg/modules/input"),
        registry_state,
        seat_state,
        output_state,
        compositor_state: Arc::new(compositor_state),
        subcompositor_state: Arc::new(subcompositor_state),
        shm_state,
        _xdg_shell_state: xdg_shell_state,

        exit: false,
        first_configure: true,
        pool,
        width: NonZeroU32::new(width).unwrap(),
        height: NonZeroU32::new(height).unwrap(),
        shift: None,
        buffer: None,
        window,
        window_frame: None,
        themed_pointer: None,
        set_cursor: false,
        cursor_icon: CursorIcon::Crosshair,
    };

    // We don't draw immediately, the configure will notify us when to first draw.

    loop {
        event_queue.blocking_dispatch(&mut simple_window).unwrap();

        if simple_window.exit {
            println!("exiting example");
            break;
        }
    }
}

struct SimpleWindow {
    title: String,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor_state: Arc<CompositorState>,
    subcompositor_state: Arc<SubcompositorState>,
    shm_state: Shm,
    _xdg_shell_state: XdgShell,

    exit: bool,
    first_configure: bool,
    pool: SlotPool,
    width: NonZeroU32,
    height: NonZeroU32,
    shift: Option<u32>,
    buffer: Option<Buffer>,
    window: Window,
    window_frame: Option<AdwaitaFrame<Self>>,
    themed_pointer: Option<ThemedPointer>,
    set_cursor: bool,
    cursor_icon: CursorIcon,
}

impl CompositorHandler for SimpleWindow {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if self.window.wl_surface() == surface {
            if let Some(frame) = self.window_frame.as_mut() {
                frame.set_scaling_factor(new_factor as f64);
            }
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
        // Not needed for this example.
    }

    fn frame(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(conn, qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for SimpleWindow {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl WindowHandler for SimpleWindow {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        self.buffer = None;

        println!(
            "Configure size {:?}, decorations: {:?}",
            configure.new_size, configure.decoration_mode
        );

        let (width, height) = if configure.decoration_mode == DecorationMode::Client {
            let window_frame = self.window_frame.get_or_insert_with(|| {
                let mut frame = AdwaitaFrame::new(
                    &self.window,
                    &self.shm_state,
                    self.compositor_state.clone(),
                    self.subcompositor_state.clone(),
                    qh.clone(),
                    FrameConfig::auto(),
                )
                .expect("failed to create client side decorations frame.");
                frame.set_title(self.title.clone());
                frame
            });

            // Un-hide the frame.
            window_frame.set_hidden(false);

            // Configure state before touching any resizing.
            window_frame.update_state(configure.state);

            // Configure the button state.
            window_frame.update_wm_capabilities(configure.capabilities);

            let (width, height) = match configure.new_size {
                (Some(width), Some(height)) => {
                    // The size could be 0.
                    window_frame.subtract_borders(width, height)
                }
                _ => {
                    // You might want to consider checking for configure bounds.
                    (Some(self.width), Some(self.height))
                }
            };

            // Clamp the size to at least one pixel.
            let width = width.unwrap_or(NonZeroU32::new(1).unwrap());
            let height = height.unwrap_or(NonZeroU32::new(1).unwrap());

            window_frame.resize(width, height);

            let (x, y) = window_frame.location();
            let outer_size = window_frame.add_borders(width.get(), height.get());
            window.xdg_surface().set_window_geometry(
                x,
                y,
                outer_size.0 as i32,
                outer_size.1 as i32,
            );

            (width, height)
        } else {
            // Hide the frame, if any.
            if let Some(frame) = self.window_frame.as_mut() {
                frame.set_hidden(true)
            }
            let width = configure.new_size.0.unwrap_or(self.width);
            let height = configure.new_size.1.unwrap_or(self.height);
            self.window.xdg_surface().set_window_geometry(
                0,
                0,
                width.get() as i32,
                height.get() as i32,
            );
            (width, height)
        };

        // Update new width and height;
        self.width = width;
        self.height = height;

        // Initiate the first draw.
        if self.first_configure {
            self.first_configure = false;
            self.draw(conn, qh);
        }
    }
}

impl SeatHandler for SimpleWindow {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.themed_pointer.is_none() {
            println!("Set pointer capability");
            println!("Creating pointer theme");
            let surface = self.compositor_state.create_surface(qh);
            let themed_pointer = self
                .seat_state
                .get_pointer_with_theme(
                    qh,
                    &seat,
                    self.shm_state.wl_shm(),
                    surface,
                    ThemeSpec::default(),
                )
                .expect("Failed to create pointer");
            self.themed_pointer.replace(themed_pointer);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.themed_pointer.is_some() {
            println!("Unset pointer capability");
            self.themed_pointer.take().unwrap().pointer().release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for SimpleWindow {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        use PointerEventKind::*;
        for event in events {
            let (x, y) = event.position;
            match event.kind {
                Enter { .. } => {
                    self.set_cursor = true;
                    self.cursor_icon = self
                        .window_frame
                        .as_mut()
                        .and_then(|frame| {
                            frame.click_point_moved(Duration::ZERO, &event.surface.id(), x, y)
                        })
                        .unwrap_or(CursorIcon::Crosshair)
                        .to_owned();

                    if &event.surface == self.window.wl_surface() {
                        println!("Pointer entered @{:?}", event.position);
                    }
                }
                Leave { .. } => {
                    if &event.surface != self.window.wl_surface() {
                        if let Some(window_frame) = self.window_frame.as_mut() {
                            window_frame.click_point_left();
                        }
                    }
                    println!("Pointer left");
                }
                Motion { time } => {
                    if let Some(new_cursor) = self.window_frame.as_mut().and_then(|frame| {
                        frame.click_point_moved(
                            Duration::from_millis(time as u64),
                            &event.surface.id(),
                            x,
                            y,
                        )
                    }) {
                        self.set_cursor = true;
                        self.cursor_icon = new_cursor.to_owned();
                    }
                }
                Press {
                    button,
                    serial,
                    time,
                }
                | Release {
                    button,
                    serial,
                    time,
                } => {
                    let pressed = if matches!(event.kind, Press { .. }) {
                        true
                    } else {
                        false
                    };
                    if &event.surface != self.window.wl_surface() {
                        let click = match button {
                            0x110 => FrameClick::Normal,
                            0x111 => FrameClick::Alternate,
                            _ => continue,
                        };

                        if let Some(action) = self.window_frame.as_mut().and_then(|frame| {
                            frame.on_click(Duration::from_millis(time as u64), click, pressed)
                        }) {
                            self.frame_action(pointer, serial, action);
                        }
                    } else if pressed {
                        println!("Press {:x} @ {:?}", button, event.position);
                        self.shift = self.shift.xor(Some(0));
                    }
                }
                Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    if &event.surface == self.window.wl_surface() {
                        println!("Scroll H:{horizontal:?}, V:{vertical:?}");
                    }
                }
            }
        }
    }
}
impl SimpleWindow {
    fn frame_action(&mut self, pointer: &wl_pointer::WlPointer, serial: u32, action: FrameAction) {
        let pointer_data = pointer.data::<PointerData>().unwrap();
        let seat = pointer_data.seat();
        match action {
            FrameAction::Close => self.exit = true,
            FrameAction::Minimize => self.window.set_minimized(),
            FrameAction::Maximize => self.window.set_maximized(),
            FrameAction::UnMaximize => self.window.unset_maximized(),
            FrameAction::ShowMenu(x, y) => self.window.show_window_menu(seat, serial, (x, y)),
            FrameAction::Resize(edge) => {
                let edge = match edge {
                    ResizeEdge::None => XdgResizeEdge::None,
                    ResizeEdge::Top => XdgResizeEdge::Top,
                    ResizeEdge::Bottom => XdgResizeEdge::Bottom,
                    ResizeEdge::Left => XdgResizeEdge::Left,
                    ResizeEdge::TopLeft => XdgResizeEdge::TopLeft,
                    ResizeEdge::BottomLeft => XdgResizeEdge::BottomLeft,
                    ResizeEdge::Right => XdgResizeEdge::Right,
                    ResizeEdge::TopRight => XdgResizeEdge::TopRight,
                    ResizeEdge::BottomRight => XdgResizeEdge::BottomRight,
                    _ => return,
                };
                self.window.resize(seat, serial, edge);
            }
            FrameAction::Move => self.window.move_(seat, serial),
            _ => (),
        }
    }
}

impl ShmHandler for SimpleWindow {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl SimpleWindow {
    pub fn draw(&mut self, conn: &Connection, qh: &QueueHandle<Self>) {
        if self.set_cursor {
            let _ = self
                .themed_pointer
                .as_mut()
                .unwrap()
                .set_cursor(conn, self.cursor_icon);
            self.set_cursor = false;
        }

        let width = self.width.get();
        let height = self.height.get();
        let stride = width as i32 * 4;

        let buffer = self.buffer.get_or_insert_with(|| {
            self.pool
                .create_buffer(
                    width as i32,
                    height as i32,
                    stride,
                    wl_shm::Format::Argb8888,
                )
                .expect("create buffer")
                .0
        });

        let canvas = match self.pool.canvas(buffer) {
            Some(canvas) => canvas,
            None => {
                // This should be rare, but if the compositor has not released the previous
                // buffer, we need double-buffering.
                let (second_buffer, canvas) = self
                    .pool
                    .create_buffer(
                        width as i32,
                        height as i32,
                        stride,
                        wl_shm::Format::Argb8888,
                    )
                    .expect("create buffer");
                *buffer = second_buffer;
                canvas
            }
        };

        // Draw to the window:
        {
            let shift = self.shift.unwrap_or(0);
            canvas
                .chunks_exact_mut(4)
                .enumerate()
                .for_each(|(index, chunk)| {
                    let x = ((index + shift as usize) % width as usize) as u32;
                    let y = (index / width as usize) as u32;

                    let a = 0xFF;
                    let r = u32::min(((width - x) * 0xFF) / width, ((height - y) * 0xFF) / height);
                    let g = u32::min((x * 0xFF) / width, ((height - y) * 0xFF) / height);
                    let b = u32::min(((width - x) * 0xFF) / width, (y * 0xFF) / height);
                    let color = (a << 24) + (r << 16) + (g << 8) + b;

                    let array: &mut [u8; 4] = chunk.try_into().unwrap();
                    *array = color.to_le_bytes();
                });

            if let Some(shift) = &mut self.shift {
                *shift = (*shift + 1) % width;
            }
        }

        // Draw the decorations frame.
        self.window_frame.as_mut().map(|frame| {
            if frame.is_dirty() && !frame.is_hidden() {
                frame.draw();
            }
        });

        // Damage the entire window
        self.window.wl_surface().damage_buffer(
            0,
            0,
            self.width.get() as i32,
            self.height.get() as i32,
        );

        // Request our next frame
        self.window
            .wl_surface()
            .frame(qh, self.window.wl_surface().clone());

        // Attach and commit to present.
        buffer
            .attach_to(self.window.wl_surface())
            .expect("buffer attach");
        self.window.wl_surface().commit();
    }
}

delegate_compositor!(SimpleWindow);
delegate_subcompositor!(SimpleWindow);
delegate_output!(SimpleWindow);
delegate_shm!(SimpleWindow);

delegate_seat!(SimpleWindow);
delegate_pointer!(SimpleWindow);

delegate_xdg_shell!(SimpleWindow);
delegate_xdg_window!(SimpleWindow);

delegate_registry!(SimpleWindow);

impl ProvidesRegistryState for SimpleWindow {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState,];
}
