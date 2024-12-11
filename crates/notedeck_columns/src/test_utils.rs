use poll_promise::Promise;
use std::thread;
use std::time::Duration;

pub fn promise_wait<'a, T: Send + 'a>(promise: &'a Promise<T>) -> &'a T {
    let mut count = 1;
    loop {
        if let Some(result) = promise.ready() {
            println!("quieried promise num times: {}", count);
            return result;
        } else {
            count += 1;
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// `promise_assert` macro
///
/// This macro is designed to emulate the nature of immediate mode asynchronous code by repeatedly calling
/// promise.ready() for a promise, sleeping for a short period of time, and repeating until the promise is ready.
///
/// Arguments:
/// - `$assertion_closure`: the assertion closure which takes two arguments: the actual result of the promise and
///   the expected value. This macro is used as an assertion closure to compare the actual and expected values.
/// - `$expected`: The expected value of type `T` that the promise's result is compared against.
/// - `$asserted_promise`: A `Promise<T>` that returns a value of type `T` when the promise is satisfied. This
///   represents the asynchronous operation whose result will be tested.
///
#[macro_export]
macro_rules! promise_assert {
    ($assertion_closure:ident, $expected:expr, $asserted_promise:expr) => {
        let result = $crate::test_utils::promise_wait($asserted_promise);
        $assertion_closure!(*result, $expected);
    };
}
