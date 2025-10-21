// Copyright (c) 2022-2023 Yuki Kishimoto
// Distributed under the MIT software license

//! Time module

use core::future::Future;
use core::time::Duration;

use futures_util::future::{AbortHandle, Abortable};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local;

#[cfg(not(target_arch = "wasm32"))]
use crate::runtime;

/// Sleep
pub async fn sleep(duration: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    if runtime::is_tokio_context() {
        tokio::time::sleep(duration).await;
    } else {
        // No need to propagate error
        let _ = runtime::handle()
            .spawn(async move {
                tokio::time::sleep(duration).await;
            })
            .await;
    }

    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::sleep(duration).await;
}

/// Timeout
pub async fn timeout<F>(timeout: Option<Duration>, future: F) -> Option<F::Output>
where
    F: Future,
{
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(timeout) = timeout {
        if runtime::is_tokio_context() {
            tokio::time::timeout(timeout, future).await.ok()
        } else {
            let (abort_handle, abort_registration) = AbortHandle::new_pair();
            let future = Abortable::new(future, abort_registration);
            tokio::select! {
                res = future => {
                    res.ok()
                }
                _ = sleep(timeout) => {
                    abort_handle.abort();
                    None
                }
            }
        }
    } else {
        Some(future.await)
    }

    #[cfg(target_arch = "wasm32")]
    {
        if let Some(timeout) = timeout {
            let (abort_handle, abort_registration) = AbortHandle::new_pair();
            let future = Abortable::new(future, abort_registration);
            spawn_local(async move {
                gloo_timers::callback::Timeout::new(timeout.as_millis() as u32, move || {
                    abort_handle.abort();
                })
                .forget();
            });
            future.await.ok()
        } else {
            Some(future.await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO: test also wasm

    #[tokio::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn test_sleep_in_tokio() {
        sleep(Duration::from_secs(5)).await;
    }

    #[async_std::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn test_sleep_in_async_std() {
        sleep(Duration::from_secs(5)).await;
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_sleep_in_smol() {
        smol::block_on(async {
            sleep(Duration::from_secs(5)).await;
        });
    }

    #[tokio::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn test_timeout_tokio() {
        // Timeout
        let result = timeout(Some(Duration::from_secs(1)), async {
            sleep(Duration::from_secs(2)).await;
        })
        .await;
        assert!(result.is_none());

        // Not timeout
        let result = timeout(Some(Duration::from_secs(10)), async {
            sleep(Duration::from_secs(1)).await;
        })
        .await;
        assert!(result.is_some());
    }

    #[async_std::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn test_timeout_async_std() {
        // Timeout
        let result = timeout(Some(Duration::from_secs(1)), async {
            sleep(Duration::from_secs(2)).await;
        })
        .await;
        assert!(result.is_none());

        // Not timeout
        let result = timeout(Some(Duration::from_secs(10)), async {
            sleep(Duration::from_secs(1)).await;
        })
        .await;
        assert!(result.is_some());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn test_timeout_smol() {
        smol::block_on(async {
            // Timeout
            let result = timeout(Some(Duration::from_secs(1)), async {
                sleep(Duration::from_secs(2)).await;
            })
            .await;
            assert!(result.is_none());

            // Not timeout
            let result = timeout(Some(Duration::from_secs(10)), async {
                sleep(Duration::from_secs(1)).await;
            })
            .await;
            assert!(result.is_some());
        });
    }
}
