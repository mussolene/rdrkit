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

`tests/fixtures/empty-disk-zlib.rdr` and `empty-disk-raw.rdr` are deterministic
synthetic containers. Each contains one zero-filled 1 MiB logical disk split
into 256 KiB chunks. The first uses zlib-compressed data records and the second
uses raw data records. Their compact chunk indexes use the matching zlib and
raw encodings as well. Neither contains a filesystem or user data. Regenerate
both with:

```sh
cargo test regenerate_empty_disks_fixture -- --ignored
```

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
