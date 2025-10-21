// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use std::any::type_name;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::FutureExt;
use futures::{ready, Sink};

mod error;
mod events;
mod filter;
mod observable;
mod shared;

pub use self::error::{ErrorKind, PharErr};
pub use self::events::Events;
use self::events::Sender;
pub use self::filter::Filter;
pub use self::observable::{Observable, ObserveConfig};
pub use self::shared::SharedPharos;

/// A pinned boxed future returned by the Observable::observe method.
pub type Observe<'a, Event, Error> =
    Pin<Box<dyn Future<Output = Result<Events<Event>, Error>> + 'a + Send>>;

/// The Pharos lighthouse. When you implement [Observable] on your type, you can forward
/// the [`observe`](Observable::observe) method to Pharos and use [SinkExt::send](https://docs.rs/futures-preview/0.3.0-alpha.19/futures/sink/trait.SinkExt.html#method.send) to notify observers.
///
/// You can of course create several `Pharos` (I know, historical sacrilege) for (different) types
/// of events.
///
/// Please see the docs for [Observable] for an example. Others can be found in the README and
/// the [examples](https://github.com/najamelan/pharos/tree/master/examples) directory of the repository.
///
/// ## Implementation.
///
/// Currently just holds a `Vec<Option<Sender>>`. It will drop observers if the channel has
/// returned an error, which means it is closed or disconnected. However, we currently don't
/// compact the vector. Slots are reused for new observers, but the vector never shrinks.
///
/// **Note**: we only detect that observers can be removed when [SinkExt::send](https://docs.rs/futures-preview/0.3.0-alpha.19/futures/sink/trait.SinkExt.html#method.send) or [Pharos::num_observers]
/// is being called. Otherwise, we won't find out about disconnected observers and the vector of observers
/// will not mark deleted observers and thus their slots can not be reused.
///
/// The [Sink](https://docs.rs/futures-preview/0.3.0-alpha.19/futures/sink/trait.Sink.html) impl
/// is not very optimized for the moment. It just loops over all observers in each poll method
/// so it will call `poll_ready` and `poll_flush` again for observers that already returned `Poll::Ready(Ok(()))`.
///
/// TODO: I will do some benchmarking and see if this can be improved, eg. by keeping a state which tracks which
/// observers we still have to poll.
pub struct Pharos<Event>
where
    Event: 'static + Clone + Send,
{
    // Observers never get moved. Their index stays stable, so that when we free a slot,
    // we can store that in `free_slots`.
    observers: Vec<Option<Sender<Event>>>,
    free_slots: Vec<usize>,
    closed: bool,
}

impl<Event> fmt::Debug for Pharos<Event>
where
    Event: 'static + Clone + Send,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pharos<{}>", type_name::<Event>())
    }
}

impl<Event> Pharos<Event>
where
    Event: 'static + Clone + Send,
{
    /// Create a new Pharos. May it's light guide you to safe harbor.
    ///
    /// You can set the initial capacity of the vector of observers, if you know you will a lot of observers
    /// it will save allocations by setting this to a higher number.
    pub fn new(capacity: usize) -> Self {
        Self {
            observers: Vec::with_capacity(capacity),
            free_slots: Vec::with_capacity(capacity),
            closed: false,
        }
    }
}

/// Creates a new pharos, using 10 as the initial capacity of the vector used to store
/// observers. If this number does really not fit your use case, call [Pharos::new].
impl<Event> Default for Pharos<Event>
where
    Event: 'static + Clone + Send,
{
    fn default() -> Self {
        Self::new(10)
    }
}

impl<Event> Observable<Event> for Pharos<Event>
where
    Event: 'static + Clone + Send,
{
    type Error = PharErr;

    /// Will re-use slots from disconnected observers to avoid growing to much.
    ///
    /// TODO: provide API for the client to compact the pharos object after reducing the
    ///       number of observers.
    fn observe(&mut self, options: ObserveConfig<Event>) -> Observe<'_, Event, Self::Error> {
        async move {
            if self.closed {
                return Err(ErrorKind::Closed.into());
            }

            let (events, sender) = Events::new(options);

            // Try to reuse a free slot
            if let Some(i) = self.free_slots.pop() {
                self.observers[i] = Some(sender);
            } else {
                self.observers.push(Some(sender));
            }

            Ok(events)
        }
        .boxed()
    }
}

// See the documentation on Channel for how poll functions work for the channels we use.
//
impl<Event> Sink<Event> for Pharos<Event>
where
    Event: Clone + 'static + Send,
{
    type Error = PharErr;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.closed {
            return Err(ErrorKind::Closed.into()).into();
        }

        // As soon as any is not ready, we are not ready.
        //
        // This is a false warning AFAICT. We need to set obs
        // to None at the end, which is not possible if we have flattened the iterator.
        #[allow(clippy::manual_flatten)]
        for obs in self.get_mut().observers.iter_mut() {
            if let Some(ref mut o) = obs {
                let res = ready!(Pin::new(o).poll_ready(cx));

                // Errors mean disconnected, so drop.
                if res.is_err() {
                    // TODO: why don't we add to free_slots here like below?
                    *obs = None;
                }
            }
        }

        Ok(()).into()
    }

    fn start_send(self: Pin<&mut Self>, evt: Event) -> Result<(), Self::Error> {
        if self.closed {
            return Err(ErrorKind::Closed.into());
        }

        let this = self.get_mut();

        for (i, opt) in this.observers.iter_mut().enumerate() {
            // if this spot in the vector has a sender
            if let Some(obs) = opt {
                // if it's closed, let's remove it.
                if obs.is_closed() {
                    this.free_slots.push(i);

                    *opt = None;
                } else if obs.filter(&evt) {
                    // if sending fails, remove it
                    if Pin::new(obs).start_send(evt.clone()).is_err() {
                        this.free_slots.push(i);
                        *opt = None;
                    }
                }
            }
        }

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.closed {
            return Err(ErrorKind::Closed.into()).into();
        }

        // We loop over all, polling them all. If any return pending, we return pending.
        // If any return an error, we drop them.
        //
        let mut pending = false;
        let this = self.get_mut();

        for (i, opt) in this.observers.iter_mut().enumerate() {
            if let Some(ref mut obs) = opt {
                match Pin::new(obs).poll_flush(cx) {
                    Poll::Pending => pending = true,
                    Poll::Ready(Ok(_)) => continue,

                    Poll::Ready(Err(_)) => {
                        this.free_slots.push(i);

                        *opt = None;
                    }
                }
            }
        }

        if pending {
            Poll::Pending
        } else {
            Ok(()).into()
        }
    }

    /// Will close and drop all observers.
    //
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.closed {
            return Ok(()).into();
        } else {
            self.closed = true;
        }

        let this = self.get_mut();

        for (i, opt) in this.observers.iter_mut().enumerate() {
            if let Some(ref mut obs) = opt {
                let res = ready!(Pin::new(obs).poll_close(cx));

                if res.is_err() {
                    this.free_slots.push(i);

                    *opt = None;
                }
            }
        }

        Ok(()).into()
    }
}
