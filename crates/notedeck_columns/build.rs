use std::process::Command;

fn fallback() {
    if let Some(dirname) = std::env::current_dir()
        .as_ref()
        .ok()
        .and_then(|cwd| cwd.file_name().and_then(|fname| fname.to_str()))
    {
        println!("cargo:rustc-env=GIT_COMMIT_HASH={dirname}");
    } else {
        println!("cargo:rustc-env=GIT_COMMIT_HASH=unknown");
    }
}

fn main() {
    let output = if let Ok(output) = Command::new("git").args(["rev-parse", "HEAD"]).output() {
        output
    } else {
        fallback();
        return;
    };

    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout);
        println!("cargo:rustc-env=GIT_COMMIT_HASH={}", hash.trim());
    } else {
        fallback();
    }
}
