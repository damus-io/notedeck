/// Test helper for exercising `self_replace::self_replace()` + relaunch.
///
/// This binary is NOT shipped — it exists only as a test fixture.
///
/// Usage:
///   update_test_helper <marker_file>                     # write identity + exit
///   update_test_helper <marker_file> <staged_binary>     # self-replace, relaunch
///
/// "Identity" is the file size of the binary itself. The v1 and v2
/// binaries are compiled with different embedded strings so they have
/// different sizes, proving which version actually ran.
use std::env;
use std::fs;
use std::process::Command;

/// Embedded version tag — changed between v1 and v2 builds via env var.
/// The different string lengths cause different binary sizes.
const VERSION_TAG: &str = match option_env!("UPDATE_TEST_VERSION") {
    Some(v) => v,
    None => "unset",
};

fn binary_identity() -> String {
    VERSION_TAG.to_string()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: update_test_helper <marker_file> [<staged_binary>]");
        std::process::exit(1);
    }

    let marker_file = &args[1];
    let identity = binary_identity();

    // Append our identity to the marker file (one line per invocation)
    let mut content = fs::read_to_string(marker_file).unwrap_or_default();
    content.push_str(&format!("{identity}\n"));
    fs::write(marker_file, &content).expect("write marker");

    // If a staged binary is provided, self-replace and relaunch
    if let Some(staged) = args.get(2) {
        self_replace::self_replace(staged).expect("self_replace failed");
        let _ = fs::remove_file(staged);

        // Relaunch ourselves (now the new binary) without the staged arg.
        // Use argv[0] (our invocation path) rather than current_exe(),
        // since current_exe() resolves symlinks and may point to the
        // cargo build directory rather than our installed location.
        Command::new(&args[0])
            .arg(marker_file)
            .spawn()
            .expect("relaunch failed");

        std::process::exit(0);
    }
}
