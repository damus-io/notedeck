// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use super::{Filter, Observe};

pub trait Observable<Event>
where
    Event: Clone + 'static + Send,
{
    /// The error type that is returned if observing is not possible.
    ///
    /// [Pharos](crate::Pharos) implements
    /// [Sink](https://docs.rs/futures-preview/0.3.0-alpha.19/futures/sink/trait.Sink.html)
    /// which has a close method, so observing will no longer be possible after close is called.
    ///
    /// Other than that, you might want to have moments in your objects lifetime when you don't want to take
    /// any more observers. Returning a result from [observe](Observable::observe) enables that.
    ///
    /// You can of course map the error of pharos to your own error type.
    type Error: std::error::Error;

    /// Add an observer to the observable. Options allow chosing the channel type and
    /// to filter events with a predicate.
    fn observe(&mut self, options: ObserveConfig<Event>) -> Observe<'_, Event, Self::Error>;
}

#[derive(Debug)]
pub struct ObserveConfig<Event>
where
    Event: Clone + 'static + Send,
{
    pub(crate) filter: Option<Filter<Event>>,
}

/// Create a default configuration:
/// - no filter
/// - an unbounded channel
impl<Event> Default for ObserveConfig<Event>
where
    Event: Clone + 'static + Send,
{
    fn default() -> Self {
        Self { filter: None }
    }
}

/// Create a [ObserveConfig] from a [Filter], getting default values for other options.
impl<Event> From<Filter<Event>> for ObserveConfig<Event>
where
    Event: Clone + 'static + Send,
{
    fn from(filter: Filter<Event>) -> Self {
        Self {
            filter: Some(filter),
        }
    }
}
