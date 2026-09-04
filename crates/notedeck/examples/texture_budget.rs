//! Measures the GPU cost of scrolling past a lot of media.
//!
//! This drives the real texture caches through a real wgpu device: it writes a
//! pile of images into a throwaway disk cache, then scrolls a window across them
//! forever, reporting the process's own `phys_footprint` alongside what the
//! caches think they are holding. On macOS `phys_footprint` is the number that
//! matters, because GPU-owned pages never appear in RSS.
//!
//! Run it twice to see the difference the budget makes:
//!
//! ```sh
//! # unbounded: what notedeck did before the budget existed
//! TEXBUDGET_BYTES=0 cargo run --release --example texture_budget -p notedeck
//! # bounded
//! TEXBUDGET_BYTES=134217728 cargo run --release --example texture_budget -p notedeck
//! ```

use std::process::Command;
use std::sync::mpsc::channel;

use egui::ColorImage;
use notedeck::{ImageType, Images, JobCache, JobPool, MediaCache, MediaCacheType, TextureState};

/// Side length of each generated image. 1024x1024 RGBA is 4 MiB of texture.
const IMAGE_SIZE: usize = 1024;

/// How many distinct images the "timeline" contains.
const IMAGE_COUNT: usize = 256;

/// How many are on screen, and therefore protected from eviction, at once.
const WINDOW: usize = 12;

fn url_for(i: usize) -> String {
    format!("https://example.invalid/img/{i}.webp")
}

/// A cheap distinguishable gradient. Cheap matters: this is encoded to lossless
/// webp `IMAGE_COUNT` times before the measurement starts.
fn generate_image(i: usize) -> ColorImage {
    let mut pixels = Vec::with_capacity(IMAGE_SIZE * IMAGE_SIZE);
    for y in 0..IMAGE_SIZE {
        for x in 0..IMAGE_SIZE {
            pixels.push(egui::Color32::from_rgb(
                (x / 4) as u8,
                (y / 4) as u8,
                i as u8,
            ));
        }
    }
    ColorImage {
        size: [IMAGE_SIZE, IMAGE_SIZE],
        pixels,
    }
}

/// The `owned unmapped (graphics)` row and totals from `vmmap -summary`, which
/// is where Metal's driver allocations show up.
struct Footprint {
    graphics_dirty: String,
    graphics_swapped: String,
    graphics_regions: String,
    phys_footprint: String,
    phys_footprint_peak: String,
}

fn read_footprint() -> Footprint {
    let mut footprint = Footprint {
        graphics_dirty: "?".into(),
        graphics_swapped: "?".into(),
        graphics_regions: "?".into(),
        phys_footprint: "?".into(),
        phys_footprint_peak: "?".into(),
    };

    // vmmap is the only thing that reports GPU-owned pages, and it is macOS
    // only. Elsewhere the cache's own byte total is all this reports.
    let pid = std::process::id();
    let Ok(out) = Command::new("vmmap")
        .args(["-summary", &pid.to_string()])
        .output()
    else {
        return footprint;
    };
    let text = String::from_utf8_lossy(&out.stdout);

    for line in text.lines() {
        if line.contains("owned unmapped (graphics)") {
            // VIRTUAL RESIDENT DIRTY SWAPPED ... REGION_COUNT
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 8 {
                footprint.graphics_dirty = cols[cols.len() - 6].to_string();
                footprint.graphics_swapped = cols[cols.len() - 5].to_string();
                footprint.graphics_regions = cols[cols.len() - 1].to_string();
            }
        } else if let Some(rest) = line.trim().strip_prefix("Physical footprint:") {
            footprint.phys_footprint = rest.trim().to_string();
        } else if let Some(rest) = line.trim().strip_prefix("Physical footprint (peak):") {
            footprint.phys_footprint_peak = rest.trim().to_string();
        }
    }

    footprint
}

fn mib(bytes: usize) -> String {
    format!("{:.0}M", bytes as f64 / (1024.0 * 1024.0))
}

struct Harness {
    images: Images,
    jobs: notedeck::MediaJobs,
    job_pool: JobPool,
    frame: usize,
    total_frames: usize,
}

impl eframe::App for Harness {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let pass_nr = ctx.cumulative_pass_nr();

        self.jobs.run_received(&mut self.job_pool, |id| {
            notedeck::run_media_job_pre_action(id, &mut self.images.textures, pass_nr);
        });
        self.jobs.deliver_all_completed(|completed| {
            notedeck::deliver_completed_media_job(completed, &mut self.images.textures, pass_nr)
        });

        self.images.textures.evict_over_budget(pass_nr);

        // Scroll: each frame the visible window slides forward by one image, so
        // after IMAGE_COUNT frames every image has been on screen once.
        let offset = self.frame % IMAGE_COUNT;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for i in 0..WINDOW {
                    let url = url_for((offset + i) % IMAGE_COUNT);
                    let state = self.images.textures.static_image.get_or_request(
                        self.jobs.sender(),
                        ctx,
                        &url,
                        ImageType::Content(None),
                    );
                    let size = egui::vec2(48.0, 48.0);
                    if let TextureState::Loaded(tex) = state {
                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                            tex.id(),
                            size,
                        )));
                    } else {
                        ui.allocate_space(size);
                    }
                }
            });
        });

        if self.frame.is_multiple_of(32) {
            let fp = read_footprint();
            println!(
                "frame {:5} pass {:5} | cache {:>6} in {:4} textures | graphics dirty {:>6} swapped {:>6} regions {:>6} | phys_footprint {:>8} peak {:>8}",
                self.frame,
                pass_nr,
                mib(self.images.textures.loaded_bytes()),
                self.images.textures.loaded_count(),
                fp.graphics_dirty,
                fp.graphics_swapped,
                fp.graphics_regions,
                fp.phys_footprint,
                fp.phys_footprint_peak,
            );
        }

        self.frame += 1;
        if self.frame >= self.total_frames {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result {
    let budget: usize = std::env::var("TEXBUDGET_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128 * 1024 * 1024);
    let total_frames: usize = std::env::var("TEXBUDGET_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1200);

    let dir = std::env::temp_dir().join(format!("texture_budget_{}", std::process::id()));
    let img_dir = dir.join(MediaCache::rel_dir(MediaCacheType::Image));
    std::fs::create_dir_all(&img_dir).expect("cache dir");

    println!(
        "writing {IMAGE_COUNT} {IMAGE_SIZE}x{IMAGE_SIZE} images to {}",
        img_dir.display()
    );
    for i in 0..IMAGE_COUNT {
        MediaCache::write(&img_dir, &url_for(i), generate_image(i)).expect("write image");
    }

    let (send, recv) = channel();
    let mut images = Images::new(dir.clone());
    // A budget of 0 means "no budget": reproduces the pre-eviction behavior.
    images
        .textures
        .set_budget(if budget == 0 { usize::MAX } else { budget });

    println!(
        "budget {} | {IMAGE_COUNT} images x {} = {} if nothing is evicted",
        if budget == 0 {
            "unbounded".to_string()
        } else {
            mib(budget)
        },
        mib(IMAGE_SIZE * IMAGE_SIZE * 4),
        mib(IMAGE_COUNT * IMAGE_SIZE * IMAGE_SIZE * 4),
    );

    let harness = Harness {
        images,
        jobs: JobCache::new(recv, send),
        job_pool: JobPool::new(2),
        frame: 0,
        total_frames,
    };

    let res = eframe::run_native(
        "texture budget",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(harness))),
    );

    std::fs::remove_dir_all(&dir).ok();
    res
}
