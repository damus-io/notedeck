//! A shot: the real notedeck, filmed with no window and no display.
//!
//! `notedeck --shot out.png` boots the app as the windowed binary does (the
//! same [`Notedeck`], the same [`Chrome`] and app roster, the same egui setup)
//! but drives its frames itself instead of through eframe and winit. Each
//! frame is the app's own [`Notedeck::tick`] inside an egui pass; input comes
//! from the command line's script (see [`script`]) rather than a window
//! system, and a `--shot` step draws that frame with egui's wgpu renderer
//! into an offscreen texture (see [`offscreen`]) and writes it as a PNG.
//!
//! So a session can look at the real app on a machine with no `DISPLAY`,
//! with no Xvfb and no xdotool:
//!
//! ```text
//! notedeck --datapath d --wait 1 --key '<C-Space>' --wait 0.3 --shot out.png
//! ```

mod offscreen;
pub mod script;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Event, FullOutput, Key, Modifiers, PointerButton, Pos2, RawInput, ViewportId};
use notedeck::Notedeck;

use crate::keymap::{KeyStroke, Mods};
use crate::setup::generate_native_options;
use crate::Chrome;
use offscreen::Offscreen;
pub use script::{ShotArgError, ShotPlan, ShotSize, Step};

/// How far apart frames are in time: a 60 Hz display's.
const FRAME: Duration = Duration::from_micros(16_667);

/// The display's own scale. Xvfb and most desktop monitors are 1.
const NATIVE_PIXELS_PER_POINT: f32 = 1.0;

/// Why a shot failed.
#[derive(Debug)]
pub enum ShotError {
    /// No GPU adapter or device could be had.
    Gpu(String),
    /// The chrome and its apps couldn't be built.
    Chrome(notedeck::Error),
    /// A frame couldn't be drawn or read back.
    Render(String),
    /// A PNG couldn't be written.
    Write {
        /// Where it was going.
        path: PathBuf,
        /// What went wrong.
        err: image::ImageError,
    },
}

impl std::fmt::Display for ShotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShotError::Gpu(err) => write!(f, "no GPU to draw on: {err}"),
            ShotError::Chrome(err) => write!(f, "couldn't start the apps: {err}"),
            ShotError::Render(err) => write!(f, "couldn't draw the frame: {err}"),
            ShotError::Write { path, err } => write!(f, "couldn't write {}: {err}", path.display()),
        }
    }
}

impl std::error::Error for ShotError {}

/// Boot notedeck from `base_path` with `plan`'s args, run its script, and
/// write its shots. Returns once the last shot is written, after the app has
/// shut down as it would on a window close.
pub async fn run(base_path: PathBuf, plan: ShotPlan) -> Result<(), ShotError> {
    // The options a window would get, so the renderer is set up as eframe
    // sets up a window's.
    let native = generate_native_options(notedeck::DataPath::new(&base_path), false);
    let offscreen = Offscreen::new(&native)
        .await
        .map_err(|err| ShotError::Gpu(err.to_string()))?;

    let ctx = window_context();
    let mut notedeck = Notedeck::init(&ctx, &base_path, &plan.args);
    notedeck.setup(&ctx);
    let chrome =
        Chrome::new_with_render_state(Some(offscreen.render_state()), &plan.args, &mut notedeck)
            .map_err(ShotError::Chrome)?;
    notedeck.set_app(chrome);

    let mut camera = Camera {
        ctx,
        notedeck,
        offscreen,
        size: plan.size,
        started: Instant::now(),
        pointer: Pos2::ZERO,
    };
    for step in &plan.steps {
        camera.step(step).await?;
    }
    Ok(())
}

/// An egui context set up as eframe sets up a desktop window's.
fn window_context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.options_mut(|o| o.max_passes = 2.try_into().expect("2 is nonzero"));
    // eframe's fallback for glyphs notedeck's own fonts lack.
    ctx.add_font_provider(std::sync::Arc::new(
        egui_system_fonts::SystemFontProvider::new(),
    ));
    ctx
}

