//! Fill a bundle directory with the microsandbox runtime, for packaging:
//!
//! `cargo run -p agentium-sandbox --features microvm --example stage_runtime -- <bundle>`
//!
//! See `agentium_sandbox::runtime` for the layout and how notedeck uses it.

#[cfg(microvm)]
#[tokio::main]
async fn main() {
    let Some(bundle) = std::env::args_os().nth(1) else {
        eprintln!("usage: stage_runtime <bundle dir>");
        std::process::exit(2);
    };
    let bundle = std::path::PathBuf::from(bundle);
    if let Err(e) = agentium_sandbox::runtime::stage(&bundle).await {
        eprintln!("stage_runtime: {e}");
        std::process::exit(1);
    }
    println!(
        "{}",
        agentium_sandbox::runtime::bundled_msb(&bundle).display()
    );
}

#[cfg(not(microvm))]
fn main() {
    eprintln!(
        "stage_runtime: built without microVMs ({:?}); pass --features microvm",
        agentium_sandbox::availability()
    );
    std::process::exit(2);
}
