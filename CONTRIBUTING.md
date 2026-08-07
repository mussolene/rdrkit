# Contributing

## Development setup

Install the stable Rust toolchain, then run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --locked
```

No proprietary binaries, confidential disk images, extracted customer data,
or reverse-engineered source code may be committed. Tests must use synthetic
fixtures or independently redistributable data.

## Changes

- Open an issue before large format or public-API changes.
- Keep source-image access read-only.
- Add tests for parser changes and malformed input.
- Add lifecycle tests for session-state changes and cleanup behavior.
- Exercise platform-specific branches on their native host or in a matching
  Linux container before requesting review.
- Use focused commits following Conventional Commits, for example
  `fix(parser): reject truncated chunk index`.
- Update `CHANGELOG.md` for user-visible behavior.

## Releases

Versions follow Semantic Versioning. A maintainer updates the version and
changelog, merges a release commit, and pushes an annotated `vX.Y.Z` tag. The
release workflow builds and publishes every supported binary and checksums.
See [docs/releasing.md](docs/releasing.md) for the complete checklist.