/// The app being filmed, and the frame loop that drives it.
struct Camera {
    ctx: egui::Context,
    notedeck: Notedeck,
    offscreen: Offscreen,
    /// The window's size in pixels.
    size: ShotSize,
    /// When the first frame ran; egui's clock counts from here.
    started: Instant,
    /// Where the pointer is, in points.
    pointer: Pos2,
}

impl Camera {
    /// Run one step of the script.
    async fn step(&mut self, step: &Step) -> Result<(), ShotError> {
        match step {
            Step::Wait(duration) => {
                let until = Instant::now() + *duration;
                while Instant::now() < until {
                    self.idle_frame(Vec::new()).await;
                }
            }
            Step::Frames(count) => {
                for _ in 0..*count {
                    self.idle_frame(Vec::new()).await;
                }
            }
            Step::Key(seq) => {
                for stroke in seq.as_slice() {
                    self.idle_frame(key_press(*stroke)).await;
                    self.idle_frame(key_release(*stroke)).await;
                }
            }
            Step::Type(text) => self.idle_frame(vec![Event::Text(text.clone())]).await,
            Step::Hover(px) => {
                self.pointer = self.to_points(*px);
                self.idle_frame(vec![Event::PointerMoved(self.pointer)])
                    .await;
            }
            Step::Click(px) => {
                self.pointer = self.to_points(*px);
                self.idle_frame(vec![Event::PointerMoved(self.pointer)])
                    .await;
                for pressed in [true, false] {
                    let button = Event::PointerButton {
                        pos: self.pointer,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    };
                    self.idle_frame(vec![button]).await;
                }
            }
            Step::Shot(path) => self.shoot(path)?,
        }
        Ok(())
    }

    /// The scale egui draws at: the display's, times notedeck's zoom.
    fn pixels_per_point(&self) -> f32 {
        NATIVE_PIXELS_PER_POINT * self.ctx.zoom_factor()
    }

    /// A position in the window's pixels, in egui's points.
    fn to_points(&self, px: Pos2) -> Pos2 {
        (px.to_vec2() / self.pixels_per_point()).to_pos2()
    }

    /// The input a window would send for a frame with `events`.
    fn raw_input(&self, events: Vec<Event>) -> RawInput {
        let size_px = egui::vec2(self.size.width as f32, self.size.height as f32);
        let screen = egui::Rect::from_min_size(Pos2::ZERO, size_px / self.pixels_per_point());
        let mut raw = RawInput {
            screen_rect: Some(screen),
            time: Some(self.started.elapsed().as_secs_f64()),
            predicted_dt: FRAME.as_secs_f32(),
            focused: true,
            events,
            ..Default::default()
        };
        let viewport = raw.viewports.entry(ViewportId::ROOT).or_default();
        viewport.native_pixels_per_point = Some(NATIVE_PIXELS_PER_POINT);
        viewport.monitor_size = Some(screen.size());
        viewport.inner_rect = Some(screen);
        viewport.outer_rect = Some(screen);
        viewport.focused = Some(true);
        raw
    }

    /// One pass of the app, as eframe runs it; its new textures are
    /// uploaded, its freed ones are left to the caller.
    fn frame(&mut self, events: Vec<Event>) -> FullOutput {
        let raw = self.raw_input(events);
        let notedeck = &mut self.notedeck;
        let output = self.ctx.run_ui(raw, |ui| notedeck.tick(ui));
        self.offscreen.set_textures(&output.textures_delta);
        output
    }

    /// A frame nobody looks at, then the gap to the next.
    async fn idle_frame(&mut self, events: Vec<Event>) {
        let output = self.frame(events);
        self.offscreen.free_textures(&output.textures_delta);
        tokio::time::sleep(FRAME).await;
    }

