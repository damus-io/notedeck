use std::path::{Path, PathBuf};
use tracing::info;

/// Install the downloaded update and relaunch the application.
///
/// `staged_path` is the path to the new binary (or .app bundle on macOS,
/// or .apk on Android) that has been extracted from the downloaded archive.
pub fn install_and_restart(staged_path: &Path) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        install_apk_android(staged_path)
    }

    #[cfg(not(target_os = "android"))]
    {
        install_and_restart_desktop(staged_path)
    }
}

#[cfg(not(target_os = "android"))]
fn install_and_restart_desktop(staged_path: &Path) -> Result<(), String> {
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

/// On Android, call into Java to fire ACTION_VIEW intent with the APK.
/// The system package installer handles the rest.
#[cfg(target_os = "android")]
fn install_apk_android(apk_path: &Path) -> Result<(), String> {
    use jni::objects::{JObject, JValue};

    info!("installing APK update from '{}'", apk_path.display());

    let path_str = apk_path
        .to_str()
        .ok_or_else(|| "APK path is not valid UTF-8".to_string())?;

    let vm = unsafe { jni::JavaVM::from_raw(ndk_context::android_context().vm().cast()) }
        .map_err(|e| format!("Failed to get JavaVM: {e}"))?;

    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("Failed to attach JNI thread: {e}"))?;

    let context = unsafe { JObject::from_raw(ndk_context::android_context().context().cast()) };

    let jpath = env
        .new_string(path_str)
        .map_err(|e| format!("Failed to create JNI string: {e}"))?;

    env.call_method(
        context,
        "installApk",
        "(Ljava/lang/String;)V",
        &[JValue::Object(&jpath.into())],
    )
    .map_err(|e| format!("Failed to call installApk: {e}"))?;

    Ok(())
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

    if file_name.ends_with(".apk") {
        // APK is the final artifact — no extraction needed
        let dest = staging_dir.join(file_name);
        std::fs::copy(archive_path, &dest).map_err(|e| format!("Failed to copy APK: {e}"))?;
        return Ok(dest);
    } else if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Helper: create a tar.gz archive containing a single file named `name`
    /// with contents `data`. Returns the path to the archive.
    fn make_tar_gz(dir: &Path, archive_name: &str, name: &str, data: &[u8]) -> PathBuf {
        let archive_path = dir.join(archive_name);
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
        builder.finish().unwrap();

        archive_path
    }

    #[test]
    fn test_extract_tar_gz_finds_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let archive_dir = tmp.path().join("archives");
        let staging_dir = tmp.path().join("staging");
        std::fs::create_dir_all(&archive_dir).unwrap();
        std::fs::create_dir_all(&staging_dir).unwrap();

        let fake_binary = b"#!/bin/sh\necho hello\n";
        let archive_path = make_tar_gz(&archive_dir, "notedeck.tar.gz", "notedeck", fake_binary);

        let result = extract_archive(&archive_path, &staging_dir);
        assert!(result.is_ok(), "extract_archive failed: {:?}", result.err());

        let binary_path = result.unwrap();
        assert!(binary_path.exists(), "binary should exist");
        assert_eq!(
            binary_path.file_name().unwrap().to_str().unwrap(),
            "notedeck"
        );

        // Verify contents match
        let contents = std::fs::read(&binary_path).unwrap();
        assert_eq!(contents, fake_binary);

        // Verify executable permission on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&binary_path)
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "binary should be executable");
        }
    }

    #[test]
    fn test_extract_raw_binary_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_dir = tmp.path().join("staging");
        std::fs::create_dir_all(&staging_dir).unwrap();

        // Create a file that doesn't look like an archive
        let raw_path = tmp.path().join("notedeck_raw");
        let fake_binary = b"ELF fake binary content";
        std::fs::write(&raw_path, fake_binary).unwrap();

        let result = extract_archive(&raw_path, &staging_dir);
        assert!(result.is_ok(), "extract_archive failed: {:?}", result.err());

        let binary_path = result.unwrap();
        assert_eq!(
            binary_path.file_name().unwrap().to_str().unwrap(),
            "notedeck"
        );
        let contents = std::fs::read(&binary_path).unwrap();
        assert_eq!(contents, fake_binary);
    }

    #[test]
    fn test_find_binary_in_dir_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let result = find_binary_in_dir(tmp.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Could not find notedeck binary"));
    }

    #[test]
    fn test_extract_zip_finds_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_dir = tmp.path().join("staging");
        std::fs::create_dir_all(&staging_dir).unwrap();

        // Create a zip with a notedeck binary inside
        let zip_path = tmp.path().join("notedeck.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        zip_writer.start_file("notedeck", options).unwrap();
        let fake_binary = b"fake zip binary";
        zip_writer.write_all(fake_binary).unwrap();
        zip_writer.finish().unwrap();

        let result = extract_archive(&zip_path, &staging_dir);
        assert!(result.is_ok(), "extract_archive failed: {:?}", result.err());

        let binary_path = result.unwrap();
        assert_eq!(
            binary_path.file_name().unwrap().to_str().unwrap(),
            "notedeck"
        );
        let contents = std::fs::read(&binary_path).unwrap();
        assert_eq!(contents, fake_binary);
    }
}
