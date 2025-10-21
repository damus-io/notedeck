//! A simple timer, used to enqueue operations meant to be executed at
//! a given time or after a given delay.

extern crate chrono;

use std::cmp::Ordering;
use std::thread;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex, Condvar};
use std::sync::mpsc::{channel, Sender};
use std::collections::BinaryHeap;
use chrono::{Duration, DateTime};
use chrono::offset::Utc;

/// An item scheduled for delayed execution.
struct Schedule<T> {
    /// The instant at which to execute.
    date: DateTime<Utc>,

    /// The schedule data.
    data : T,

    /// A mechanism to cancel execution of an item.
    guard: Guard,

    /// If `Some(d)`, the item must be repeated every interval of
    /// length `d`, until cancelled.
    repeat: Option<Duration>
}
impl <T> Ord for Schedule<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.date.cmp(&other.date).reverse()
    }
}
impl <T> PartialOrd for Schedule<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.date.partial_cmp(&other.date).map(|ord| ord.reverse())
    }
}
impl <T> Eq for Schedule<T> {
}
impl <T> PartialEq for Schedule<T> {
    fn eq(&self, other: &Self) -> bool {
        self.date.eq(&other.date)
    }
}

/// An operation to be sent across threads.
enum Op<T> {
    /// Schedule a new item for execution.
    Schedule(Schedule<T>),

    /// Stop the thread.
    Stop
}

/// A mutex-based kind-of-channel used to communicate between the
/// Communication thread and the Scheuler thread.
struct WaiterChannel<T> {
    /// Pending messages.
    messages: Mutex<Vec<Op<T>>>,
    /// A condition variable used for waiting.
    condvar: Condvar,
}
impl <T> WaiterChannel<T> {
    fn with_capacity(cap: usize) -> Self {
        WaiterChannel {
            messages: Mutex::new(Vec::with_capacity(cap)),
            condvar: Condvar::new(),
        }
    }
}

/// A trait that allows configurable execution of scheduled item
/// on the scheduler thread.
trait Executor<T> {
    // Due to difference in use between Box<FnMut()> and most other data
    // types, this trait requires implementors to provide two implementations
    // of execute. While both of these functions execute the data item
    // they differ on whether they make an equivalent data item available
    // to the Scheduler to store in recurring schedules.
    //
    // execute() is called whenever a non-recurring data item needs
    // to be executed, and consumes the data item in the process.
    //
    // execute_clone() is called whenever a recurring data item needs
    // to be executed, and produces a new equivalent data item. This
    // function should be more or less equivalent to:
    //
    // fn execute_clone(&mut self, data : T) -> T {
    //   self.execute(data.clone());
    //   data
    // }

    fn execute(&mut self, data : T);

    fn execute_clone(&mut self, data : T) -> T;
}

/// An executor implementation for executing callbacks on the scheduler
/// thread.
struct CallbackExecutor;

impl Executor<Box<FnMut() + Send>> for CallbackExecutor {
    fn execute(&mut self, mut data : Box<FnMut() + Send>) {
        data();
    }

    fn execute_clone(&mut self, mut data : Box<FnMut() + Send>) -> Box<FnMut() + Send> {
        data();
        data
    }
}

/// An executor implementation for delivering messages to a channel.
struct DeliveryExecutor<T>
    where T : 'static + Send {
    /// The channel to deliver messages to.
    tx : Sender<T>
}

impl <T> Executor<T> for DeliveryExecutor<T>
    where T : 'static + Send + Clone {
    fn execute(&mut self, data : T) {
        let _ = self.tx.send(data);
    }

    fn execute_clone(&mut self, data : T) -> T {
        let _ = self.tx.send(data.clone());
        data
    }
}


struct Scheduler<T,E> where E : Executor<T> {
    waiter: Arc<WaiterChannel<T>>,
    heap: BinaryHeap<Schedule<T>>,
    executor: E
}

impl <T,E> Scheduler<T,E> where E : Executor<T> {
    fn with_capacity(waiter: Arc<WaiterChannel<T>>, executor : E, capacity: usize) -> Self {
        Scheduler {
            waiter: waiter,
            executor: executor,
            heap: BinaryHeap::with_capacity(capacity),
        }
    }

