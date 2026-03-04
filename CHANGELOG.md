# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- "Connecting to remote cluster" screen no longer appears on launch when using provisioned mode without manual peers.

## [0.1.6] - 2026-02-24

### Fixed
- Tray icon now bundled in DEB/RPM packages (fixes wrong icon color on non-Flatpak installs).

### Changed
- Flatpak socket changed to `fallback-x11`.
- Added `just release` recipe for automated release builds.

## [0.1.5] - 2026-02-20

### Changed
- Flatpak notifications now use the Notification portal instead of direct D-Bus.
- License changed to GPL-3.0-or-later.

### Fixed
- Autostart command was double-wrapped by the Background portal.
- Window was invisible when starting minimized and then showing from tray.

## [0.1.1] - 2026-02-19

### Fixed
- Device join/leave notifications are more reliable.

## [0.1.0] - 2026-02-18

### Added
- Initial release.
