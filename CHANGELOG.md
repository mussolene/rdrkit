# Changelog

All notable changes are documented here. The project follows
[Semantic Versioning](https://semver.org/) and uses Git tags in the form
`vMAJOR.MINOR.PATCH`.

## [Unreleased]

### Planned

- One-command mount and unmount orchestration.
- Additional RDR variants and integrity metadata.

## [0.1.0] - 2026-07-14

### Added

- Read-only parsing of indexed RDR archives.
- Footer-directed archive-directory lookup without a linear image scan.
- Compact chunk-index support for raw and zlib-compressed records.
- Fast object listing, sparse raw extraction, and localhost NFSv3 serving.
- macOS and Linux operating instructions.
- CI, release binaries, dependency auditing, license checks, and secret scans.

[Unreleased]: https://github.com/mussolene/rdrkit/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/mussolene/rdrkit/releases/tag/v0.1.0