    /// Run a frame and write it to `path`.
    fn shoot(&mut self, path: &Path) -> Result<(), ShotError> {
        let output = self.frame(Vec::new());
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let clear_color =
            eframe::App::clear_color(&self.notedeck, &self.ctx.global_style().visuals);
        let image = self.offscreen.render(
            &primitives,
            output.pixels_per_point,
            [self.size.width, self.size.height],
            clear_color,
        );
        self.offscreen.free_textures(&output.textures_delta);
        image
            .map_err(ShotError::Render)?
            .save(path)
            .map_err(|err| ShotError::Write {
                path: path.to_owned(),
                err,
            })?;
        println!("{}", path.display());
        Ok(())
    }
}

/// The modifiers a window system reports with `mods` held, as egui-winit
/// maps them: `command` is Ctrl, except on macOS where it is Cmd.
fn modifiers(mods: Mods) -> Modifiers {
    let ctrl = mods.contains(Mods::CTRL);
    let cmd = mods.contains(Mods::CMD);
    let mac = cfg!(target_os = "macos");
    Modifiers {
        alt: mods.contains(Mods::ALT),
        ctrl,
        shift: mods.contains(Mods::SHIFT),
        mac_cmd: mac && cmd,
        command: if mac { cmd } else { ctrl },
    }
}

/// The text a press of `stroke` types, as a window system sends it beside
/// the key: none while Ctrl, Alt or Cmd is held, nor for a named key.
fn typed_text(stroke: KeyStroke) -> Option<String> {
    if stroke.mods.contains(Mods::CTRL)
        || stroke.mods.contains(Mods::ALT)
        || stroke.mods.contains(Mods::CMD)
    {
        return None;
    }
    let c = match stroke.key {
        Key::Space => ' ',
        // egui's symbol for it is the Unicode minus sign.
        Key::Minus => '-',
        key => {
            let mut chars = key.symbol_or_name().chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_graphic() => c,
                _ => return None,
            }
        }
    };
    let c = if stroke.mods.contains(Mods::SHIFT) {
        c.to_ascii_uppercase()
    } else {
        c.to_ascii_lowercase()
    };
    Some(c.to_string())
}

/// The events of pressing `stroke`: the modifiers going down, the key, and
/// the text it types.
fn key_press(stroke: KeyStroke) -> Vec<Event> {
    let modifiers = modifiers(stroke.mods);
    let mut events = vec![
        Event::ModifiersChanged(modifiers),
        Event::Key {
            key: stroke.key,
            physical_key: Some(stroke.key),
            pressed: true,
            repeat: false,
            modifiers,
        },
    ];
    events.extend(typed_text(stroke).map(Event::Text));
    events
}

/// The events of letting go of `stroke`: the key, then the modifiers.
fn key_release(stroke: KeyStroke) -> Vec<Event> {
    vec![
        Event::Key {
            key: stroke.key,
            physical_key: Some(stroke.key),
            pressed: false,
            repeat: false,
            modifiers: modifiers(stroke.mods),
        },
        Event::ModifiersChanged(Modifiers::NONE),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(token: &str) -> KeyStroke {
        KeyStroke::from_token(token).unwrap()
    }

    #[test]
    fn ctrl_k_reaches_egui_as_the_command_shortcut() {
        let events = key_press(stroke("<C-k>"));
        let Event::Key { key, modifiers, .. } = &events[1] else {
            panic!("no key event in {events:?}");
        };
        assert_eq!(*key, Key::K);
        if cfg!(not(target_os = "macos")) {
            assert!(modifiers.matches_exact(Modifiers::COMMAND));
        }
        // A shortcut types nothing.
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn plain_keys_type_their_character() {
        assert_eq!(typed_text(stroke("g")).as_deref(), Some("g"));
        assert_eq!(typed_text(stroke("G")).as_deref(), Some("G"));
        assert_eq!(typed_text(stroke("?")).as_deref(), Some("?"));
        assert_eq!(typed_text(stroke("<Space>")).as_deref(), Some(" "));
        assert_eq!(typed_text(stroke("<Enter>")), None);
        assert_eq!(typed_text(stroke("<Esc>")), None);
    }
}
