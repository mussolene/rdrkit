# Changelog

All notable changes are documented here. The project follows
[Semantic Versioning](https://semver.org/) and uses Git tags in the form
`vMAJOR.MINOR.PATCH`.

## [Unreleased]

## [0.2.0-rc.1] - 2026-08-07

### Added

- One-command read-only mount orchestration on macOS and Linux.
- Managed mount sessions with `mount`, `unmount`, and `status` commands.
- Interactive object selection when an image contains several indexed objects.
- Dynamic localhost NFS ports and readiness handshakes.
- Recoverable session state for interrupted mount and unmount operations.
- Public image and object metadata types for future frontends.
- End-to-end synthetic coverage of the released NFS server lifecycle.

### Changed

- Split the executable entrypoint from the reusable Rust library.
- Made managed macOS mounts discover and mount volumes from a read-only attached disk.
- Made managed Linux mounts discover filesystems through read-only loop devices with partition scanning.
- Restricted session metadata to the current user and documented the host filesystem-parser threat model.

### Planned

- Lightweight macOS application and `.rdr` file association.
- Additional RDR variants and integrity metadata.

### Known limitations

- Native privileged Linux mount and interrupted-cleanup smoke tests remain
  required before the stable `v0.2.0` release.

## [0.1.0] - 2026-07-14

### Added

- Read-only parsing of indexed RDR archives.
- Footer-directed archive-directory lookup without a linear image scan.
- Compact chunk-index support for raw and zlib-compressed records.
- Fast object listing, sparse raw extraction, and localhost NFSv3 serving.
- macOS and Linux operating instructions.
- CI, release binaries, dependency auditing, license checks, and secret scans.

[Unreleased]: https://github.com/mussolene/rdrkit/compare/v0.2.0-rc.1...HEAD
[0.2.0-rc.1]: https://github.com/mussolene/rdrkit/compare/v0.1.0...v0.2.0-rc.1
[0.1.0]: https://github.com/mussolene/rdrkit/releases/tag/v0.1.0
