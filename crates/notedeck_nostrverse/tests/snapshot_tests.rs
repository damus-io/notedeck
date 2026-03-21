use std::path::PathBuf;

/// Download a model from a URL to a local directory, returning the path.
/// Skips download if the file already exists in the cache.
fn download_model(url: &str, dir: &std::path::Path) -> PathBuf {
    let filename = url.rsplit('/').next().unwrap_or("model.glb");
    let path = dir.join(filename);
    if path.exists() {
        return path;
    }
    let resp = ehttp::fetch_blocking(&ehttp::Request::get(url))
        .unwrap_or_else(|e| panic!("Failed to download {url}: {e}"));
    assert!(resp.ok, "HTTP {} for {url}", resp.status);
    std::fs::write(&path, &resp.bytes).unwrap();
    path
}

/// Create a headless wgpu device (prefers software/CPU adapter for determinism).
fn create_headless_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::None,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("No GPU adapter found — install lavapipe for headless rendering");

    let info = adapter.get_info();
    eprintln!("Using adapter: {} ({:?})", info.name, info.device_type);

    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("snapshot_test"),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        },
        None,
    ))
    .expect("Failed to create device")
}

fn snapshots_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// Compare a rendered image against a saved snapshot.
/// Creates/updates the snapshot when UPDATE_SNAPSHOTS is set.
fn check_snapshot(name: &str, img: &image::RgbaImage) {
    let dir = snapshots_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));

    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        img.save(&path).unwrap();
        eprintln!("Updated snapshot: {}", path.display());
        return;
    }

    if !path.exists() {
        img.save(&path).unwrap();
        eprintln!("Created new snapshot: {}", path.display());
        return;
    }

    let reference = image::open(&path)
        .unwrap_or_else(|e| panic!("Failed to open snapshot {}: {e}", path.display()))
        .to_rgba8();

    assert_eq!(
        (img.width(), img.height()),
        (reference.width(), reference.height()),
        "Snapshot {name} size mismatch"
    );

    // Allow small per-pixel differences for software rendering variance
    let total_pixels = (img.width() * img.height()) as usize;
    let mut diff_pixels = 0usize;
    for (a, b) in img.pixels().zip(reference.pixels()) {
        let max_channel_diff =
            a.0.iter()
                .zip(b.0.iter())
                .map(|(x, y)| (*x as i16 - *y as i16).unsigned_abs())
                .max()
                .unwrap_or(0);
        if max_channel_diff > 2 {
            diff_pixels += 1;
        }
    }

    let diff_ratio = diff_pixels as f64 / total_pixels as f64;
    assert!(
        diff_ratio < 0.01,
        "Snapshot {name} differs: {:.2}% pixels changed ({diff_pixels}/{total_pixels}). \
         Run `scripts/snapshot-test --update` to accept.",
        diff_ratio * 100.0
    );
}

/// Parse the demo space and load all models into a renderer, exercising
/// the same protoverse → renderbud pipeline as production.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_demo_scene() {
    let width = 800;
    let height = 600;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let (device, queue) = create_headless_device();
    let mut renderer = renderbud::Renderer::new(&device, &queue, format, (width, height));

    // Parse the production demo space definition
    let space =
        protoverse::parse(notedeck_nostrverse::DEMO_SPACE).expect("Failed to parse demo space");

    // Model download cache (reused across runs)
    let tmp = std::env::temp_dir().join("notedeck-snapshot-models");
    std::fs::create_dir_all(&tmp).unwrap();

    // Walk the space tree: download + load each object's model, place it
    let mut placed = 0;
    eprintln!("Space has {} cells", space.cells.len());
    for idx in 0..space.cells.len() {
        let cell_id = protoverse::CellId(idx as u32);

        let model_url = match space.model_url(cell_id) {
            Some(url) => url.to_string(),
            None => continue,
        };

        let path = download_model(&model_url, &tmp);
        let model = match renderer.load_gltf_model(&device, &queue, &path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Warning: failed to load {}: {e}", model_url);
                continue;
            }
        };

        let pos = space
            .position(cell_id)
            .map(|(x, y, z)| glam::Vec3::new(x as f32, y as f32, z as f32))
            .unwrap_or(glam::Vec3::ZERO);
        let rot = space
            .rotation(cell_id)
            .map(|(x, y, z)| {
                glam::Quat::from_euler(
                    glam::EulerRot::YXZ,
                    (y as f32).to_radians(),
                    (x as f32).to_radians(),
                    (z as f32).to_radians(),
                )
            })
            .unwrap_or(glam::Quat::IDENTITY);

        let transform = renderbud::Transform {
            translation: pos,
            rotation: rot,
            scale: glam::Vec3::ONE,
        };
        renderer.place_object(model, transform);
        placed += 1;
        let name = space.name(cell_id).unwrap_or("?");
        eprintln!(
            "  Placed {name} at ({:.1}, {:.1}, {:.1})",
            pos.x, pos.y, pos.z
        );
    }
    eprintln!("Placed {placed} objects total");

    // Position camera to see both rooms from above
    renderer.set_camera(
        glam::Vec3::new(0.0, 10.0, 10.0), // eye: elevated wide view
        glam::Vec3::new(0.0, 0.0, 2.0),   // target: center of the rooms
    );

    let img = renderer.render_to_image(&device, &queue);
    check_snapshot("demo_scene", &img);
}