    fn run(&mut self) {
        enum Sleep {
            NotAtAll,
            UntilAwakened,
            AtMost(Duration)
        }

        let ref waiter = *self.waiter;
        loop {
            let mut lock = waiter.messages.lock().unwrap();

            // Pop all messages.
            for msg in lock.drain(..) {
                match msg {
                    Op::Stop => {
                        return;
                    }
                    Op::Schedule(sched) => self.heap.push(sched),
                }
            }

            // Pop all the callbacks that are ready.

            // If we don't find
            let mut sleep = Sleep::UntilAwakened;
            loop {
                let now = Utc::now();
                if let Some(sched) = self.heap.peek() {
                    if sched.date > now {
                        // First item is not ready yet, so we need to
                        // wait until it is or something happens.
                        sleep = Sleep::AtMost(sched.date.signed_duration_since(now));
                        break;
                    }
                } else {
                    // Schedule is empty, nothing to do, wait until something happens.
                    break;
                }
                // At this stage, we have an item that has reached
                // execution time. The `unwrap()` is guaranteed to
                // succeed.
                let sched = self.heap.pop().unwrap();
                if !sched.guard.should_execute() {
                    // Execution has been cancelled, skip this item.
                    continue;
                }

                if let Some(delta) = sched.repeat {
                    let data = self.executor.execute_clone(sched.data);

                    // This is a repeating timer, so we need to
                    // enqueue the next call.
                    sleep = Sleep::NotAtAll;
                    self.heap.push(Schedule {
                        date: sched.date + delta,
                        data: data,
                        guard: sched.guard,
                        repeat: Some(delta)
                    });
                } else {
                    self.executor.execute(sched.data);
                }
            }

            match sleep {
                Sleep::UntilAwakened => {
                    let _ = waiter.condvar.wait(lock);
                },
                Sleep::AtMost(delay) => {
                    let sec = delay.num_seconds();
                    let ns = (delay - Duration::seconds(sec)).num_nanoseconds().unwrap(); // This `unwrap()` asserts that the number of ns is not > 1_000_000_000. Since we just substracted the number of seconds, the assertion should always pass.
                    let duration = std::time::Duration::new(sec as u64, ns as u32);
                    let _ = waiter.condvar.wait_timeout(lock, duration);
                },
                Sleep::NotAtAll => {}
            }
        }
    }
}

/// Shared coordination logic for timer threads.
pub struct TimerBase<T>
    where T : 'static + Send {
    /// Sender used to communicate with the _Communication_ thread. In
    /// turn, this thread will send 
    tx: Sender<Op<T>>,
}

impl <T> Drop for TimerBase<T>
    where T : 'static + Send {
    /// Stop the timer threads.
    fn drop(&mut self) {
        self.tx.send(Op::Stop).unwrap();
    }
}

impl <T> TimerBase<T>
    where T : 'static + Send {
    /// Create a timer base.
    ///
    /// This immediatey launches two threads, which will remain
    /// launched until the timer is dropped. As expected, the threads
    /// spend most of their life waiting for instructions.
    fn new<E>(executor : E) -> Self
        where E : 'static + Executor<T> + Send {
        Self::with_capacity(executor, 32)
    }

    /// As `new()`, but with a manually specified initial capaicty.
    fn with_capacity<E>(executor : E, capacity: usize) -> Self
        where E : 'static + Executor<T> + Send {
        let waiter_send = Arc::new(WaiterChannel::with_capacity(capacity));
        let waiter_recv = waiter_send.clone();

        // Spawn a first thread, whose sole role is to dispatch
        // messages to the second thread without having to wait too
        // long for the mutex.
        let (tx, rx) = channel();
        thread::spawn(move || {
            use Op::*;
            let ref waiter = *waiter_send;
            for msg in rx.iter() {
                let mut vec = waiter.messages.lock().unwrap();
                match msg {
                    Schedule(sched) => {
                        vec.push(Schedule(sched));
                        waiter.condvar.notify_one();
                    }
                    Stop => {
                        vec.clear();
                        vec.push(Op::Stop);
                        waiter.condvar.notify_one();
                        return;
                    }
                }
            }
        });

        // Spawn a second thread, in charge of scheduling.
        thread::Builder::new().name("Timer thread".to_owned()).spawn(move || {
            let mut scheduler = Scheduler::with_capacity(waiter_recv, executor, capacity);
            scheduler.run()
        }).unwrap();
        TimerBase {
            tx: tx
        }
    }

    pub fn schedule_with_delay(&self, delay: Duration, data : T) -> Guard {
        self.schedule_with_date(Utc::now() + delay, data)
    }

    pub fn schedule_with_date<D>(&self, date: DateTime<D>, data : T) -> Guard
        where D : chrono::offset::TimeZone
    {
        self.schedule(date, None, data)
    }

    pub fn schedule_repeating(&self, repeat: Duration, data : T) -> Guard
    {
        self.schedule(Utc::now() + repeat, Some(repeat), data)
    }

    pub fn schedule<D>(&self, date: DateTime<D>, repeat: Option<Duration>, data : T) -> Guard
        where D : chrono::offset::TimeZone
    {
        let guard = Guard::new();
        self.tx.send(Op::Schedule(Schedule {
            date: date.with_timezone(&Utc),
            data: data,
            guard: guard.clone(),
            repeat: repeat
        })).unwrap();
        guard
    }
}

