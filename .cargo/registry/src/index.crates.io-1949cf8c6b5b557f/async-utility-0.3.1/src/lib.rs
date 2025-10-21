// Copyright (c) 2022-2023 Yuki Kishimoto
// Distributed under the MIT software license

//! Async Utility

pub extern crate futures_util;
pub extern crate tokio;

#[cfg(not(target_arch = "wasm32"))]
mod runtime;
pub mod task;
pub mod time;
