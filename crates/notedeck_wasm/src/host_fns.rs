use crate::commands::{button_key, UiCommand};
use std::collections::HashMap;
use wasmer::{FunctionEnv, FunctionEnvMut, Imports, Memory, MemoryView, Store};

pub struct HostEnv {
    pub memory: Option<Memory>,
    pub commands: Vec<UiCommand>,
    pub button_events: HashMap<String, bool>,
    pub button_occ: HashMap<String, u32>,
    pub available_width: f32,
    pub available_height: f32,
}

impl HostEnv {
    pub fn new() -> Self {
        Self {
            memory: None,
            commands: Vec::new(),
            button_events: HashMap::new(),
            button_occ: HashMap::new(),
            available_width: 0.0,
            available_height: 0.0,
        }
    }
}

/// Read a UTF-8 string from WASM linear memory.
fn read_wasm_str(view: &MemoryView, ptr: i32, len: i32) -> Option<String> {
    let ptr = ptr as u64;
    let len = len as usize;
    let mut buf = vec![0u8; len];
    view.read(ptr, &mut buf).ok()?;
    String::from_utf8(buf).ok()
}

/// Register all host imports into the given store.
pub fn create_imports(store: &mut Store, env: &FunctionEnv<HostEnv>) -> Imports {
    use wasmer::Function;

    // --- Widgets ---

    fn nd_label(mut env: FunctionEnvMut<HostEnv>, ptr: i32, len: i32) {
        let (data, store) = env.data_and_store_mut();
        let memory = data.memory.as_ref().expect("memory not set");
        let view = memory.view(&store);
        if let Some(text) = read_wasm_str(&view, ptr, len) {
            data.commands.push(UiCommand::Label(text));
        }
    }

    fn nd_heading(mut env: FunctionEnvMut<HostEnv>, ptr: i32, len: i32) {
        let (data, store) = env.data_and_store_mut();
        let memory = data.memory.as_ref().expect("memory not set");
        let view = memory.view(&store);
        if let Some(text) = read_wasm_str(&view, ptr, len) {
            data.commands.push(UiCommand::Heading(text));
        }
    }

    fn nd_button(mut env: FunctionEnvMut<HostEnv>, ptr: i32, len: i32) -> i32 {
        let (data, store) = env.data_and_store_mut();
        let memory = data.memory.as_ref().expect("memory not set");
        let view = memory.view(&store);
        if let Some(text) = read_wasm_str(&view, ptr, len) {
            let occ = data.button_occ.entry(text.clone()).or_insert(0);
            let key = button_key(&text, *occ);
            *occ += 1;
            let clicked = data.button_events.get(&key).copied().unwrap_or(false);
            data.commands.push(UiCommand::Button(text));
            if clicked {
                1
            } else {
                0
            }
        } else {
            0
        }
    }

    fn nd_add_space(mut env: FunctionEnvMut<HostEnv>, pixels: f32) {
        let (data, _store) = env.data_and_store_mut();
        data.commands.push(UiCommand::AddSpace(pixels));
    }

    // --- Layout queries ---

    fn nd_available_width(env: FunctionEnvMut<HostEnv>) -> f32 {
        env.data().available_width
    }

    fn nd_available_height(env: FunctionEnvMut<HostEnv>) -> f32 {
        env.data().available_height
    }

    // --- Drawing primitives (coordinates relative to app rect) ---

    fn nd_draw_rect(mut env: FunctionEnvMut<HostEnv>, x: f32, y: f32, w: f32, h: f32, color: i32) {
        let (data, _store) = env.data_and_store_mut();
        data.commands.push(UiCommand::DrawRect {
            x,
            y,
            w,
            h,
            color: color as u32,
        });
    }

    fn nd_draw_circle(mut env: FunctionEnvMut<HostEnv>, cx: f32, cy: f32, r: f32, color: i32) {
        let (data, _store) = env.data_and_store_mut();
        data.commands.push(UiCommand::DrawCircle {
            cx,
            cy,
            r,
            color: color as u32,
        });
    }

    fn nd_draw_line(
        mut env: FunctionEnvMut<HostEnv>,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: i32,
    ) {
        let (data, _store) = env.data_and_store_mut();
        data.commands.push(UiCommand::DrawLine {
            x1,
            y1,
            x2,
            y2,
            width,
            color: color as u32,
        });
    }

    fn nd_draw_text(
        mut env: FunctionEnvMut<HostEnv>,
        x: f32,
        y: f32,
        ptr: i32,
        len: i32,
        size: f32,
        color: i32,
    ) {
        let (data, store) = env.data_and_store_mut();
        let memory = data.memory.as_ref().expect("memory not set");
        let view = memory.view(&store);
        if let Some(text) = read_wasm_str(&view, ptr, len) {
            data.commands.push(UiCommand::DrawText {
                x,
                y,
                text,
                size,
                color: color as u32,
            });
        }
    }

    wasmer::imports! {
        "env" => {
            "nd_label" => Function::new_typed_with_env(store, env, nd_label),
            "nd_heading" => Function::new_typed_with_env(store, env, nd_heading),
            "nd_button" => Function::new_typed_with_env(store, env, nd_button),
            "nd_add_space" => Function::new_typed_with_env(store, env, nd_add_space),
            "nd_available_width" => Function::new_typed_with_env(store, env, nd_available_width),
            "nd_available_height" => Function::new_typed_with_env(store, env, nd_available_height),
            "nd_draw_rect" => Function::new_typed_with_env(store, env, nd_draw_rect),
            "nd_draw_circle" => Function::new_typed_with_env(store, env, nd_draw_circle),
            "nd_draw_line" => Function::new_typed_with_env(store, env, nd_draw_line),
            "nd_draw_text" => Function::new_typed_with_env(store, env, nd_draw_text),
        }
    }
}
