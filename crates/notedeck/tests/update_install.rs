//! Integration tests for the update installation pipeline.
//!
//! Tests the real update flow: archive extraction via our `extract_archive()`,
//! `self_replace::self_replace()` via the `update_test_helper` binary,
//! and process relaunch.

#![cfg(feature = "auto-update")]

#[cfg(unix)]
mod unix_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    /// Build the `update_test_helper` binary with a specific version tag.
    /// Returns the path to the compiled binary.
    fn build_helper(version: &str) -> std::path::PathBuf {
        let status = Command::new("cargo")
            .args([
                "build",
                "--bin",
                "update_test_helper",
                "--features",
                "auto-update",
            ])
            .env("UPDATE_TEST_VERSION", version)
            .status()
            .expect("cargo build failed");
        assert!(status.success(), "failed to build update_test_helper");

        // Find the binary in target/debug
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        workspace_root.join("target/debug/update_test_helper")
    }

    /// Test that `extract_archive()` correctly extracts a tar.gz and finds
    /// the notedeck binary with executable permissions.
    #[test]
    fn test_extract_tar_gz_pipeline() {
        let tmp = tempfile::tempdir().unwrap();
        let staging_dir = tmp.path().join("staging");
        let archive_dir = tmp.path().join("archive");
        fs::create_dir_all(&staging_dir).unwrap();
        fs::create_dir_all(&archive_dir).unwrap();

        // Create a fake binary and pack it into a tar.gz named "notedeck"
        let fake_content = b"#!/bin/sh\necho updated\n";
        let archive_path = archive_dir.join("notedeck.tar.gz");
        {
            let file = fs::File::create(&archive_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);

            let mut header = tar::Header::new_gnu();
            header.set_size(fake_content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "notedeck", &fake_content[..])
                .unwrap();
            builder.finish().unwrap();
        }

        let binary_path = notedeck::updater::platform::extract_archive(&archive_path, &staging_dir)
            .expect("extract_archive failed");

        // Verify binary was found and has correct name
        assert!(binary_path.exists());
        assert_eq!(
            binary_path.file_name().unwrap().to_str().unwrap(),
            "notedeck"
        );

        // Verify contents
        assert_eq!(fs::read(&binary_path).unwrap(), fake_content);

        // Verify executable permissions
        let mode = fs::metadata(&binary_path).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "binary should be executable");
    }

    /// The macOS update asset is the whole `Notedeck.app`, packed by
    /// `scripts/macos_build.sh` with the system tar:
    /// `tar czf <asset> -C <bundle dir> Notedeck.app`.
    ///
    /// Pack a fake bundle the same way and check that `extract_archive()`
    /// returns the `.app` (not a raw binary) with the executable's exec bit
    /// and the bundle's symlinks intact, since a signed bundle that loses
    /// either no longer verifies.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_extract_macos_app_bundle_tarball() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let bundle_dir = tmp.path().join("bundle/osx");
        let staging_dir = tmp.path().join("staging");
        fs::create_dir_all(&staging_dir).unwrap();

        let app = bundle_dir.join("Notedeck.app");
        let contents = app.join("Contents");
        let macos = contents.join("MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::create_dir_all(contents.join("Resources")).unwrap();
        fs::write(contents.join("Info.plist"), "<plist/>").unwrap();
        let exe = macos.join("notedeck");
        fs::write(&exe, "#!/bin/sh\necho updated\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("../MacOS/notedeck", contents.join("Resources/link")).unwrap();

        let asset = tmp.path().join("notedeck-aarch64-macos.tar.gz");
        let status = Command::new("/usr/bin/tar")
            .arg("czf")
            .arg(&asset)
            .arg("-C")
            .arg(&bundle_dir)
            .arg("Notedeck.app")
            .status()
            .expect("run tar");
        assert!(status.success(), "tar failed");

        let found = notedeck::updater::platform::extract_archive(&asset, &staging_dir)
            .expect("extract_archive failed");

        assert_eq!(found, staging_dir.join("Notedeck.app"));
        let extracted_exe = found.join("Contents/MacOS/notedeck");
        let mode = fs::metadata(&extracted_exe).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "bundle executable should stay executable"
        );
        let link = found.join("Contents/Resources/link");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(&link).unwrap(),
            Path::new("../MacOS/notedeck")
        );
    }

    /// Build a minimal `<name>.app` (Info.plist plus a shell-script
    /// executable that prints `tag`) under `dir`, ad-hoc signed.
    #[cfg(target_os = "macos")]
    fn make_signed_fake_app(dir: &Path, name: &str, tag: &str) -> std::path::PathBuf {
        let app = dir.join(format!("{name}.app"));
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>fake</string>
<key>CFBundleIdentifier</key><string>com.example.{name}</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
"#
            ),
        )
        .unwrap();
        let exe = macos.join("fake");
        fs::write(&exe, format!("#!/bin/sh\necho {tag}\n")).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();

        let status = Command::new("/usr/bin/codesign")
            .args(["-s", "-", "--force", "--deep"])
            .arg(&app)
            .status()
            .expect("run codesign");
        assert!(status.success(), "ad-hoc signing failed");
        app
    }

    /// `verify_bundle_signature` accepts an intact ad-hoc signed bundle and
    /// rejects one whose executable changed after signing.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore] // shells out to codesign
    fn test_verify_bundle_signature() {
        use notedeck::updater::platform::verify_bundle_signature;

        let tmp = tempfile::tempdir().unwrap();
        let good = make_signed_fake_app(&tmp.path().join("good"), "Fake", "good");
        verify_bundle_signature(&good).expect("intact bundle should verify");

        let bad = make_signed_fake_app(&tmp.path().join("bad"), "Fake", "bad");
        fs::write(
            bad.join("Contents/MacOS/fake"),
            "#!/bin/sh\necho tampered\n",
        )
        .unwrap();
        let err = verify_bundle_signature(&bad).expect_err("tampered bundle must not verify");
        assert!(err.contains("Signature check failed"), "{err}");
    }

    /// An ad-hoc signed running app has no TeamIdentifier, so there is
    /// nothing to match an update against: `verify_staged_bundle` refuses,
    /// even when the staged bundle itself verifies.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore] // shells out to codesign
    fn test_verify_staged_bundle_refuses_teamless_running_app() {
        use notedeck::updater::platform::verify_staged_bundle;

        let tmp = tempfile::tempdir().unwrap();
        let running = make_signed_fake_app(&tmp.path().join("installed"), "Fake", "v1");
        let staged = make_signed_fake_app(&tmp.path().join("staging"), "Fake", "v2");

        let err = verify_staged_bundle(&staged, &running).expect_err("no team to match");
        assert!(err.contains("has no signing team"), "{err}");
    }

    /// `swap_bundle` puts the staged bundle in place of the running one,
    /// leaves no `.app.old`/`.app.new` behind, and the result still verifies.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore] // shells out to codesign
    fn test_swap_bundle_replaces_whole_app() {
        use notedeck::updater::platform::{swap_bundle, verify_bundle_signature};

        let tmp = tempfile::tempdir().unwrap();
        let installed_dir = tmp.path().join("Applications");
        let running = make_signed_fake_app(&installed_dir, "Fake", "v1");
        let staged = make_signed_fake_app(&tmp.path().join("staging"), "Fake", "v2");

        swap_bundle(&staged, &running).expect("swap");

        let out = Command::new(running.join("Contents/MacOS/fake"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "v2");
        verify_bundle_signature(&running).expect("swapped-in bundle should still verify");
        assert!(!staged.exists(), "staged bundle was moved, not copied");
        let leftovers: Vec<_> = fs::read_dir(&installed_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("Fake.app")]);
    }

    /// Test `self_replace::self_replace()` + relaunch using the real
    /// `update_test_helper` binary.
    ///
    /// 1. Build v1 and v2 of the helper (with different VERSION_TAG)
    /// 2. Copy v1 to a tmpdir as the "installed" binary
    /// 3. Copy v2 as the "staged" update
    /// 4. Run v1 with the staged path — it calls self_replace + relaunch
    /// 5. Read the marker file: should have two lines (v1's tag, then v2's tag)
    #[test]
    #[ignore] // slow: builds helper binary twice via cargo
    fn test_self_replace_and_relaunch() {
        // Build both versions
        let v1_src = build_helper("version-one");
        // Force rebuild with different version
        let v2_src = build_helper("version-two");

        // Wait — cargo build caches, so v1 and v2 end up as the same binary
        // (the last build wins). We need to copy v1 BEFORE building v2.
        // Rebuild in correct order:
        let _ = build_helper("version-one");
        let tmp = tempfile::tempdir().unwrap();
        let v1_copy = tmp.path().join("v1_binary");
        fs::copy(&v1_src, &v1_copy).unwrap();

        let _ = build_helper("version-two");
        let v2_copy = tmp.path().join("v2_binary");
        fs::copy(&v2_src, &v2_copy).unwrap();

        // Verify they're actually different
        let v1_bytes = fs::read(&v1_copy).unwrap();
        let v2_bytes = fs::read(&v2_copy).unwrap();
        assert_ne!(v1_bytes, v2_bytes, "v1 and v2 should be different binaries");

        // Set up the test: "installed" binary is v1, "staged" is v2
        let installed = tmp.path().join("notedeck");
        let staged = tmp.path().join("notedeck_staged");
        let marker = tmp.path().join("marker.txt");

        fs::copy(&v1_copy, &installed).unwrap();
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).unwrap();
        fs::copy(&v2_copy, &staged).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();

        // Run v1 with staged path — triggers self_replace + relaunch
        let status = Command::new(&installed)
            .args([marker.to_str().unwrap(), staged.to_str().unwrap()])
            .status()
            .expect("failed to run v1");
        assert!(status.success(), "v1 exited with failure");

        // Give the relaunched child a moment to finish
        std::thread::sleep(Duration::from_millis(500));

        // Read marker: should have two lines
        let content = fs::read_to_string(&marker).expect("marker file should exist");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "expected 2 lines (v1 + v2), got: {content:?}"
        );
        assert_eq!(lines[0], "version-one", "first run should be v1");
        assert_eq!(
            lines[1], "version-two",
            "second run (after self-replace) should be v2"
        );

        // The staged file should have been cleaned up by self_replace
        assert!(!staged.exists(), "staged binary should be cleaned up");

        // The installed binary should now be v2
        let final_bytes = fs::read(&installed).unwrap();
        assert_eq!(final_bytes, v2_bytes, "installed binary should now be v2");
    }

    /// Full pipeline: extract from archive → self-replace → relaunch.
    /// Combines extraction with self-replace to test the complete update flow.
    #[test]
    #[ignore] // slow: builds helper binary twice via cargo
    fn test_full_update_pipeline() {
        // Build v1 and v2
        let _ = build_helper("pipeline-v1");
        let tmp = tempfile::tempdir().unwrap();
        let v1_src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("target/debug/update_test_helper");
        let v1_copy = tmp.path().join("v1_binary");
        fs::copy(&v1_src, &v1_copy).unwrap();

        let _ = build_helper("pipeline-v2");
        let v2_copy = tmp.path().join("v2_binary");
        fs::copy(&v1_src, &v2_copy).unwrap(); // v1_src now contains v2

        // Pack v2 into a tar.gz named "notedeck" (as our release archives do)
        let archive_dir = tmp.path().join("archive");
        let staging_dir = tmp.path().join("staging");
        fs::create_dir_all(&archive_dir).unwrap();
        fs::create_dir_all(&staging_dir).unwrap();

        let archive_path = archive_dir.join("notedeck.tar.gz");
        {
            let file = fs::File::create(&archive_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            builder.append_path_with_name(&v2_copy, "notedeck").unwrap();
            builder.finish().unwrap();
        }

        // Step 1: extract the archive using our extract_archive()
        let extracted = notedeck::updater::platform::extract_archive(&archive_path, &staging_dir)
            .expect("extract_archive failed");
        assert!(extracted.exists());

        // Step 2: "install" v1 and run with extracted v2 as staged
        let installed = tmp.path().join("notedeck_installed");
        let marker = tmp.path().join("marker.txt");
        fs::copy(&v1_copy, &installed).unwrap();
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).unwrap();

        let status = Command::new(&installed)
            .args([marker.to_str().unwrap(), extracted.to_str().unwrap()])
            .status()
            .expect("failed to run installed binary");
        assert!(status.success());

        std::thread::sleep(Duration::from_millis(500));

        let content = fs::read_to_string(&marker).expect("marker file");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "expected v1 + v2 runs, got: {content:?}");
        assert_eq!(lines[0], "pipeline-v1");
        assert_eq!(lines[1], "pipeline-v2");
    }
}
