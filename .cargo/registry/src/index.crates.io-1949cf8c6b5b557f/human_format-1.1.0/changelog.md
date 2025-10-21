# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](http://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.0] - 2024-02-16

### Changed

- Format check included in build
- Improve error handling in try_parse with better ergonomics - [PR#19]https://github.com/BobGneu/human-format-rs/pull/9 by [@jgrund](https://github.com/jgrund)

### Removed

- removed Travis & Appveyor

## [1.0.3] - 2019-11-23

### Fixed

- Removed unnecessary logging - [PR#9](https://github.com/BobGneu/human-format-rs/pull/9) by [@jaysonsantos](https://github.com/jaysonsantos)
- Corrected binary base to 1024

## [1.0.2] - 2018-02-01

### Fixed

- Corrected issue with API, expecting owned strings when the common occurance will be references.

## [1.0.1] - 2018-01-28

### Added

- Updated Documentation to improve utility of [docs.rs](https://docs.rs/crate/human_format/)
- Added fmt to build scripts

## [1.0.0] - 2018-01-28

Initial Release

[unreleased]: https://github.com/BobGneu/human-format-rs/compare/master...develop
[1.1.0]: https://github.com/BobGneu/human-format-rs/compare/1.0.3...1.1.0
[1.0.3]: https://github.com/BobGneu/human-format-rs/compare/1.0.2...1.0.3
[1.0.2]: https://github.com/BobGneu/human-format-rs/compare/1.0.1...1.0.2
[1.0.1]: https://github.com/BobGneu/human-format-rs/compare/1.0.0...1.0.1
[1.0.0]: https://github.com/BobGneu/human-format-rs/tree/1.0.0
