// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use std::any::type_name;
use std::fmt;

pub enum Filter<Event>
where
    Event: Clone + 'static + Send,
{
    /// A function pointer to a predicate to filter events.
    Pointer(fn(&Event) -> bool),
}

impl<Event> Filter<Event>
where
    Event: Clone + 'static + Send,
{
    /// Invoke the predicate.
    pub(crate) fn call(&mut self, evt: &Event) -> bool {
        match self {
            Self::Pointer(f) => f(evt),
        }
    }
}

impl<Event> fmt::Debug for Filter<Event>
where
    Event: Clone + 'static + Send,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pointer(_) => write!(f, "pharos::Filter<{}>::Pointer(_)", type_name::<Event>()),
        }
    }
}
