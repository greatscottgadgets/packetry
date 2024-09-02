# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
## [Unreleased]
-->

## [0.2.2] - 2024-09-02

### Added

- Add fuzzer to help find decoder bugs.
- Document clearing of Traffic and Device panes.
- Document both functions of Stop button.

### Changed

- Clean up GObject subclasses.
- Implement iterators for stream types, speeding up file saving.

### Fixed

- Treat SETUP packets with non-zero EP num as indicating OUT direction.
- Don't try to find the endpoint for a malformed packet.
- Add libharfbuzz to Linux AppImage, fixing symbol lookup error.


## [0.2.1] - 2024-08-15

### Changed

- Update documentation for 0.2.0.

### Fixed

- Use 24-bit rather than 16-bit increments for timestamps, fixing slow file
  save.


## [0.2.0] - 2024-08-13

### Added

- Add detail pane.
- Add packetry-cli wrapper program, enabling command-line options on Windows.
- Add Linux AppImage build.
- Use usb.ids database to interpret various ID values.
- Use GIO File abstraction, supporting file operations over MTP or SMB, for
  example.
- Add information about command line options to Application instance.

### Changed
- Bump nusb dependency to 0.1.10 and remove workaround for 0.1.9.
- Handle opening files in the standard way for a GTK application.

### Fixed
- Avoid underflow in UI code when capture is completely empty.
- Validate packet CRCs and lengths, and diagnose malformed packets.


## [0.1.0] - 2024-07-16

### Added

- Initial release


[Unreleased]: https://github.com/greatscottgadgets/packetry/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/greatscottgadgets/packetry/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/greatscottgadgets/packetry/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/greatscottgadgets/packetry/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/greatscottgadgets/packetry/releases/tag/v0.1.0
