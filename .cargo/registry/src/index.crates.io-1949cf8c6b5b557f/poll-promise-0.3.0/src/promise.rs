use std::cell::UnsafeCell;

/// Used to send a result to a [`Promise`].
///
/// You must call [`Self::send`] with a value eventually.
///
/// If you drop the `Sender` without putting a value into it,
/// it will cause the connected [`Promise`] to panic when polled.
#[must_use = "You should call Sender::send with the result"]
pub struct Sender<T>(std::sync::mpsc::Sender<T>);

impl<T> Sender<T> {
    /// Send the result to the [`Promise`].
    ///
    /// If the [`Promise`] has dropped, this does nothing.
    pub fn send(self, value: T) {
        self.0.send(value).ok(); // We ignore the error caused by the receiver being dropped.
    }
}

/// The type of a running task.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum TaskType {
    /// This task is running in the local thread.
    Local,
    /// This task is running async in another thread.
    Async,
    /// This task is running in a different manner.
    None,
}

// ----------------------------------------------------------------------------

/// A promise that waits for the reception of a single value,
/// presumably from some async task.
///
/// A `Promise` starts out waiting for a value.
/// Each time you call a member method it will check if that value is ready.
/// Once ready, the `Promise` will store the value until you drop the `Promise`.
///
/// Example:
///
/// ```
/// # fn something_slow() {}
/// # use poll_promise::Promise;
/// #
/// let promise = Promise::spawn_thread("slow_operation", move || something_slow());
///
/// // Then in the game loop or immediate mode GUI code:
/// if let Some(result) = promise.ready() {
///     // Use/show result
/// } else {
///     // Show a loading screen
/// }
/// ```
///
/// If you enable the `tokio` feature you can use `poll-promise` with the [tokio](https://github.com/tokio-rs/tokio)
/// runtime to run `async` tasks using [`Promise::spawn_async`], [`Promise::spawn_local`], and [`Promise::spawn_blocking`].
#[must_use]
pub struct Promise<T: Send + 'static> {
    data: PromiseImpl<T>,
    task_type: TaskType,

    #[cfg(feature = "tokio")]
    join_handle: Option<tokio::task::JoinHandle<()>>,

    #[cfg(feature = "smol")]
    smol_task: Option<smol::Task<()>>,

    #[cfg(feature = "async-std")]
    async_std_join_handle: Option<async_std::task::JoinHandle<()>>,
}

#[cfg(all(
    not(docsrs),
    any(
        all(feature = "tokio", feature = "smol"),
        all(feature = "tokio", feature = "async-std"),
        all(feature = "tokio", feature = "web"),
        all(feature = "smol", feature = "async-std"),
        all(feature = "smol", feature = "web"),
        all(feature = "async-std", feature = "web"),
    )
))]
compile_error!(
    "You can only specify one of the executor features: 'tokio', 'smol', 'async-std' or 'web'"
);

// Ensure that Promise is !Sync, confirming the safety of the unsafe code.
static_assertions::assert_not_impl_all!(Promise<u32>: Sync);
static_assertions::assert_impl_all!(Promise<u32>: Send);