/// A timer, used to schedule execution of callbacks at a later date.
///
/// In the current implementation, each timer is executed as two
/// threads. The _Scheduler_ thread is in charge of maintaining the
/// queue of callbacks to execute and of actually executing them. The
/// _Communication_ thread is in charge of communicating with the
/// _Scheduler_ thread (which requires acquiring a possibly-long-held
/// Mutex) without blocking the caller thread.
pub struct Timer {
    base: TimerBase<Box<FnMut() + Send>>
}

impl Timer {
    /// Create a timer.
    ///
    /// This immediatey launches two threads, which will remain
    /// launched until the timer is dropped. As expected, the threads
    /// spend most of their life waiting for instructions.
    pub fn new() -> Self {
        Timer { base : TimerBase::new(CallbackExecutor) }
    }

    /// As `new()`, but with a manually specified initial capaicty.
    pub fn with_capacity(capacity: usize) -> Self {
        Timer { base : TimerBase::with_capacity(CallbackExecutor, capacity) }
    }

    /// Schedule a callback for execution after a delay.
    ///
    /// Callbacks are guaranteed to never be called before the
    /// delay. However, it is possible that they will be called a
    /// little after the delay.
    ///
    /// If the delay is negative or 0, the callback is executed as
    /// soon as possible.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, execution is cancelled.
    ///
    /// # Performance
    ///
    /// The callback is executed on the Scheduler thread. It should
    /// therefore terminate very quickly, or risk causing delaying
    /// other callbacks.
    ///
    /// # Failures
    ///
    /// Any failure in `cb` will scheduler thread and progressively
    /// contaminate the Timer and the calling thread itself. You have
    /// been warned.
    ///
    /// # Example
    ///
    /// ```
    /// extern crate timer;
    /// extern crate chrono;
    /// use std::sync::mpsc::channel;
    ///
    /// let timer = timer::Timer::new();
    /// let (tx, rx) = channel();
    ///
    /// let _guard = timer.schedule_with_delay(chrono::Duration::seconds(3), move || {
    ///   // This closure is executed on the scheduler thread,
    ///   // so we want to move it away asap.
    ///
    ///   let _ignored = tx.send(()); // Avoid unwrapping here.
    /// });
    ///
    /// rx.recv().unwrap();
    /// println!("This code has been executed after 3 seconds");
    /// ```
    pub fn schedule_with_delay<F>(&self, delay: Duration, cb: F) -> Guard
        where F: 'static + FnMut() + Send {
        self.base.schedule_with_delay(delay, Box::new(cb))
    }

    /// Schedule a callback for execution at a given date.
    ///
    /// Callbacks are guaranteed to never be called before their
    /// date. However, it is possible that they will be called a
    /// little after it.
    ///
    /// If the date is in the past, the callback is executed as soon
    /// as possible.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, execution is cancelled.
    ///
    ///
    /// # Performance
    ///
    /// The callback is executed on the Scheduler thread. It should
    /// therefore terminate very quickly, or risk causing delaying
    /// other callbacks.
    ///
    /// # Failures
    ///
    /// Any failure in `cb` will scheduler thread and progressively
    /// contaminate the Timer and the calling thread itself. You have
    /// been warned.
    pub fn schedule_with_date<F, T>(&self, date: DateTime<T>, cb: F) -> Guard
        where F: 'static + FnMut() + Send, T : chrono::offset::TimeZone
    {
        self.base.schedule_with_date(date, Box::new(cb))
    }

    /// Schedule a callback for execution once per interval.
    ///
    /// Callbacks are guaranteed to never be called before their
    /// date. However, it is possible that they will be called a
    /// little after it.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, repeat is stopped.
    ///
    ///
    /// # Performance
    ///
    /// The callback is executed on the Scheduler thread. It should
    /// therefore terminate very quickly, or risk causing delaying
    /// other callbacks.
    ///
    /// # Failures
    ///
    /// Any failure in `cb` will scheduler thread and progressively
    /// contaminate the Timer and the calling thread itself. You have
    /// been warned.
    ///
    /// # Example
    ///
    /// ```
    /// extern crate timer;
    /// extern crate chrono;
    /// use std::thread;
    /// use std::sync::{Arc, Mutex};
    ///
    /// let timer = timer::Timer::new();
    /// // Number of times the callback has been called.
    /// let count = Arc::new(Mutex::new(0));
    ///
    /// // Start repeating. Each callback increases `count`.
    /// let guard = {
    ///   let count = count.clone();
    ///   timer.schedule_repeating(chrono::Duration::milliseconds(5), move || {
    ///     *count.lock().unwrap() += 1;
    ///   })
    /// };
    ///
    /// // Sleep one second. The callback should be called ~200 times.
    /// thread::sleep(std::time::Duration::new(1, 0));
    /// let count_result = *count.lock().unwrap();
    /// assert!(190 <= count_result && count_result <= 210,
    ///   "The timer was called {} times", count_result);
    ///
    /// // Now drop the guard. This should stop the timer.
    /// drop(guard);
    /// thread::sleep(std::time::Duration::new(0, 100));
    ///
    /// // Let's check that the count stops increasing.
    /// let count_start = *count.lock().unwrap();
    /// thread::sleep(std::time::Duration::new(1, 0));
    /// let count_stop =  *count.lock().unwrap();
    /// assert_eq!(count_start, count_stop);
    /// ```
    pub fn schedule_repeating<F>(&self, repeat: Duration, cb: F) -> Guard
        where F: 'static + FnMut() + Send
    {
        self.base.schedule_repeating(repeat, Box::new(cb))
    }

    /// Schedule a callback for execution at a given time, then once
    /// per interval. A typical use case is to execute code once per
    /// day at 12am.
    ///
    /// Callbacks are guaranteed to never be called before their
    /// date. However, it is possible that they will be called a
    /// little after it.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, repeat is stopped.
    ///
    ///
    /// # Performance
    ///
    /// The callback is executed on the Scheduler thread. It should
    /// therefore terminate very quickly, or risk causing delaying
    /// other callbacks.
    ///
    /// # Failures
    ///
    /// Any failure in `cb` will scheduler thread and progressively
    /// contaminate the Timer and the calling thread itself. You have
    /// been warned.
    pub fn schedule<F, T>(&self, date: DateTime<T>, repeat: Option<Duration>, cb: F) -> Guard
        where F: 'static + FnMut() + Send, T : chrono::offset::TimeZone
    {
        self.base.schedule(date, repeat, Box::new(cb))
    }
}

