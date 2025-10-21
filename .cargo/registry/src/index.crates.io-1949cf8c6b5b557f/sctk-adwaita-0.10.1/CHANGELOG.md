## [Unreleased]

## 0.10.1
- Panic hardening of ab-glyph (#64)

## 0.10.0
- Bump sctk to 0.19

## 0.9.0
- Bump crossfont to 0.8.0 (#60)

## 0.8.1
- Fix `ab_glyph` renderer panicking with integer scale factor 3 (#50)
- Improved roundness of headerbar (#51)

## 0.8.0
- **Breaking:** `AdwaitaFrame::new` now takes `Arc<CompositorState>` as an argument
- Fix leftmost title pixel sometimes being cut off (#45)
- Fix transparency in ab_glyph renderer (#44)
- Extended resize corners (#47)
- Center maximize icon (#46)
- Window shadows (#43)

### Dependencies updates
- Bump crossfont to 0.6.0 (#52)

## 0.7.0
- **Breaking:** `wayland-csd-frame` is now used as a part of the public interface.

## 0.6.1
- Bump tiny-skia to v0.11 (#32) 
- cleanup: Remove debug println (#29) 
- Support custom header buttons layouts (#30)
- The double click threshold value was raised to `400ms`

## 0.6.0
- Update the `smithay-client-toolkit` to `0.17.0`
- Don't use tiny-skia's default features

## 0.5.4
- Timeout dbus call to settings portal (100ms)

## 0.5.3
- `ab_glyph` titles will read the system title font using memory mapped buffers instead of reading to heap.
  Lowers RAM usage.
- Improve titlebar-font config parsing to correctly handle more font names.

## 0.5.2
- `ab_glyph` & `crossfont` titles will use gnome "titlebar-font" config if available.
- `ab_glyph` titles are now more consistent with `crossfont` titles both using system sans
  if no better font config is available.
- Rounded corners are now disabled on maximized and tiled windows.
- Double click interval is now 400ms (as previous 1s interval was caused by bug fixed in 0.5.1)

## 0.5.1
- Use dbus org.freedesktop.portal.Settings to automatically choose light or dark theming.
- Double click detection fix.
- Apply button click on release instead of press.

## 0.5.0
- `title` feature got removed
- `ab_glyph` default feature got added (for `ab_glyph` based title rendering)
- `crossfont` feature got added (for `crossfont` based title rendering)
    - Can be enable like this: 
        ```toml
        sctk-adwaita = { default-features = false, features = ["crossfont"] }
        ```