impl<T: Send + 'static> Promise<T> {
    /// Create a [`Promise`] and a corresponding [`Sender`].
    ///
    /// Put the promised value into the sender when it is ready.
    /// If you drop the `Sender` without putting a value into it,
    /// it will cause a panic when polling the `Promise`.
    ///
    /// See also [`Self::spawn_blocking`], [`Self::spawn_async`], [`Self::spawn_local`], and [`Self::spawn_thread`].
    pub fn new() -> (Sender<T>, Self) {
        // We need a channel that we can wait blocking on (for `Self::block_until_ready`).
        // (`tokio::sync::oneshot` does not support blocking receive).
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Sender(tx),
            Self {
                data: PromiseImpl(UnsafeCell::new(PromiseStatus::Pending(rx))),
                task_type: TaskType::None,

                #[cfg(feature = "tokio")]
                join_handle: None,

                #[cfg(feature = "async-std")]
                async_std_join_handle: None,

                #[cfg(feature = "smol")]
                smol_task: None,
            },
        )
    }

    /// Create a promise that already has the result.
    pub fn from_ready(value: T) -> Self {
        Self {
            data: PromiseImpl(UnsafeCell::new(PromiseStatus::Ready(value))),
            task_type: TaskType::None,

            #[cfg(feature = "tokio")]
            join_handle: None,

            #[cfg(feature = "async-std")]
            async_std_join_handle: None,

            #[cfg(feature = "smol")]
            smol_task: None,
        }
    }

    /// Spawn a future. Runs the task concurrently.
    ///
    /// See [`Self::spawn_local`].
    ///
    /// You need to compile `poll-promise` with the "tokio" feature for this to be available.
    ///
    /// ## tokio
    /// This should be used for spawning asynchronous work that does _not_ do any heavy CPU computations
    /// as that will block other spawned tasks and will delay them. For example network IO, timers, etc.
    ///
    /// These type of future can have manually blocking code within it though, but has to then manually use
    /// [`tokio::task::block_in_place`](https://docs.rs/tokio/1.15.0/tokio/task/fn.block_in_place.html) on that,
    /// or `.await` that future.
    ///
    /// If you have a function or closure that you just want to offload to processed in the background, use the [`Self::spawn_blocking`] function instead.
    ///
    /// See the [tokio docs](https://docs.rs/tokio/1.15.0/tokio/index.html#cpu-bound-tasks-and-blocking-code) for more details about
    /// CPU-bound tasks vs async IO tasks.
    ///
    /// This is a convenience method, using [`Self::new`] with [`tokio::task::spawn`].
    ///
    /// ## Example
    /// ``` no_run
    /// # async fn something_async() {}
    /// # use poll_promise::Promise;
    /// let promise = Promise::spawn_async(async move { something_async().await });
    /// ```
    #[cfg(any(feature = "tokio", feature = "smol", feature = "async-std"))]
    pub fn spawn_async(future: impl std::future::Future<Output = T> + 'static + Send) -> Self {
        let (sender, mut promise) = Self::new();
        promise.task_type = TaskType::Async;

        #[cfg(feature = "tokio")]
        {
            promise.join_handle =
                Some(tokio::task::spawn(async move { sender.send(future.await) }));
        }

        #[cfg(feature = "smol")]
        {
            promise.smol_task =
                Some(crate::EXECUTOR.spawn(async move { sender.send(future.await) }));
        }

        #[cfg(feature = "async-std")]
        {
            promise.async_std_join_handle =
                Some(async_std::task::spawn(
                    async move { sender.send(future.await) },
                ));
        }

        promise
    }

    /// Spawn a future. Runs it in the local thread.
    ///
    /// You need to compile `poll-promise` with either the "tokio", "smol", or "web" feature for this to be available.
    ///
    /// This is a convenience method, using [`Self::new`] with [`tokio::task::spawn_local`].
    /// Unlike [`Self::spawn_async`] this method does not require [`Send`].
    /// However, you will have to set up [`tokio::task::LocalSet`]s yourself.
    ///
    /// ## Example
    /// ``` no_run
    /// # async fn something_async() {}
    /// # use poll_promise::Promise;
    /// let promise = Promise::spawn_local(async move { something_async().await });
    /// ```
    #[cfg(any(feature = "tokio", feature = "web", feature = "smol"))]
    pub fn spawn_local(future: impl std::future::Future<Output = T> + 'static) -> Self {
        // When using the web feature we don't mutate promise.
        #[allow(unused_mut)]
        let (sender, mut promise) = Self::new();
        promise.task_type = TaskType::Local;

        // This *generally* works but not super well.
        // Tokio doesn't do any fancy local scheduling.
        #[cfg(feature = "tokio")]
        {
            promise.join_handle = Some(tokio::task::spawn_local(async move {
                sender.send(future.await);
            }));
        }

        #[cfg(feature = "web")]
        {
            wasm_bindgen_futures::spawn_local(async move { sender.send(future.await) });
        }

        #[cfg(feature = "smol")]
        {
            promise.smol_task = Some(
                crate::LOCAL_EXECUTOR
                    .with(|exec| exec.spawn(async move { sender.send(future.await) })),
            );
        }

        promise
    }

    /// Spawn a blocking closure in a background task.
    ///
    /// You need to compile `poll-promise` with the "tokio" feature for this to be available.
    ///
    /// ## tokio
    /// This is a simple mechanism to offload a heavy function/closure to be processed in the thread pool for blocking CPU work.
    ///
    /// It can't do any async code. For that, use [`Self::spawn_async`].
    ///
    /// This is a convenience method, using [`Self::new`] with [`tokio::task::spawn`] and [`tokio::task::block_in_place`].
    ///
    /// ``` no_run
    /// # fn something_cpu_intensive() {}
    /// # use poll_promise::Promise;
    /// let promise = Promise::spawn_blocking(move || something_cpu_intensive());
    /// ```
    #[cfg(any(feature = "tokio", feature = "async-std"))]
    pub fn spawn_blocking<F>(f: F) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        let (sender, mut promise) = Self::new();
        #[cfg(feature = "tokio")]
        {
            promise.join_handle = Some(tokio::task::spawn(async move {
                sender.send(tokio::task::block_in_place(f));
            }));
        }

        #[cfg(feature = "async-std")]
        {
            promise.async_std_join_handle = Some(async_std::task::spawn_blocking(move || {
                sender.send(f());
            }));
        }

        promise
    }

    /// Spawn a blocking closure in a background thread.
    ///
    /// The first argument is the name of the thread you spawn, passed to [`std::thread::Builder::name`].
    /// It shows up in panic messages.
    ///
    /// This is a convenience method, using [`Self::new`] and [`std::thread::Builder`].
    ///
    /// If you are compiling with the "tokio" or "web" features, you should use [`Self::spawn_blocking`] or [`Self::spawn_async`] instead.
    ///
    /// ```
    /// # fn something_slow() {}
    /// # use poll_promise::Promise;
    /// let promise = Promise::spawn_thread("slow_operation", move || something_slow());
    /// ```
    #[cfg(not(target_arch = "wasm32"))] // can't spawn threads in wasm.
    pub fn spawn_thread<F>(thread_name: impl Into<String>, f: F) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        let (sender, promise) = Self::new();
        std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || sender.send(f()))
            .expect("Failed to spawn thread");
        promise
    }

    /// Polls the promise and either returns a reference to the data, or [`None`] if still pending.
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn ready(&self) -> Option<&T> {
        match self.poll() {
            std::task::Poll::Pending => None,
            std::task::Poll::Ready(value) => Some(value),
        }
    }

    /// Polls the promise and either returns a mutable reference to the data, or [`None`] if still pending.
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn ready_mut(&mut self) -> Option<&mut T> {
        match self.poll_mut() {
            std::task::Poll::Pending => None,
            std::task::Poll::Ready(value) => Some(value),
        }
    }

    /// Returns either the completed promise object or the promise itself if it is not completed yet.
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn try_take(self) -> Result<T, Self> {
        self.data.try_take().map_err(|data| Self {
            data,
            task_type: self.task_type,

            #[cfg(feature = "tokio")]
            join_handle: None,

            #[cfg(feature = "async-std")]
            async_std_join_handle: None,

            #[cfg(feature = "smol")]
            smol_task: self.smol_task,
        })
    }

    /// Block execution until ready, then returns a reference to the value.
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn block_until_ready(&self) -> &T {
        self.data.block_until_ready(self.task_type)
    }

    /// Block execution until ready, then returns a mutable reference to the value.
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn block_until_ready_mut(&mut self) -> &mut T {
        self.data.block_until_ready_mut(self.task_type)
    }

    /// Block execution until ready, then returns the promised value and consumes the `Promise`.
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn block_and_take(self) -> T {
        self.data.block_until_ready(self.task_type);
        match self.data.0.into_inner() {
            PromiseStatus::Pending(_) => unreachable!(),
            PromiseStatus::Ready(value) => value,
        }
    }

    /// Returns either a reference to the ready value [`std::task::Poll::Ready`]
    /// or [`std::task::Poll::Pending`].
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn poll(&self) -> std::task::Poll<&T> {
        self.data.poll(self.task_type)
    }

    /// Returns either a mut reference to the ready value in a [`std::task::Poll::Ready`]
    /// or a [`std::task::Poll::Pending`].
    ///
    /// Panics if the connected [`Sender`] was dropped before a value was sent.
    pub fn poll_mut(&mut self) -> std::task::Poll<&mut T> {
        self.data.poll_mut(self.task_type)
    }

    /// Returns the type of task this promise is running.
    /// See [`TaskType`].
    pub fn task_type(&self) -> TaskType {
        self.task_type
    }

    /// Abort the running task spawned by [`Self::spawn_async`].
    #[cfg(feature = "tokio")]
    pub fn abort(self) {
        if let Some(join_handle) = self.join_handle {
            join_handle.abort();
        }
    }
}

