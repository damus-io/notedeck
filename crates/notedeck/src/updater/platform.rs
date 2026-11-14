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

/// Check, before downloading anything, that an update could be installed
/// over the running copy.
///
/// - **macOS:** the running executable must sit inside a `.app` bundle that
///   isn't App-Translocated (run straight from a DMG or a quarantined
///   download), and the bundle's parent directory must be writable, since
///   the install renames the bundle there.
/// - **Other desktops:** the executable's directory must be writable.
/// - **Android:** always `Ok`; the system package installer does the work.
///
/// The `Err` is a reason a person can read.
pub fn can_install_in_place() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        Ok(())
    }

    #[cfg(not(target_os = "android"))]
    {
        let current_exe =
            std::env::current_exe().map_err(|e| format!("Failed to get current exe: {e}"))?;
        install_dir_for(&current_exe).and_then(|dir| dir_is_writable(&dir))
    }
}

/// The directory an install over `current_exe` writes into: the `.app`'s
/// parent on macOS (refusing a translocated or bundle-less run), the
/// executable's own directory elsewhere.
#[cfg(not(target_os = "android"))]
fn install_dir_for(current_exe: &Path) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    let target = {
        let bundle = find_app_bundle(current_exe)?;
        if is_translocated(&bundle) {
            return Err(
                "Notedeck is running from a translocated location; move Notedeck.app to Applications to enable updates"
                    .to_string(),
            );
        }
        bundle
    };

    #[cfg(not(target_os = "macos"))]
    let target = current_exe.to_path_buf();

    target
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("'{}' has no parent directory", target.display()))
}

