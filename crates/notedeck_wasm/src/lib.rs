mod commands;
mod host_fns;

use host_fns::HostEnv;
use notedeck::{AppContext, AppResponse};
use wasmer::{FunctionEnv, Instance, Module, Store};

pub struct WasmApp {
    store: Store,
    instance: Instance,
    env: FunctionEnv<HostEnv>,
    name: String,
}

impl WasmApp {
    /// Load a WASM app from raw bytes.
    pub fn from_bytes(wasm_bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut store = Store::default();
        let module = Module::new(&store, wasm_bytes)?;

        let host_env = HostEnv::new();
        let env = FunctionEnv::new(&mut store, host_env);

        let imports = host_fns::create_imports(&mut store, &env);
        let instance = Instance::new(&mut store, &module, &imports)?;

        // Give host functions access to WASM linear memory.
        let memory = instance.exports.get_memory("memory")?.clone();
        env.as_mut(&mut store).memory = Some(memory.clone());

        // Read app name from exported globals, if present.
        let name =
            read_app_name(&instance, &mut store, &memory).unwrap_or_else(|| "WASM App".to_string());

        Ok(Self {
            store,
            instance,
            env,
            name,
        })
    }

    /// Load a WASM app from a file path.
    pub fn from_file(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// The display name of this WASM app.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Read app name from WASM exports: nd_app_name_ptr (i32) and nd_app_name_len (i32).
fn read_app_name(
    instance: &Instance,
    store: &mut Store,
    memory: &wasmer::Memory,
) -> Option<String> {
    let ptr_global = instance.exports.get_global("nd_app_name_ptr").ok()?;
    let len_global = instance.exports.get_global("nd_app_name_len").ok()?;

    let ptr = ptr_global.get(store).i32()? as u64;
    let len = len_global.get(store).i32()? as usize;

    if len == 0 {
        return None;
    }

    let view = memory.view(store);
    let mut buf = vec![0u8; len];
    view.read(ptr, &mut buf).ok()?;
    String::from_utf8(buf).ok()
}

impl notedeck::App for WasmApp {
    fn update(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        // 1. Clear commands and reset occurrence counter
        {
            let data = self.env.as_mut(&mut self.store);
            data.commands.clear();
            data.button_occ.clear();
        }

        // 2. Run WASM — host functions push commands
        let nd_update = self
            .instance
            .exports
            .get_function("nd_update")
            .expect("WASM module must export nd_update");
        if let Err(e) = nd_update.call(&mut self.store, &[]) {
            tracing::error!("WASM nd_update error: {e}");
        }

        // 3. Take commands and render with real UI
        let cmds = {
            let data = self.env.as_mut(&mut self.store);
            std::mem::take(&mut data.commands)
        };
        let new_events = commands::render_commands(&cmds, ui);

        // 4. Store events for next frame
        self.env.as_mut(&mut self.store).button_events = new_events;

        AppResponse::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::UiCommand;

    /// Helper: compile WAT to WASM bytes and load as WasmApp.
    fn app_from_wat(wat: &str) -> WasmApp {
        let wasm = wat::parse_str(wat).expect("valid WAT");
        WasmApp::from_bytes(&wasm).expect("load WASM module")
    }

    /// Helper: run one frame of the WASM app inside a headless egui context.
    /// Returns the commands that were generated.
    fn run_update(app: &mut WasmApp) -> Vec<UiCommand> {
        let mut result_cmds = Vec::new();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Clear + reset
                {
                    let data = app.env.as_mut(&mut app.store);
                    data.commands.clear();
                    data.button_occ.clear();
                }

                // Run WASM
                let nd_update = app
                    .instance
                    .exports
                    .get_function("nd_update")
                    .expect("nd_update");
                nd_update.call(&mut app.store, &[]).expect("nd_update ok");

                // Grab commands before rendering
                let cmds = {
                    let data = app.env.as_mut(&mut app.store);
                    std::mem::take(&mut data.commands)
                };
                result_cmds = cmds
                    .iter()
                    .map(|c| match c {
                        UiCommand::Label(t) => UiCommand::Label(t.clone()),
                        UiCommand::Heading(t) => UiCommand::Heading(t.clone()),
                        UiCommand::Button(t) => UiCommand::Button(t.clone()),
                        UiCommand::AddSpace(px) => UiCommand::AddSpace(*px),
                    })
                    .collect();

                // Render and collect events
                let new_events = commands::render_commands(&cmds, ui);
                app.env.as_mut(&mut app.store).button_events = new_events;
            });
        });
        result_cmds
    }

    #[test]
    fn load_empty_module() {
        let app = app_from_wat(
            r#"(module
                (memory (export "memory") 1)
                (func (export "nd_update"))
            )"#,
        );
        assert!(app.instance.exports.get_function("nd_update").is_ok());
    }

    #[test]
    fn run_noop_update() {
        let mut app = app_from_wat(
            r#"(module
                (memory (export "memory") 1)
                (func (export "nd_update"))
            )"#,
        );
        let cmds = run_update(&mut app);
        assert!(cmds.is_empty());
    }

    #[test]
    fn call_nd_label() {
        let mut app = app_from_wat(
            r#"(module
                (import "env" "nd_label" (func $nd_label (param i32 i32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "hi")
                (func (export "nd_update")
                    (call $nd_label (i32.const 0) (i32.const 2))
                )
            )"#,
        );
        let cmds = run_update(&mut app);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], UiCommand::Label(t) if t == "hi"));
    }

    #[test]
    fn call_nd_heading() {
        let mut app = app_from_wat(
            r#"(module
                (import "env" "nd_heading" (func $nd_heading (param i32 i32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "Title")
                (func (export "nd_update")
                    (call $nd_heading (i32.const 0) (i32.const 5))
                )
            )"#,
        );
        let cmds = run_update(&mut app);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], UiCommand::Heading(t) if t == "Title"));
    }

    #[test]
    fn call_nd_button() {
        let mut app = app_from_wat(
            r#"(module
                (import "env" "nd_button" (func $nd_button (param i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "Click")
                (func (export "nd_update")
                    (drop (call $nd_button (i32.const 0) (i32.const 5)))
                )
            )"#,
        );
        let cmds = run_update(&mut app);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], UiCommand::Button(t) if t == "Click"));
    }

    #[test]
    fn call_nd_add_space() {
        let mut app = app_from_wat(
            r#"(module
                (import "env" "nd_add_space" (func $nd_add_space (param f32)))
                (memory (export "memory") 1)
                (func (export "nd_update")
                    (call $nd_add_space (f32.const 10.0))
                )
            )"#,
        );
        let cmds = run_update(&mut app);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], UiCommand::AddSpace(px) if (*px - 10.0).abs() < f32::EPSILON));
    }

    #[test]
    fn call_multiple_host_fns() {
        let mut app = app_from_wat(
            r#"(module
                (import "env" "nd_heading" (func $nd_heading (param i32 i32)))
                (import "env" "nd_label" (func $nd_label (param i32 i32)))
                (import "env" "nd_button" (func $nd_button (param i32 i32) (result i32)))
                (import "env" "nd_add_space" (func $nd_add_space (param f32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "Hello")
                (data (i32.const 5) "World")
                (data (i32.const 10) "Btn")
                (func (export "nd_update")
                    (call $nd_heading (i32.const 0) (i32.const 5))
                    (call $nd_add_space (f32.const 8.0))
                    (call $nd_label (i32.const 5) (i32.const 5))
                    (drop (call $nd_button (i32.const 10) (i32.const 3)))
                )
            )"#,
        );
        let cmds = run_update(&mut app);
        assert_eq!(cmds.len(), 4);
        assert!(matches!(&cmds[0], UiCommand::Heading(t) if t == "Hello"));
        assert!(matches!(&cmds[1], UiCommand::AddSpace(_)));
        assert!(matches!(&cmds[2], UiCommand::Label(t) if t == "World"));
        assert!(matches!(&cmds[3], UiCommand::Button(t) if t == "Btn"));
    }

    #[test]
    fn button_returns_prev_frame_event() {
        // Module that stores nd_button's return value in a global.
        let mut app = app_from_wat(
            r#"(module
                (import "env" "nd_button" (func $nd_button (param i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "Click")
                (global $result (mut i32) (i32.const -1))
                (global (export "btn_result") (mut i32) (i32.const -1))
                (func (export "nd_update")
                    (global.set $result (call $nd_button (i32.const 0) (i32.const 5)))
                    (global.set 1 (global.get $result))
                )
            )"#,
        );

        // Frame 1: no previous events, button returns 0
        run_update(&mut app);
        let result = app
            .instance
            .exports
            .get_global("btn_result")
            .unwrap()
            .get(&mut app.store)
            .i32()
            .unwrap();
        assert_eq!(result, 0, "first frame: button should return 0");

        // Simulate a click by injecting an event
        app.env
            .as_mut(&mut app.store)
            .button_events
            .insert("Click".to_string(), true);

        // Frame 2: button should now return 1
        run_update(&mut app);
        let result = app
            .instance
            .exports
            .get_global("btn_result")
            .unwrap()
            .get(&mut app.store)
            .i32()
            .unwrap();
        assert_eq!(
            result, 1,
            "second frame: button should return 1 after click"
        );
    }

    #[test]
    fn app_name_from_exports() {
        let app = app_from_wat(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 500) "Test App")
                (global (export "nd_app_name_ptr") i32 (i32.const 500))
                (global (export "nd_app_name_len") i32 (i32.const 8))
                (func (export "nd_update"))
            )"#,
        );
        assert_eq!(app.name(), "Test App");
    }

    #[test]
    fn app_name_defaults_when_missing() {
        let app = app_from_wat(
            r#"(module
                (memory (export "memory") 1)
                (func (export "nd_update"))
            )"#,
        );
        assert_eq!(app.name(), "WASM App");
    }

    #[test]
    fn from_bytes_rejects_invalid_wasm() {
        let result = WasmApp::from_bytes(b"not wasm");
        assert!(result.is_err());
    }
}