/// A timer, used to schedule delivery of messages at a later date.
///
/// In the current implementation, each timer is executed as two
/// threads. The _Scheduler_ thread is in charge of maintaining the
/// queue of messages to deliver and of actually deliverying them. The
/// _Communication_ thread is in charge of communicating with the
/// _Scheduler_ thread (which requires acquiring a possibly-long-held
/// Mutex) without blocking the caller thread.
///
/// Similar functionality could be implemented using the generic Timer
/// type, however, using MessageTimer has two performance advantages
/// over doing so. First, MessageTimer does not need to heap allocate
/// a closure for each scheduled item, since the messages to queue are
/// passed directly. Second, MessageTimer avoids the dynamic dispatch
/// overhead associated with invoking the closure functions.
pub struct MessageTimer<T>
    where T : 'static + Send + Clone {
    base: TimerBase<T>
}

impl <T> MessageTimer<T>
    where T : 'static + Send + Clone {
    /// Create a message timer.
    ///
    /// This immediatey launches two threads, which will remain
    /// launched until the timer is dropped. As expected, the threads
    /// spend most of their life waiting for instructions.
    pub fn new(tx: Sender<T>) -> Self {
        MessageTimer {
            base : TimerBase::new(DeliveryExecutor { tx : tx })
        }
    }

    /// As `new()`, but with a manually specified initial capaicty.
    pub fn with_capacity(tx: Sender<T>, capacity: usize) -> Self {
        MessageTimer {
            base : TimerBase::with_capacity(DeliveryExecutor { tx : tx }, capacity)
        }
    }

    /// Schedule a message for delivery after a delay.
    ///
    /// Messages are guaranteed to never be delivered before the
    /// delay. However, it is possible that they will be delivered a
    /// little after the delay.
    ///
    /// If the delay is negative or 0, the message is delivered as
    /// soon as possible.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, delivery is cancelled.
    ///
    ///
    /// # Example
    ///
    /// ```
    /// extern crate timer;
    /// extern crate chrono;
    /// use std::sync::mpsc::channel;
    ///
    /// let (tx, rx) = channel();
    /// let timer = timer::MessageTimer::new(tx);
    /// let _guard = timer.schedule_with_delay(chrono::Duration::seconds(3), 3);
    ///
    /// rx.recv().unwrap();
    /// println!("This code has been executed after 3 seconds");
    /// ```
    pub fn schedule_with_delay(&self, delay: Duration, msg : T) -> Guard {
        self.base.schedule_with_delay(delay, msg)
    }

    /// Schedule a message for delivery at a given date.
    ///
    /// Messages are guaranteed to never be delivered before their
    /// date. However, it is possible that they will be delivered a
    /// little after it.
    ///
    /// If the date is in the past, the message is delivered as soon
    /// as possible.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, delivery is cancelled.
    ///
    pub fn schedule_with_date<D>(&self, date: DateTime<D>, msg : T) -> Guard
        where D : chrono::offset::TimeZone
    {
        self.base.schedule_with_date(date, msg)
    }

    /// Schedule a message for delivery once per interval.
    ///
    /// Messages are guaranteed to never be delivered before their
    /// date. However, it is possible that they will be delivered a
    /// little after it.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, repeat is stopped.
    ///
    ///
    /// # Performance
    ///
    /// The message is cloned on the Scheduler thread. Cloning of
    /// messages should therefore succeed very quickly, or risk
    /// delaying other messages.
    ///
    /// # Failures
    ///
    /// Any failure in cloning of messages will occur on the scheduler thread
    /// and will contaminate the Timer and the calling thread itself. You have
    /// been warned.
    ///
    /// # Example
    ///
    /// ```
    /// extern crate timer;
    /// extern crate chrono;
    /// use std::sync::mpsc::channel;
    ///
    /// let (tx, rx) = channel();
    /// let timer = timer::MessageTimer::new(tx);
    ///
    /// // Start repeating.
    /// let guard = timer.schedule_repeating(chrono::Duration::milliseconds(5), 0);
    ///
    /// let mut count = 0;
    /// while count < 5 {
    ///   let _ = rx.recv();
    ///   println!("Prints every 5 milliseconds");
    ///   count += 1;
    /// }
    /// ```
    pub fn schedule_repeating(&self, repeat: Duration, msg : T) -> Guard
    {
        self.base.schedule_repeating(repeat, msg)
    }

    /// Schedule a message for delivery at a given time, then once
    /// per interval. A typical use case is to execute code once per
    /// day at 12am.
    ///
    /// Messages are guaranteed to never be delivered before their
    /// date. However, it is possible that they will be delivered a
    /// little after it.
    ///
    /// This method returns a `Guard` object. If that `Guard` is
    /// dropped, repeat is stopped.
    ///
    /// # Performance
    ///
    /// The message is cloned on the Scheduler thread. Cloning of
    /// messages should therefore succeed very quickly, or risk
    /// delaying other messages.
    ///
    /// # Failures
    ///
    /// Any failure in cloning of messages will occur on the scheduler thread
    /// and will contaminate the Timer and the calling thread itself. You have
    /// been warned.
    pub fn schedule<D>(&self, date: DateTime<D>, repeat: Option<Duration>, msg : T) -> Guard
        where D : chrono::offset::TimeZone
    {
        self.base.schedule(date, repeat, msg)
    }
}

