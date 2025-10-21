# Change Log
All notable changes to this project will be documented in this file.
This project adheres to [Semantic Versioning](http://semver.org/).

## v0.3.2 (2025-06-15)

- Depend on wasm-bindgen 0.2.89 or higher
- Fix panic when prediction calculation is invalid

## v0.3.1 (2024-01-13)

- Added a WASM SIMD implementation.
- Allow reading the XMP packet.
- Admit more precision values in JPEG Lossless.

## v0.3.0 (2022-10-17)

- The MSRV policy is now managed by the `rust-version` field in `Cargo.toml`.
- The color transform can now be overridden as well as hinted with
  `Decoder::set_color_transform`.

## v0.2.6 (2022-05-09)

- Another fix to allow usage in WASM target.
- Decoding in the WASM target is now actively tested in CI.

## v0.2.5 (2022-05-02)

- Fix single threaded usage in WASM target.

## v0.2.4 (2022-04-01)

- Corrects minimal version requirements of dependency `rayon`.

## v0.2.3 (2022-02-14)

- Added `Decoder::set_max_decoding_buffer_size` which limits the bytes
  allocated for the output of the decoding process.
- Added Arm64-Neon intrinsic implementation of idct and color conversion. This
  depends on a Rust nightly compiler feature ([`aarch64_target_feature`]) and
  it must be explicitly enabled. As soon as the minimum supported Rust version
  includes the stabilization of this feature, the code will be enabled by
  default and the feature changed to do nothing.

[`aarch64_target_feature`]: https://github.com/rust-lang/rust/issues/90620

## v0.2.2 (2022-02-12)

- Added and SSE3-specific SIMD intrinsic implementation for idct and color
  conversion. It will run if applicable targets are detect at _runtime_.
- The SIMD implementation is not bit-for-bit compatible with non-SIMD output.
  You can enable the `platform_independent` feature, to ensure that only
  bit-for-bit equivalent code runs and output is the same on all platforms.
- Improved performance some more by avoiding bounds checks with array types.
- Multithreading is now used more frequently, without the rayon target, except
  on an explicit list of unsupported platforms.

## v0.2.1 (2022-12-09)

- Fix decoding error due to conflict of lossless with some spectral selections.

## v0.2.0 (2021-12-04)

- Added Lossless JPEG support
- Added support for EXIF and ICC data
- Minimum supported rust version changed to 1.48 and no formal policy for bump releases for now
- Minor stability fixes on invalid jpeg images

## v0.1.22 (2021-01-27)

- Fix panic on jpeg without frames.

## v0.1.21 (2021-01-23)

- Fix incorrect order of MCUs in non-interleaved streams
- DCT Progressive images with incomplete coefficient blocks are now rendered
- Fix a panic on invalid dimensions
- Reduce allocations and runtime of decoding
- Rework multi-threading to run a thread per component

## v0.1.20 (2020-07-04)

- Fix decoding of some progressive images failing
- Several more performance improvements
- Add `PixelFormat::pixel_bytes` to determine the size of pixels
- Cleanup and clarification of the 8x8 idct implementation
- Updated fuzzing harnesses and helpers

## v0.1.19 (2020-04-27)
- Fix decoding returning too much image data
- Fix recognizing padding in marker segments
- Several decode performance improvements
- Remove use of deprecated `Error::description`

## v0.1.18 (2019-12-10)
- Fix two bugs causing panics introduced in 0.1.17.

## v0.1.17 (2019-12-08)
- Minimum supported rust version changed to 1.34
- Fix clippy::into_iter_on_array warning
- Ignore extraneous bytes after SOS
- Support IDCT Scaling

## v0.1.16 (2019-08-25)
- Minimum supported rust version changed to 1.28
- Allow zero length DHT segments

## v0.1.15 (2018-06-10)
- Added support for WebAssembly and asm.js (thanks @CryZe!)
- Bugfix for images with APP14 segments longer than 12 bytes.

## v0.1.14 (2018-02-15)
- Updated `rayon` to 1.0.

## v0.1.13 (2017-06-14)
- Updated `rayon` to 0.8.

## v0.1.12 (2017-04-07)
- Fixed an integer overflow in `derive_huffman_codes`.
- Updated `rayon` to 0.7.

## v0.1.11 (2017-01-09)
- Fixed an integer overflow.
- Updated `byteorder` to 1.0.

## v0.1.10 (2016-12-23)
- Updated `rayon` to 0.6

## v0.1.9 (2016-12-12)
- Added a generic integer upsampler, which brings support for some unusual subsampling schemes, e.g. 4:1:1 (thanks @iamrohit7!)
- Made rayon optional through the `rayon` cargo feature (thanks @jackpot51!)

## v0.1.8 (2016-11-05)
* Updated rayon to version 0.5.

## v0.1.7 (2016-10-04)
- Added `UnsupportedFeature::NonIntegerSubsamplingRatio` error
- Fixed a bug which could cause certain images to fail decoding
- Fixed decoding of JPEGs which has a final RST marker in their entropy-coded data
- Avoid allocating coefficients when calling `read_info()` on progressive JPEGs

## v0.1.6 (2016-07-12)
- Added support for 16-bit quantization tables (even though the JPEG spec explicitly
  states "An 8-bit DCT-based process shall not use a 16-bit precision quantization table",
  but since libjpeg allows it there is little choice...)
- Added support for decoding files with extraneous data (this violates the JPEG spec, but libjpeg allows it)
- Fixed panic when decoding files without SOF
- Fixed bug which caused files with certain APP marker segments to fail decoding

## v0.1.5 (2016-06-22)
- Removed `euclid` and `num-rational` dependencies
- Updated `rayon` to 0.4

## v0.1.4 (2016-04-20)
- Replaced `num` with `num-rational`

## v0.1.3 (2016-04-06)
- Updated `byteorder` to 0.5

## v0.1.2 (2016-03-08)
- Fixed a bug which was causing some progressive JPEGs to fail decoding
- Performance improvements

## v0.1.1 (2016-02-29)
- Performance improvements

## v0.1.0 (2016-02-13)
- Initial release
