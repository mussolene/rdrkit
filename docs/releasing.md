# Release process

Releases follow Semantic Versioning and are produced from annotated Git tags.
GitHub Actions is the only supported binary publication path.

## Prepare

1. Confirm the `main` branch is green.
2. Select the next `MAJOR.MINOR.PATCH` version according to SemVer.
3. Update `package.version` in `Cargo.toml`.
4. Run `cargo check --locked` so `Cargo.lock` is current.
5. Move relevant entries from `Unreleased` into a dated changelog section.
6. Verify that README commands and the support matrix still match the release.

## Validate

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo build --release --locked
cargo audit --deny warnings
cargo deny check
actionlint
gitleaks detect --source . --no-banner --redact
```

Do not release if any check is skipped or failing.

For a release that changes mount orchestration, also perform these host checks:

1. On macOS, mount a representative indexed image, confirm every reported
   volume is read-only, run `rdrkit status`, and unmount by session id.
2. On Linux, repeat the managed workflow with a partitioned image and confirm
   that `losetup --partscan` discovers the expected filesystem-bearing slices.
3. Interrupt one mount or unmount operation, confirm `status` reports the saved
   session, and verify that retrying `unmount` completes cleanup.
4. Confirm the localhost listener, NFS mount, raw device, filesystem mounts,
   and server process are all gone after cleanup.

Do not substitute a successful `serve` test for the native mount checks. CI can
validate parsing, process startup, and platform compilation, but it does not
exercise privileged host mount operations.

## Tag

Create a focused release commit using Conventional Commits, then an annotated
tag whose version exactly matches `Cargo.toml`:

```sh
git commit -m "chore(release): vX.Y.Z"
git tag -a vX.Y.Z -m "rdrkit vX.Y.Z"
git push origin main
git push origin vX.Y.Z
```

## Publish

The `Release` workflow builds natively on four GitHub-hosted runners:

- macOS ARM64;
- macOS Intel;
- Linux ARM64;
- Linux x86-64.

Each job packages the binary with the README, changelog, and license. The final
job downloads every archive, creates `SHA256SUMS`, and publishes one GitHub
Release containing all artifacts.

## Verify

1. Confirm all four build jobs succeeded.
2. Download every archive from the release page.
3. Verify `SHA256SUMS`.
4. Run `rdrkit --version` on at least one macOS and one Linux host.
5. Confirm the release badge and changelog links resolve.

If publication fails, fix the workflow or source and create a patch version. Do
not replace binaries attached to an existing immutable release tag.
