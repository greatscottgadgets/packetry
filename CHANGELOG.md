# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
## [Unreleased]
-->

## [0.5.0] - 2025-09-29

### Added

- Add support for non-packet events from Cynthion analyzer.
- Add support for VBUS power control with Cynthion analyzer.
- Add support for the PcapNG file format with custom non-packet blocks.
- Add decoding of HID report descriptors.
- Add facility to dump the capture database for debugging.

### Changed

- The device list is maintained automatically and the scan button is removed.
- USB capture devices are kept open whilst selected for use.
- Filename extensions are added automatically when saving.
- File dialogs remember the last used directory.
- Backtraces are always captured when an error occurs.
- Panics in worker threads result in an error dialog rather than a crash.
- UI updates are made from a snapshot of the database state.
- Capture backends have been ported to the nusb 0.2 API.
- Most of the UI is now defined in XML and editable with Cambalache.
- Source code has been significantly reorganised.

### Fixed

- Stop descriptors from being intermingled when a device address is reused.
- Fix a decoder crash when a double SETUP packet is seen.
- Fix crashes in GTK where the view could get out of sync with the model.
- Fix descriptions of invalid groups leaking internal details.
- Fix inconsistent pane sizing when window is first shown.
- Fix units in text shown in progress bar.
- Handle runtime GTK version mismatches gracefully.


## [0.4.0] - 2024-10-29

### Added

- Add context menu system and options to save data.
- Add initial support for HID descriptors.


## [0.3.0] - 2024-10-21

### Added

- Add keyboard shortcuts.
- Add transaction-level and packet-level views of capture.
- Update documentation for multiple traffic views.
- Add backend API for USB capture devices.
- Add support for iCE40-usbtrace capture device.
- Display all descriptor types in device view.
- Add connecting lines to test output.

### Fixed

- Fix handling of alternate interface settings.
- Handle descriptors that are longer than defined in the specification.
- Fix interpretation of isochronous transactions, including ambiguous cases.


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


[Unreleased]: https://github.com/greatscottgadgets/packetry/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/greatscottgadgets/packetry/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/greatscottgadgets/packetry/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/greatscottgadgets/packetry/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/greatscottgadgets/packetry/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/greatscottgadgets/packetry/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/greatscottgadgets/packetry/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/greatscottgadgets/packetry/releases/tag/v0.1.0
