pub type Result<T> = std::result::Result<T, Error>;

/// Errors encountered when opening a URI.
#[derive(Debug)]
pub enum Error {
    /// Could not acquire the android environment.
    ///
    /// See the `android-env` crate for more details.
    AndroidEnvironment,
    #[cfg(target_os = "android")]
    Java(jni::errors::Error),
    /// The provided URI was malformed.
    MalformedUri,
    /// No handler was available to open the URI.
    NoHandler,
    /// An unknown error occurred.
    ///
    /// Note that on certain platforms if a handler is not available this error
    /// variant will be returned, as the error returned by the operating system
    /// is not fine-grained enough.
    Unknown,
}

#[cfg(target_os = "android")]
impl From<jni::errors::Error> for Error {
    fn from(value: jni::errors::Error) -> Self {
        Self::Java(value)
    }
}
