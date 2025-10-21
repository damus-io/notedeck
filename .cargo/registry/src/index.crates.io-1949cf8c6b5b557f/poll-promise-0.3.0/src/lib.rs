//! `poll-promise` is a Rust crate for polling the result of a concurrent (e.g. `async`) operation.
//!
//! It is particularly useful in games and immediate mode GUI:s, where one often wants to start
//! a background operation and then ask "are we there yet?" on each subsequent frame
//! until the operation completes.
//!
//! Example:
//!
//! ```
//! # fn something_slow() {}
//! # use poll_promise::Promise;
//! #
//! let promise = Promise::spawn_thread("slow_operation", something_slow);
//!
//! // Then in the game loop or immediate mode GUI code:
//! if let Some(result) = promise.ready() {
//!     // Use/show result
//! } else {
//!     // Show a loading screen
//! }
//! ```
//!
//! ## Features
//! `poll-promise` can be used with any async runtime (or without one!),
//! but a few convenience methods are added
//! when compiled with the following features:
//!
#![doc = document_features::document_features!()]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
//!

// BEGIN - Embark standard lints v6 for Rust 1.55+
// do not change or add/remove here, but one can add exceptions after this section
// for more info see: <https://github.com/EmbarkStudios/rust-ecosystem/issues/59>
#![deny(unsafe_code)]
#![warn(
    clippy::all,
    clippy::await_holding_lock,
    clippy::char_lit_as_u8,
    clippy::checked_conversions,
    clippy::dbg_macro,
    clippy::debug_assert_with_mut_call,
    clippy::doc_markdown,
    clippy::empty_enum,
    clippy::enum_glob_use,
    clippy::exit,
    clippy::expl_impl_clone_on_copy,
    clippy::explicit_deref_methods,
    clippy::explicit_into_iter_loop,
    clippy::fallible_impl_from,
    clippy::filter_map_next,
    clippy::flat_map_option,
    clippy::float_cmp_const,
    clippy::fn_params_excessive_bools,
    clippy::from_iter_instead_of_collect,
    clippy::if_let_mutex,
    clippy::implicit_clone,
    clippy::imprecise_flops,
    clippy::inefficient_to_string,
    clippy::invalid_upcast_comparisons,
    clippy::large_digit_groups,
    clippy::large_stack_arrays,
    clippy::large_types_passed_by_value,
    clippy::let_unit_value,
    clippy::linkedlist,
    clippy::lossy_float_literal,
    clippy::macro_use_imports,
    clippy::manual_ok_or,
    clippy::map_err_ignore,
    clippy::map_flatten,
    clippy::map_unwrap_or,
    clippy::match_on_vec_items,
    clippy::match_same_arms,
    clippy::match_wild_err_arm,
    clippy::match_wildcard_for_single_variants,
    clippy::mem_forget,
    clippy::mismatched_target_os,
    clippy::missing_enforced_import_renames,
    clippy::mut_mut,
    clippy::mutex_integer,
    clippy::needless_borrow,
    clippy::needless_continue,
    clippy::needless_for_each,
    clippy::option_option,
    clippy::path_buf_push_overwrite,
    clippy::ptr_as_ptr,
    clippy::rc_mutex,
    clippy::ref_option_ref,
    clippy::rest_pat_in_fully_bound_structs,
    clippy::same_functions_in_if_condition,
    clippy::semicolon_if_nothing_returned,
    clippy::single_match_else,
    clippy::string_add_assign,
    clippy::string_add,
    clippy::string_lit_as_bytes,
    clippy::string_to_string,
    clippy::todo,
    clippy::trait_duplication_in_bounds,
    clippy::unimplemented,
    clippy::unnested_or_patterns,
    clippy::unused_self,
    clippy::useless_transmute,
    clippy::verbose_file_reads,
    clippy::zero_sized_map_values,
    future_incompatible,
    nonstandard_style,
    rust_2018_idioms
)]
// END - Embark standard lints v6 for Rust 1.55+
// crate-specific exceptions:
#![deny(missing_docs, rustdoc::missing_crate_level_docs)]

mod promise;

pub use promise::{Promise, Sender, TaskType};

#[cfg(feature = "smol")]
static EXECUTOR: smol::Executor<'static> = smol::Executor::new();
#[cfg(feature = "smol")]
thread_local! {
    static LOCAL_EXECUTOR: smol::LocalExecutor<'static> = smol::LocalExecutor::new();
}