/// Whether we can create files in `dir`, checked by creating and removing
/// a probe file, so ACLs and read-only mounts count too.
#[cfg(not(target_os = "android"))]
pub fn dir_is_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".notedeck-update-probe-{}", std::process::id()));
    std::fs::File::create(&probe).map_err(|e| {
        format!(
            "Can't install updates: '{}' isn't writable ({e})",
            dir.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Whether macOS is running this bundle from an App Translocation mount
/// (`/private/var/folders/.../AppTranslocation/...`). A translocated
/// bundle is a read-only random path; replacing it would do nothing.
#[cfg(target_os = "macos")]
pub fn is_translocated(bundle: &Path) -> bool {
    bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
}

/// On macOS, verify the staged `.app` and swap it in for the running one.
///
/// A raw binary is refused: patching `Contents/MacOS/notedeck` inside a
/// signed, hardened-runtime bundle breaks its seal, so a raw binary on
/// macOS is always a packaging bug.
#[cfg(target_os = "macos")]
fn install_macos(staged_path: &Path, current_exe: &Path) -> Result<(), String> {
    if staged_path.extension().and_then(|e| e.to_str()) != Some("app") {
        return Err(format!(
            "Refusing to install '{}': a macOS update must be a whole .app bundle",
            staged_path.display()
        ));
    }

    let bundle_path = find_app_bundle(current_exe)?;
    verify_staged_bundle(staged_path, &bundle_path)?;
    swap_bundle(staged_path, &bundle_path)
}

/// Check the staged bundle before the running one is touched: its
/// signature must verify (`codesign --verify --deep --strict`), and its
/// TeamIdentifier must equal the running bundle's.
///
/// A running bundle with no team (ad-hoc signed or unsigned, i.e. a dev
/// build) is refused, since there is nothing to match against.
#[cfg(target_os = "macos")]
pub fn verify_staged_bundle(staged: &Path, running: &Path) -> Result<(), String> {
    verify_bundle_signature(staged)?;

    let running_team = bundle_team_identifier(running)?.ok_or_else(|| {
        format!(
            "Running app '{}' has no signing team; refusing to auto-update an unsigned or ad-hoc build",
            running.display()
        )
    })?;
    let staged_team = bundle_team_identifier(staged)?
        .ok_or_else(|| format!("Update '{}' has no signing team", staged.display()))?;

    if staged_team != running_team {
        return Err(format!(
            "Update is signed by team {staged_team}, but the running app by team {running_team}"
        ));
    }
    Ok(())
}

/// Run `codesign --verify --deep --strict` on a bundle. A truncated,
/// modified or unsigned bundle fails here.
#[cfg(target_os = "macos")]
pub fn verify_bundle_signature(bundle: &Path) -> Result<(), String> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(bundle)
        .output()
        .map_err(|e| format!("Failed to run codesign: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "Signature check failed for '{}': {}",
        bundle.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// The TeamIdentifier a bundle is signed with, from `codesign -dv`, or
/// `None` when it has none.
#[cfg(target_os = "macos")]
fn bundle_team_identifier(bundle: &Path) -> Result<Option<String>, String> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=2"])
        .arg(bundle)
        .output()
        .map_err(|e| format!("Failed to run codesign: {e}"))?;
    // codesign -d prints its report on stderr, and exits non-zero for an
    // unsigned bundle, which is just "no team".
    let report = String::from_utf8_lossy(&out.stderr);
    Ok(parse_team_identifier(&report).map(str::to_string))
}

/// Pull the `TeamIdentifier=` value out of `codesign -dv` output.
/// `TeamIdentifier=not set` (ad-hoc signatures) and a missing line are `None`.
pub fn parse_team_identifier(codesign_report: &str) -> Option<&str> {
    codesign_report
        .lines()
        .find_map(|line| line.trim().strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
}

/// Swap `staged` in for `bundle`: rename the running bundle aside, rename
/// the new one in, and restore the old one if that fails.
///
/// `rename` can't cross volumes (`EXDEV`), so a staged bundle on another
/// volume is first copied with `ditto` (which keeps xattrs and signatures)
/// into a sibling `<name>.app.new`, before anything is moved.
#[cfg(target_os = "macos")]
pub fn swap_bundle(staged: &Path, bundle: &Path) -> Result<(), String> {
    let parent = bundle
        .parent()
        .ok_or_else(|| format!("'{}' has no parent directory", bundle.display()))?;
    let backup = bundle.with_extension("app.old");
    let sibling = bundle.with_extension("app.new");
    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_dir_all(&sibling);

    let source = if same_volume(staged, parent) {
        staged.to_path_buf()
    } else {
        info!(
            "staged bundle is on another volume; copying to '{}'",
            sibling.display()
        );
        ditto(staged, &sibling)?;
        sibling.clone()
    };

    info!(
        "swapping .app bundle: '{}' -> '{}'",
        bundle.display(),
        backup.display()
    );

    std::fs::rename(bundle, &backup)
        .map_err(|e| format!("Failed to move current .app to backup: {e}"))?;

    if let Err(e) = std::fs::rename(&source, bundle) {
        let _ = std::fs::rename(&backup, bundle);
        let _ = std::fs::remove_dir_all(&sibling);
        return Err(format!("Failed to move new .app into place: {e}"));
    }

    // Best-effort cleanup: the old bundle, and the staged one if we copied it.
    let _ = std::fs::remove_dir_all(&backup);
    if source != staged {
        let _ = std::fs::remove_dir_all(staged);
    }
    Ok(())
}

/// Whether two paths live on the same filesystem, so `rename` between
/// them works. Unknown counts as different, which only costs a copy.
#[cfg(target_os = "macos")]
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// Copy a bundle with `/usr/bin/ditto`, keeping symlinks, xattrs and the
/// code signature intact.
#[cfg(target_os = "macos")]
fn ditto(from: &Path, to: &Path) -> Result<(), String> {
    let status = std::process::Command::new("/usr/bin/ditto")
        .arg(from)
        .arg(to)
        .status()
        .map_err(|e| format!("Failed to run ditto: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let _ = std::fs::remove_dir_all(to);
    Err(format!(
        "ditto '{}' -> '{}' failed: {status}",
        from.display(),
        to.display()
    ))
}

/// Find the .app bundle root from an executable path inside it.
/// e.g. /Applications/Notedeck.app/Contents/MacOS/notedeck -> /Applications/Notedeck.app
#[cfg(target_os = "macos")]
pub fn find_app_bundle(exe_path: &Path) -> Result<PathBuf, String> {
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

/// Use self-replace to atomically swap the binary (Linux/Windows).
#[cfg(not(any(target_os = "macos", target_os = "android")))]
fn install_replace(staged_path: &Path, _current_exe: &Path) -> Result<(), String> {
    self_replace::self_replace(staged_path).map_err(|e| format!("self-replace failed: {e}"))?;

    // Clean up the staged file
    let _ = std::fs::remove_file(staged_path);

    Ok(())
}

/// Relaunch the application after update.
///
/// On macOS, a detached shell waits for this process to exit and then
/// `open`s the bundle (without `-n`), so the new instance never shares the
/// data dir, the LMDB env or Dave's IPC socket with the old one.
#[cfg(not(target_os = "android"))]
fn relaunch(current_exe: &Path) -> Result<(), String> {
    info!("relaunching '{}'", current_exe.display());

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        let bundle = find_app_bundle(current_exe)?;
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.1; done; exec /usr/bin/open "$2""#)
            .arg("sh")
            .arg(std::process::id().to_string())
            .arg(&bundle)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("Failed to schedule relaunch: {e}"))?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        std::process::Command::new(current_exe)
            .spawn()
            .map_err(|e| format!("Failed to relaunch: {e}"))?;
    }

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
        // APK is the final artifact — no extraction needed.
        // Return the archive path directly; the caller must NOT delete it.
        Ok(archive_path.to_path_buf())
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

    /// Captured `codesign -dv --verbose=2` report of a Developer ID app.
    const DEVELOPER_ID_REPORT: &str = "Executable=/Applications/Blender.app/Contents/MacOS/Blender
Identifier=org.blenderfoundation.blender
Format=app bundle with Mach-O thin (arm64)
CodeDirectory v=20500 size=1420681 flags=0x10000(runtime) hashes=44385+7 location=embedded
Signature size=9005
Authority=Developer ID Application: Stichting Blender Foundation (68UA947AUU)
Authority=Developer ID Certification Authority
Authority=Apple Root CA
Timestamp=Jul 14, 2026 at 03:29:09
Info.plist entries=19
TeamIdentifier=68UA947AUU
Runtime Version=26.1.0
Sealed Resources version=2 rules=13 files=6490
Internal requirements count=1 size=192
";

    /// Captured `codesign -dv --verbose=2` report of an ad-hoc signature.
    const ADHOC_REPORT: &str = "Executable=/private/var/folders/kb/T/x/ls
Identifier=ls-5555494471f30178d6d23727b9081004e81ac5bb
Format=Mach-O universal (x86_64 arm64e)
CodeDirectory v=20400 size=356 flags=0x2(adhoc) hashes=5+2 location=embedded
Signature=adhoc
Info.plist=not bound
TeamIdentifier=not set
Sealed Resources=none
Internal requirements count=0 size=12
";

    #[test]
    fn test_parse_team_identifier() {
        assert_eq!(
            parse_team_identifier(DEVELOPER_ID_REPORT),
            Some("68UA947AUU")
        );
        assert_eq!(parse_team_identifier(ADHOC_REPORT), None);
        assert_eq!(
            parse_team_identifier("/tmp/x.app: code object is not signed at all\n"),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_find_app_bundle_nested() {
        assert_eq!(
            find_app_bundle(Path::new(
                "/Applications/Notedeck.app/Contents/MacOS/notedeck"
            )),
            Ok(PathBuf::from("/Applications/Notedeck.app"))
        );
        // The innermost bundle wins (a helper app nested in another).
        assert_eq!(
            find_app_bundle(Path::new(
                "/Applications/Outer.app/Contents/Helpers/Inner.app/Contents/MacOS/inner"
            )),
            Ok(PathBuf::from(
                "/Applications/Outer.app/Contents/Helpers/Inner.app"
            ))
        );
        assert!(find_app_bundle(Path::new("/Users/me/target/debug/notedeck")).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_is_translocated() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/xx/abc123/T/AppTranslocation/0F1E2D3C-AAAA-BBBB-CCCC-1234567890AB/d/Notedeck.app"
        )));
        assert!(!is_translocated(Path::new("/Applications/Notedeck.app")));
        assert!(!is_translocated(Path::new(
            "/Users/me/AppTranslocationNotes/Notedeck.app"
        )));
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn test_dir_is_writable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        assert!(dir_is_writable(tmp.path()).is_ok());
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "probe removed"
        );

        let ro = tmp.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = dir_is_writable(&ro);
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = result.expect_err("a 0555 dir is not writable");
        assert!(err.contains("isn't writable"), "{err}");
    }
}
