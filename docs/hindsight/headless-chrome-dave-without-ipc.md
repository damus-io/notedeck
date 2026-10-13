# A headless chrome builds Dave without the IPC spawn socket

`Chrome::new_headless` builds Dave with `Dave::new`, which rebinds the machine's
one spawn socket: a dave-featured chrome test cuts off the running notedeck.

- headway:dave/step-neglect-awful — one `cargo test -p notedeck_chrome --features headway,dave` (actions.rs open_tests) took the live notedeck's spawn.sock mid-session; nothing in the test run says so
