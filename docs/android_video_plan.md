# Android Video Backend Plan

## Goals

- Use Android’s native MediaCodec stack for H.264/H.265 hardware decode.
- Keep the existing `egui_video::Player` API surface untouched so the UI code in `crates/notedeck_ui/src/note/video.rs` continues to work without conditional logic.
- Support inline playback at 60 fps and fullscreen scaling, matching the current desktop experience.
- Leave audio plumbing optional in the first iteration; video playback must not regress if audio is unavailable.

## Building Blocks

- [`ndk::media::MediaExtractor`](https://docs.rs/ndk/latest/ndk/media/struct.MediaExtractor.html) to demux remote MP4s.
- [`ndk::media::MediaCodec`](https://docs.rs/ndk/latest/ndk/media/struct.MediaCodec.html) (decoder mode) to feed compressed samples into the hardware decoder.
- [`ndk::media::image_reader::ImageReader`](https://docs.rs/ndk/latest/ndk/media/image_reader/struct.ImageReader.html) configured for `RGBA_8888` to receive decoded frames via a Surface.
- A decode worker thread that:
  - Pulls samples with `MediaExtractor::read_sample_data`.
  - Queues them into the codec with `dequeue_input_buffer` / `queue_input_buffer`.
  - Releases output buffers with `release_output_buffer(..., render = true)` so the ImageReader sees the produced frame.
- A frame bridge that calls `ImageReader::acquire_latest_image`, copies plane 0 into an `egui::ColorImage`, and updates the texture handle.

## API Shape

```text
AndroidPlayer
 ├── DecodeSession (Arc<Mutex<...>>)
 │     ├── MediaExtractor
 │     ├── MediaCodec
 │     ├── ImageReader
 │     ├── frame_queue: lock-free ring
 │     ├── state: PlayerState cache
 │     └── threads: decode + image pump
 ├── TextureHandle (egui)
 ├── PlayerState cache (mirrors desktop behaviour)
 └── Drop impl that tears down threads & closes codec
```

`Player::ui` will:

1. Pull the latest frame from `frame_queue`.
2. Upload it via `ctx.tex_manager`.
3. Render with the existing button chrome.

## Minimum Android API Level

- Requires API 26+ for `ImageReader::new_with_usage` and hardware buffer usage flags.
- We enable the `ndk` crate features: `["media", "api-level-29"]` so we get the modern APIs while staying compatible with our current minSdk (19) for other modules—video code is guarded behind `#[cfg(target_os = "android")]`.

## Outstanding Questions

- Audio: we can reuse SDL via `sdl-android` or switch to `cpal/oboe`. For the first iteration we publish video-only playback and keep the audio stub (`Player::with_audio` returns `Ok(self)` but no-op).
- Texture uploads: currently performed on the UI thread. If necessary we can introduce a staging texture double-buffer to avoid blocking on `acquire_latest_image`.

## Current Status

- MediaCodec + ImageReader backend scaffolding is in place and gated behind `target_os = "android"`.
- Frames are copied into `egui::ColorImage` and uploaded each frame; audio is stubbed out (`AudioDevice = ()`).
- Pause/resume and stop commands are supported, but seeking and looping still need implementation.
- SDL-based audio initialisation in `notedeck_ui` succeeds because the backend now advertises a no-op `init_audio_device`.
- No automated tests yet; exercise manually via `cargo ndk --target arm64-v8a run` once the Android build finishes wiring the JNI surface.

## Next Steps

1. Create `egui-video-rs/src/android.rs` with the scaffolding.
2. Update `lib.rs` to select `android` backend.
3. Extend Cargo manifests with `ndk` dependency and feature flags.
4. Implement decode thread + ImageReader bridge.
5. Wire into `notedeck_ui` (drops the stub on Android).
6. Document Gradle/CI requirements (NDK, codec availability).