/// 'Tick' the `smol` thread executor.
///
/// Poll promise will call this for you when using [`Promise::block_until_ready`] and friends.
/// If so desired [`Promise::poll`] will run this for you with the `smol_tick_poll` feature.
#[cfg(feature = "smol")]
pub fn tick() -> bool {
    crate::EXECUTOR.try_tick()
}

/// 'Tick' the `smol` local thread executor.
///
/// Poll promise will call this for you when using [`Promise::block_until_ready`] and friends.
/// If so desired [`Promise::poll`] will run this for you with the `smol_tick_poll` feature.
#[cfg(feature = "smol")]
pub fn tick_local() -> bool {
    crate::LOCAL_EXECUTOR.with(|exec| exec.try_tick())
}

#[cfg(test)]
mod test {
    use crate::Promise;

    #[test]
    fn it_spawns_threads() {
        let promise = Promise::spawn_thread("test", || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            0
        });

        assert_eq!(0, promise.block_and_take());
    }

    #[test]
    #[cfg(feature = "smol")]
    fn it_runs_async_threaded() {
        let promise = Promise::spawn_async(async move { 0 });

        assert_eq!(0, promise.block_and_take());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(feature = "tokio")]
    async fn it_runs_async_threaded() {
        let promise = Promise::spawn_async(async move { 0 });

        assert_eq!(0, promise.block_and_take());
    }

    #[test]
    #[cfg(feature = "async-std")]
    fn it_runs_async_threaded() {
        let promise = Promise::spawn_async(async move { 0 });

        assert_eq!(0, promise.block_and_take());
    }

    #[test]
    #[cfg(feature = "smol")]
    fn it_runs_locally() {
        let promise = Promise::spawn_local(async move { 0 });

        assert_eq!(0, promise.block_and_take());
    }

    #[test]
    #[cfg(feature = "smol")]
    fn it_runs_background() {
        let promise = Promise::spawn_async(async move {
            let mut e = 0;
            for i in -10000..0 {
                e += i;
            }
            e
        });
        #[cfg(not(feature = "smol_tick_poll"))]
        crate::tick();

        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(promise.ready().is_some(), "was not finished");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(feature = "tokio")]
    async fn it_runs_background() {
        let promise = Promise::spawn_async(async move {
            let mut e = 0;
            for i in -10000..0 {
                e += i;
            }
            e
        });

        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(promise.ready().is_some(), "was not finished");
    }

    #[test]
    #[cfg(feature = "async-std")]
    fn it_runs_background() {
        let promise = Promise::spawn_async(async move {
            let mut e = 0;
            for i in -10000..0 {
                e += i;
            }
            e
        });

        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(promise.ready().is_some(), "was not finished");
    }

    #[test]
    #[cfg(feature = "smol")]
    fn it_can_block() {
        let promise = Promise::spawn_local(async move {
            std::thread::sleep(std::time::Duration::from_secs(1));
        });
        #[cfg(not(feature = "smol_tick_poll"))]
        crate::tick_local();

        assert!(promise.ready().is_some(), "was not finished");
    }

    #[test]
    #[cfg(feature = "smol")]
    fn it_can_run_async_functions() {
        let promise = Promise::spawn_async(async move { something_async().await });
        #[cfg(not(feature = "smol_tick_poll"))]
        crate::tick();

        assert!(promise.block_and_take(), "example.com is ipv4");
    }
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(feature = "tokio")]
    async fn it_can_run_async_functions() {
        let promise = Promise::spawn_async(async move { something_async().await });

        assert!(promise.block_and_take(), "example.com is ipv4");
    }

    #[test]
    #[cfg(feature = "async-std")]
    fn it_can_run_async_functions() {
        let promise = Promise::spawn_async(async move { something_async().await });

        assert!(promise.block_and_take(), "example.com is ipv4");
    }

    #[test]
    #[cfg(feature = "smol")]
    fn it_can_run_async_functions_locally() {
        let promise = Promise::spawn_local(async move { something_async().await });
        #[cfg(not(feature = "smol_tick_poll"))]
        crate::tick_local();

        assert!(promise.block_and_take(), "example.com is ipv4");
    }

    #[cfg(any(feature = "smol", feature = "tokio", feature = "async-std"))]
    async fn something_async() -> bool {
        async_net::resolve("example.com:80").await.unwrap()[0].is_ipv4()
    }
}
