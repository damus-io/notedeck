# tracing-logcat

tracing-logcat is a library that provides an Android logcat output for the `tracing` library. It directly communicates with Android's `logd` process instead of using `liblog.so`, making it suitable for use with statically linked executables.

See [`examples/`](./examples/) for examples of how to use this library.

## License

tracing-logcat is licensed under Apache 2.0. Please see [`LICENSE`](./LICENSE) for the full license text.