// ----------------------------------------------------------------------------

enum PromiseStatus<T: Send + 'static> {
    Pending(std::sync::mpsc::Receiver<T>),
    Ready(T),
}

struct PromiseImpl<T: Send + 'static>(UnsafeCell<PromiseStatus<T>>);

impl<T: Send + 'static> PromiseImpl<T> {
    #[allow(unused_variables)]
    fn poll_mut(&mut self, task_type: TaskType) -> std::task::Poll<&mut T> {
        let inner = self.0.get_mut();
        match inner {
            PromiseStatus::Pending(rx) => {
                #[cfg(all(feature = "smol", feature = "smol_tick_poll"))]
                Self::tick(task_type);
                if let Ok(value) = rx.try_recv() {
                    *inner = PromiseStatus::Ready(value);
                    match inner {
                        PromiseStatus::Ready(ref mut value) => std::task::Poll::Ready(value),
                        PromiseStatus::Pending(_) => unreachable!(),
                    }
                } else {
                    std::task::Poll::Pending
                }
            }
            PromiseStatus::Ready(ref mut value) => std::task::Poll::Ready(value),
        }
    }

    /// Returns either the completed promise object or the promise itself if it is not completed yet.
    fn try_take(self) -> Result<T, Self> {
        let inner = self.0.into_inner();
        match inner {
            PromiseStatus::Pending(ref rx) => match rx.try_recv() {
                Ok(value) => Ok(value),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    Err(PromiseImpl(UnsafeCell::new(inner)))
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("The Promise Sender was dropped")
                }
            },
            PromiseStatus::Ready(value) => Ok(value),
        }
    }

    #[allow(unsafe_code)]
    #[allow(unused_variables)]
    fn poll(&self, task_type: TaskType) -> std::task::Poll<&T> {
        let this = unsafe {
            // SAFETY: This is safe since Promise (and PromiseData) are !Sync and thus
            // need external synchronization anyway. We can only transition from
            // Pending->Ready, not the other way around, so once we're Ready we'll
            // stay ready.
            self.0.get().as_mut().expect("UnsafeCell should be valid")
        };
        match this {
            PromiseStatus::Pending(rx) => {
                #[cfg(all(feature = "smol", feature = "smol_tick_poll"))]
                Self::tick(task_type);
                match rx.try_recv() {
                    Ok(value) => {
                        *this = PromiseStatus::Ready(value);
                        match this {
                            PromiseStatus::Ready(ref value) => std::task::Poll::Ready(value),
                            PromiseStatus::Pending(_) => unreachable!(),
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => std::task::Poll::Pending,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        panic!("The Promise Sender was dropped")
                    }
                }
            }
            PromiseStatus::Ready(ref value) => std::task::Poll::Ready(value),
        }
    }

    #[allow(unused_variables)]
    fn block_until_ready_mut(&mut self, task_type: TaskType) -> &mut T {
        // Constantly poll until we're ready.
        #[cfg(feature = "smol")]
        while self.poll(task_type).is_pending() {
            // Tick unless poll does it for us.
            #[cfg(not(feature = "smol_tick_poll"))]
            Self::tick(task_type);
        }
        let inner = self.0.get_mut();
        match inner {
            PromiseStatus::Pending(rx) => {
                let value = rx.recv().expect("The Promise Sender was dropped");
                *inner = PromiseStatus::Ready(value);
                match inner {
                    PromiseStatus::Ready(ref mut value) => value,
                    PromiseStatus::Pending(_) => unreachable!(),
                }
            }
            PromiseStatus::Ready(ref mut value) => value,
        }
    }

    #[allow(unsafe_code)]
    #[allow(unused_variables)]
    fn block_until_ready(&self, task_type: TaskType) -> &T {
        // Constantly poll until we're ready.
        #[cfg(feature = "smol")]
        while self.poll(task_type).is_pending() {
            // Tick unless poll does it for us.
            #[cfg(not(feature = "smol_tick_poll"))]
            Self::tick(task_type);
        }
        let this = unsafe {
            // SAFETY: This is safe since Promise (and PromiseData) are !Sync and thus
            // need external synchronization anyway. We can only transition from
            // Pending->Ready, not the other way around, so once we're Ready we'll
            // stay ready.
            self.0.get().as_mut().expect("UnsafeCell should be valid")
        };
        match this {
            PromiseStatus::Pending(rx) => {
                let value = rx.recv().expect("The Promise Sender was dropped");
                *this = PromiseStatus::Ready(value);
                match this {
                    PromiseStatus::Ready(ref value) => value,
                    PromiseStatus::Pending(_) => unreachable!(),
                }
            }
            PromiseStatus::Ready(ref value) => value,
        }
    }

    #[cfg(feature = "smol")]
    fn tick(task_type: TaskType) {
        match task_type {
            TaskType::Local => {
                crate::tick_local();
            }
            TaskType::Async => {
                crate::tick();
            }
            TaskType::None => (),
        };
    }
}
