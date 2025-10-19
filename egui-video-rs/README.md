# egui-video, a video playing library for [`egui`](https://github.com/emilk/egui)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/n00kii/egui-video/blob/main/README.md)

plays videos in egui from file path or from bytes using pure Rust dependencies

This project was forked from https://github.com/n00kii/egui-video which used ffmpeg and sdl2,
and modified to use pure Rust libraries.

This is not a performant implementation of video playback, as it renders each frame to an `egui::ColorImage`, and all
video decoding is done on the CPU. However it does provide a simple way to add video playback to an egui application.

## usage:
```rust
/* called once (top level initialization) */

{ // if using audio...
    let audio_sys = sdl2::init()?.audio()?;
    let audio_device = egui_video::init_audio_device(&audio_sys)?;

    // don't let audio_device drop out of memory! (or else you lose audio)

    add_audio_device_to_state_somewhere(audio_device);
}
```
```rust
/* called once (creating a player) */

let mut player = Player::new(ctx, my_media_path)?;

{ // if using audio...
    player = player.with_audio(&mut my_state.audio_device)
}
```
```rust
/* called every frame (showing the player) */
player.ui(ui, [player.width as f32, player.height as f32]);
```
### current caveats
 - need to compile in `release` or `opt-level=3` otherwise limited playback performance
 - ~~bad (playback, seeking) performance with large resolution streams~~
 - ~~seeking can be slow (is there better way of dropping packets?)~~
 - ~~depending on the specific stream, seeking can fail and mess up playback/seekbar (something to do with dts?)~~
 - ~~no audio playback~~