/// A value scoping a schedule. When this value is dropped, the
/// schedule is cancelled.
#[derive(Clone)]
pub struct Guard {
    should_execute: Arc<AtomicBool>,
    ignore_drop: bool
}
impl Guard {
    fn new() -> Self {
        Guard {
            should_execute: Arc::new(AtomicBool::new(true)),
            ignore_drop: false
        }
    }
    fn should_execute(&self) -> bool {
        self.should_execute.load(AtomicOrdering::Relaxed)
    }

    /// Ignores the guard, preventing it from disabling the scheduled
    /// item. This can be used to avoid maintaining a Guard handle
    /// for items that will never be cancelled.
    pub fn ignore(mut self) {
        self.ignore_drop = true;
    }
}
impl Drop for Guard {
    /// Cancel a schedule.
    fn drop(&mut self) {
        if !self.ignore_drop {
            self.should_execute.store(false, AtomicOrdering::Relaxed)
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use chrono::{Duration, Utc};

    #[test]
    fn test_schedule_with_delay() {
        let timer = Timer::new();
        let (tx, rx) = channel();
        let mut guards = vec![];

        // Schedule a number of callbacks in an arbitrary order, make sure
        // that they are executed in the right order.
        let mut delays = vec![1, 5, 3, -1];
        let start = Utc::now();
        for i in delays.clone() {
            println!("Scheduling for execution in {} seconds", i);
            let tx = tx.clone();
            guards.push(timer.schedule_with_delay(Duration::seconds(i), move || {
                println!("Callback {}", i);
                tx.send(i).unwrap();
            }));
        }

        delays.sort();
        for (i, msg) in (0..delays.len()).zip(rx.iter()) {
            let elapsed = Utc::now().signed_duration_since(start).num_seconds();
            println!("Received message {} after {} seconds", msg, elapsed);
            assert_eq!(msg, delays[i]);
            assert!(delays[i] <= elapsed && elapsed <= delays[i] + 3, "We have waited {} seconds, expecting [{}, {}]", elapsed, delays[i], delays[i] + 3);
        }

        // Now make sure that callbacks that are designed to be executed
        // immediately are executed quickly.
        let start = Utc::now();
        for i in vec![10, 0] {
            println!("Scheduling for execution in {} seconds", i);
            let tx = tx.clone();
            guards.push(timer.schedule_with_delay(Duration::seconds(i), move || {
                println!("Callback {}", i);
                tx.send(i).unwrap();
            }));
        }

        assert_eq!(rx.recv().unwrap(), 0);
        assert!(Utc::now().signed_duration_since(start) <= Duration::seconds(1));
    }

    #[test]
    fn test_message_timer() {
        let (tx, rx) = channel();
        let timer = MessageTimer::new(tx);
        let start = Utc::now();

        let mut delays = vec!(400, 300, 100, 500, 200);
        for delay in delays.clone() {
            timer.schedule_with_delay(Duration::milliseconds(delay), delay).ignore();
        }

        delays.sort();
        for delay in delays {
            assert_eq!(rx.recv().unwrap(), delay);
        }
        assert!(Utc::now().signed_duration_since(start) <= Duration::seconds(1));
    }

    #[test]
    fn test_guards() {
        println!("Testing that callbacks aren't called if the guard is dropped");
        let timer = Timer::new();
        let called = Arc::new(Mutex::new(false));

        for i in 0..10 {
            let called = called.clone();
            timer.schedule_with_delay(Duration::milliseconds(i), move || {
                *called.lock().unwrap() = true;
            });
        }

        thread::sleep(std::time::Duration::new(1, 0));
        assert_eq!(*called.lock().unwrap(), false);
    }

    #[test]
    fn test_guard_ignore() {
        let timer = Timer::new();
        let called = Arc::new(Mutex::new(false));

        {
            let called = called.clone();
            timer.schedule_with_delay(Duration::milliseconds(1), move || {
                *called.lock().unwrap() = true;
            }).ignore();
        }

        thread::sleep(std::time::Duration::new(1, 0));
        assert_eq!(*called.lock().unwrap(), true);
    }

    struct NoCloneMessage;

    impl Clone for NoCloneMessage {
        fn clone(&self) -> Self {
            panic!("TestMessage should not be cloned");
        }
    }

    #[test]
    fn test_no_clone() {
        // Make sure that, if no schedule is supplied to a MessageTimer
        // the message instances are not cloned.
        let (tx, rx) = channel();
        let timer = MessageTimer::new(tx);
        timer.schedule_with_delay(Duration::milliseconds(0), NoCloneMessage).ignore();
        timer.schedule_with_delay(Duration::milliseconds(0), NoCloneMessage).ignore();
        
        for _ in 0..2 {
            let _  = rx.recv();
        }
    }
}
