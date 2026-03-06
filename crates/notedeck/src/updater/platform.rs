use std::path::{Path, PathBuf};
use tracing::info;

/// Install the downloaded update and relaunch the application.
///
/// `staged_path` is the path to the new binary (or .app bundle on macOS)
/// that has been extracted from the downloaded archive.
pub fn install_and_restart(staged_path: &Path) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current exe: {e}"))?;

    info!(
        "installing update from '{}' over '{}'",
        staged_path.display(),
        current_exe.display()
    );

    #[cfg(target_os = "macos")]
    {
        install_macos(staged_path, &current_exe)?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        install_replace(staged_path, &current_exe)?;
    }

    relaunch(&current_exe)
}

/// On macOS, replace the entire .app bundle to preserve code signing.
#[cfg(target_os = "macos")]
fn install_macos(staged_path: &Path, current_exe: &Path) -> Result<(), String> {
    // current_exe is something like /Applications/Notedeck.app/Contents/MacOS/notedeck
    // We need to find the .app bundle root
    let bundle_path = find_app_bundle(current_exe)?;

    // Check if the staged path is a .app bundle or a raw binary
    if staged_path.extension().and_then(|e| e.to_str()) == Some("app") {
        // Replacing entire .app bundle
        let backup = bundle_path.with_extension("app.old");

        info!(
            "swapping .app bundle: '{}' -> '{}'",
            bundle_path.display(),
            backup.display()
        );

        // Atomic-ish swap: rename current, move new, cleanup
        std::fs::rename(&bundle_path, &backup)
            .map_err(|e| format!("Failed to move current .app to backup: {e}"))?;

        if let Err(e) = std::fs::rename(staged_path, &bundle_path) {
            // Try to restore backup
            let _ = std::fs::rename(&backup, &bundle_path);
            return Err(format!("Failed to move new .app into place: {e}"));
        }

        // Best-effort cleanup of old bundle
        let _ = std::fs::remove_dir_all(&backup);
        Ok(())
    } else {
        // Raw binary — use self-replace on the binary inside the bundle
        install_replace(staged_path, current_exe)
    }
}

/// Find the .app bundle root from an executable path inside it.
/// e.g. /Applications/Notedeck.app/Contents/MacOS/notedeck -> /Applications/Notedeck.app
#[cfg(target_os = "macos")]
fn find_app_bundle(exe_path: &Path) -> Result<PathBuf, String> {
    let mut path = exe_path;
    while let Some(parent) = path.parent() {
        if let Some(ext) = path.extension() {
            if ext == "app" {
                return Ok(path.to_path_buf());
            }
        }
        path = parent;
    }
    Err(format!(
        "Could not find .app bundle containing '{}'",
        exe_path.display()
    ))
}

/// Use self-replace to atomically swap the binary (Linux/Windows, or macOS raw binary)
fn install_replace(staged_path: &Path, _current_exe: &Path) -> Result<(), String> {
    self_replace::self_replace(staged_path).map_err(|e| format!("self-replace failed: {e}"))?;

    // Clean up the staged file
    let _ = std::fs::remove_file(staged_path);

    Ok(())
}

/// Relaunch the application after update
fn relaunch(current_exe: &Path) -> Result<(), String> {
    info!("relaunching '{}'", current_exe.display());

    #[cfg(target_os = "macos")]
    {
        // On macOS, find the .app bundle and use `open` to relaunch
        if let Ok(bundle) = find_app_bundle(current_exe) {
            std::process::Command::new("open")
                .arg("-n")
                .arg(&bundle)
                .spawn()
                .map_err(|e| format!("Failed to relaunch via open: {e}"))?;
            std::process::exit(0);
        }
    }

    // Fallback: direct exec
    std::process::Command::new(current_exe)
        .spawn()
        .map_err(|e| format!("Failed to relaunch: {e}"))?;

    std::process::exit(0);
}

/// Extract a downloaded archive to the staging directory.
/// Returns the path to the extracted binary or .app bundle.
pub fn extract_archive(archive_path: &Path, staging_dir: &Path) -> Result<PathBuf, String> {
    let file_name = archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
        extract_tar_gz(archive_path, staging_dir)
    } else if file_name.ends_with(".zip") {
        extract_zip(archive_path, staging_dir)
    } else {
        // Assume it's a raw binary
        let dest = staging_dir.join("notedeck");
        std::fs::copy(archive_path, &dest).map_err(|e| format!("Failed to copy binary: {e}"))?;
        Ok(dest)
    }
}

fn extract_tar_gz(archive_path: &Path, staging_dir: &Path) -> Result<PathBuf, String> {
    let file =
        std::fs::File::open(archive_path).map_err(|e| format!("Failed to open archive: {e}"))?;

    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    archive
        .unpack(staging_dir)
        .map_err(|e| format!("Failed to extract tar.gz: {e}"))?;

    find_binary_in_dir(staging_dir)
}

fn extract_zip(archive_path: &Path, staging_dir: &Path) -> Result<PathBuf, String> {
    let file =
        std::fs::File::open(archive_path).map_err(|e| format!("Failed to open archive: {e}"))?;

    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Failed to read zip: {e}"))?;

    archive
        .extract(staging_dir)
        .map_err(|e| format!("Failed to extract zip: {e}"))?;

    find_binary_in_dir(staging_dir)
}

/// Find the notedeck binary (or .app bundle) in the extracted directory
fn find_binary_in_dir(dir: &Path) -> Result<PathBuf, String> {
    // Look for .app bundle first (macOS)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("app") {
                return Ok(path);
            }
        }
    }

    // Look for notedeck binary
    let candidates = ["notedeck", "notedeck.exe"];
    for name in &candidates {
        let path = dir.join(name);
        if path.exists() {
            // Ensure it's executable on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
            return Ok(path);
        }
    }

    Err(format!(
        "Could not find notedeck binary in '{}'",
        dir.display()
    ))
}
