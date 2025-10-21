fn slow_operation() -> String {
    std::thread::sleep(std::time::Duration::from_secs(2));
    "Hello from other thread!".to_owned()
}

fn main() {
    let promise = poll_promise::Promise::spawn_thread("bg_thread", slow_operation);

    eprint!("Waiting");

    // This loop would normally be a game loop, or the executor of an immediate mode GUI.
    loop {
        // Poll the promise:
        if let Some(result) = promise.ready() {
            eprintln!("\nDONE: {:?}", result);
            break;
        } else {
            eprint!("."); // show that we are waiting
        }

        // Do other stuff, e.g. game logic or painting a UI
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
