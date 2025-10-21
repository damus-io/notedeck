// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use std::any::type_name;
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::channel::mpsc::{
    self, UnboundedReceiver as FutUnboundedReceiver, UnboundedSender as FutUnboundedSender,
};
use futures::{Sink, Stream};

use super::{ErrorKind, Filter, ObserveConfig, PharErr};

/// A stream of events. This is returned from [Observable::observe](crate::Observable::observe).
/// You will only start receiving events from the moment you call this. Any events in the observed
/// object emitted before will not be delivered.
#[derive(Debug)]
pub struct Events<Event>
where
    Event: Clone + 'static + Send,
{
    rx: Receiver<Event>,
}

impl<Event> Events<Event>
where
    Event: Clone + 'static + Send,
{
    pub(crate) fn new(config: ObserveConfig<Event>) -> (Self, Sender<Event>) {
        let (tx, rx) = mpsc::unbounded();
        (
            Self {
                rx: Receiver { rx },
            },
            Sender {
                tx,
                filter: config.filter,
            },
        )
    }
}

// Just forward
impl<Event> Stream for Events<Event>
where
    Event: Clone + 'static + Send,
{
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.rx).poll_next(cx)
    }
}

/// The sender of the channel
pub(crate) struct Sender<Event>
where
    Event: Clone + 'static + Send,
{
    tx: FutUnboundedSender<Event>,
    filter: Option<Filter<Event>>,
}

impl<Event> Sender<Event>
where
    Event: Clone + 'static + Send,
{
    // Verify whether this observer is still around.
    #[inline]
    pub(crate) fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Check whether this sender is interested in this event.
    #[inline]
    pub(crate) fn filter(&mut self, evt: &Event) -> bool {
        Self::filter_inner(&mut self.filter, evt)
    }

    fn filter_inner(filter: &mut Option<Filter<Event>>, evt: &Event) -> bool {
        match filter {
            Some(f) => f.call(evt),
            None => true,
        }
    }
}

/// The receiver of the channel, abstracting over different channel types.
struct Receiver<Event>
where
    Event: Clone + 'static + Send,
{
    rx: FutUnboundedReceiver<Event>,
}

impl<Event> fmt::Debug for Receiver<Event>
where
    Event: 'static + Clone + Send,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PharosReceiver<{}>", type_name::<Event>())
    }
}

impl<Event> Stream for Receiver<Event>
where
    Event: Clone + 'static + Send,
{
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let rx = self.get_mut();
        Pin::new(&mut rx.rx).poll_next(cx)
    }
}

impl<Event> Sink<Event> for Sender<Event>
where
    Event: Clone + 'static + Send,
{
    type Error = PharErr;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let tx = self.get_mut();
        Pin::new(&mut tx.tx).poll_ready(cx).map_err(Into::into)
    }

    fn start_send(self: Pin<&mut Self>, item: Event) -> Result<(), Self::Error> {
        let tx = self.get_mut();
        Pin::new(&mut tx.tx).start_send(item).map_err(Into::into)
    }

    // Note that on futures-rs bounded channels poll_flush has a problematic implementation.
    // - it just calls poll_ready, which means it will be pending when the buffer is full. So
    //   it will make SinkExt::send hang, bad!
    // - it will swallow disconnected errors, so we don't get feedback allowing us to free slots.
    //
    // In principle channels are always flushed, because when the message is in the buffer, it's
    // ready for the reader to read. So this should just be a noop.
    //
    // We compensate for the error swallowing by checking `is_closed`.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.tx.is_closed() {
            Poll::Ready(Err(ErrorKind::Closed.into()))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let tx = self.get_mut();
        Pin::new(&mut tx.tx).poll_close(cx).map_err(Into::into)
    }
}

#[cfg(test)]
//
mod tests {
    use super::*;

    #[test]
    //
    fn debug() {
        let e = Events::<bool>::new(ObserveConfig::default());

        assert_eq!(
            "Events { rx: pharos::events::Receiver::<bool>::Unbounded(_) }",
            &format!("{:?}", e.0)
        );
    }
}
