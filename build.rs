use std::process::Command;

fn main() {
    if let Ok(output) = Command::new("git").args(["rev-parse", "HEAD"]).output() {
        if output.status.success() {
            let hash = String::from_utf8_lossy(&output.stdout);
            println!("cargo:rustc-env=GIT_COMMIT_HASH={}", hash.trim());
        }
    }
}
