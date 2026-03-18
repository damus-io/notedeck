//! Integration tests for the update installation pipeline.
//!
//! Tests the real update flow: archive extraction via our `extract_archive()`,
//! `self_replace::self_replace()` via the `update_test_helper` binary,
//! and process relaunch.

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
