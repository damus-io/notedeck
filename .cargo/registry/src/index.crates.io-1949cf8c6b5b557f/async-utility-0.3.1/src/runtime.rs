// Copyright (c) 2022-2023 Yuki Kishimoto
// Distributed under the MIT software license

//! Runtime

use std::borrow::Cow;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle, Runtime};

// TODO: use LazyLock when MSRV will be at 1.80.0
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime")
    })
}

pub(crate) fn handle() -> Cow<'static, Handle> {
    match Handle::try_current() {
        Ok(handle) => Cow::Owned(handle),
        Err(..) => {
            let rt: &Runtime = runtime();
            let handle: &Handle = rt.handle();
            Cow::Borrowed(handle)
        }
    }
}

#[inline]
pub(crate) fn is_tokio_context() -> bool {
    Handle::try_current().is_ok()
}
