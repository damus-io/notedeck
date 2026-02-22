use crate::commands::{button_key, UiCommand};
use std::collections::HashMap;
use wasmer::{FunctionEnv, FunctionEnvMut, Imports, Memory, MemoryView, Store};

pub struct HostEnv {
    pub memory: Option<Memory>,
    pub commands: Vec<UiCommand>,
    pub button_events: HashMap<String, bool>,
    pub button_occ: HashMap<String, u32>,
}

impl HostEnv {
    pub fn new() -> Self {
        Self {
            memory: None,
            commands: Vec::new(),
            button_events: HashMap::new(),
            button_occ: HashMap::new(),
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

    wasmer::imports! {
        "env" => {
            "nd_label" => Function::new_typed_with_env(store, env, nd_label),
            "nd_heading" => Function::new_typed_with_env(store, env, nd_heading),
            "nd_button" => Function::new_typed_with_env(store, env, nd_button),
            "nd_add_space" => Function::new_typed_with_env(store, env, nd_add_space),
        }
    }
}
