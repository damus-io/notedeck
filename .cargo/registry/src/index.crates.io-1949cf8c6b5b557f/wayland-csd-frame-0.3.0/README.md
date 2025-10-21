# wayland-csd-frame

`wayland-csd-frame` aims to provide common client side decorations (CSD) frame
for xdg-shell Wayland windows establishing a stable interface between windowing
libraries (e.g winit) and decorations drawing libraries.

This library defines a simple interface other crates providing decoration
frames could use to integrate with crates like winit. An example of crates
using this interface to provide decorations frame:

- https://github.com/PolyMeilex/sctk-adwaita an Adwaita-like frame.
- https://github.com/smithay/client-toolkit provides bare bones `FallbackFrame`. 