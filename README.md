# rdrkit

[![CI](https://github.com/mussolene/rdrkit/actions/workflows/ci.yml/badge.svg)](https://github.com/mussolene/rdrkit/actions/workflows/ci.yml)
[![Security](https://github.com/mussolene/rdrkit/actions/workflows/security.yml/badge.svg)](https://github.com/mussolene/rdrkit/actions/workflows/security.yml)
[![Release](https://img.shields.io/github/v/release/mussolene/rdrkit?display_name=tag)](https://github.com/mussolene/rdrkit/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

`rdrkit` provides read-only access to disk objects stored in indexed R-Drive
Image `.rdr` archives. It reads the archive's embedded chunk directory, exposes
one object as a seekable raw file over localhost NFSv3, and lets standard
operating-system tools attach the filesystem.

The complete raw disk is **not** written to temporary storage. Only chunks
requested by the operating system are read and decompressed.

> [!WARNING]
> This is an independent, clean-room implementation for data recovery and
> interoperability. It is not affiliated with, endorsed by, or supported by
> R-Tools Technology. Always keep the source image immutable and mount restored
> filesystems read-only.

## Why rdrkit exists

RDR images can contain large NTFS, FAT, or other filesystem objects, but the
original mounting software is not available on every operating system and may
depend on kernel drivers. `rdrkit` keeps the proprietary container concern
small: it translates indexed RDR chunks into a normal read-only byte stream,
then delegates filesystem handling to mature host tools.

## Support matrix

| Host | Architecture | Release binary | Attachment path |
| --- | --- | --- | --- |
| macOS | Apple Silicon (`aarch64`) | Yes | NFSv3 → `hdiutil` |
| macOS | Intel (`x86_64`) | Yes | NFSv3 → `hdiutil` |
| Linux | ARM64 (`aarch64`) | Yes | NFSv3 → read-only loop mount |
| Linux | x86-64 | Yes | NFSv3 → read-only loop mount |

The source RDR archive is never opened for writing.

## Quick start

### 1. Install

Download the archive for your platform from
[GitHub Releases](https://github.com/mussolene/rdrkit/releases), verify it with
`SHA256SUMS`, then install the binary:

```sh
tar -xzf rdrkit-v0.1.0-<target>.tar.gz
chmod +x rdrkit
sudo install -m 0755 rdrkit /usr/local/bin/rdrkit
```

Or build from source:

```sh
git clone https://github.com/mussolene/rdrkit.git
cd rdrkit
cargo build --release --locked
sudo install -m 0755 target/release/rdrkit /usr/local/bin/rdrkit
```

### 2. List objects

This reads the embedded archive directory and compact indexes; it does not scan
the complete image:

```sh
rdrkit list backup.rdr
```

Example output:

```text
image=backup.rdr size=459.00 GiB
object=0 size=499.00 MiB chunks=1996 chunk_size=256.00 KiB
object=1 size=512.00 MiB chunks=2048 chunk_size=256.00 KiB
object=2 size=128.00 MiB chunks=512 chunk_size=256.00 KiB
object=3 size=929.31 GiB chunks=3806464 chunk_size=256.00 KiB
```

### 3. Serve an object

Keep this process running while the volume is attached:

```sh
rdrkit serve backup.rdr --object 3 --listen 127.0.0.1:11111
```

The NFS export contains one read-only virtual file named `object-3.raw`.

## Mount on macOS

In a second terminal:

```sh
mkdir -p "$HOME/Library/Caches/rdrkit/object-3"

mount_nfs \
  -o nolocks,vers=3,tcp,rsize=131072,actimeo=1,port=11111,mountport=11111 \
  localhost:/ "$HOME/Library/Caches/rdrkit/object-3"

hdiutil attach \
  -readonly \
  -nomount \
  -imagekey diskimage-class=CRawDiskImage \
  "$HOME/Library/Caches/rdrkit/object-3/object-3.raw"
```

`hdiutil` prints a device such as `/dev/disk7`. Mount it read-only:

```sh
diskutil mount readOnly /dev/disk7
```

Detach in reverse order before stopping `rdrkit`:

```sh
diskutil unmount /dev/disk7
hdiutil detach /dev/disk7
umount "$HOME/Library/Caches/rdrkit/object-3"
```

No third-party macOS kernel extension is required.

## Mount on Linux

Linux requires the NFS client, loop-device support, and filesystem support for
the contained volume. Package names vary by distribution.

```sh
sudo mkdir -p /mnt/rdrkit-nfs /mnt/rdr-volume

sudo mount -t nfs \
  -o ro,nolock,vers=3,tcp,port=11111,mountport=11111 \
  127.0.0.1:/ /mnt/rdrkit-nfs

LOOP_DEVICE=$(sudo losetup --find --show --read-only /mnt/rdrkit-nfs/object-3.raw)
sudo mount -o ro "$LOOP_DEVICE" /mnt/rdr-volume
```

Unmount before stopping `rdrkit`:

```sh
sudo umount /mnt/rdr-volume
sudo losetup --detach "$LOOP_DEVICE"
sudo umount /mnt/rdrkit-nfs
```

If automatic filesystem detection is unavailable, specify the filesystem type,
for example `sudo mount -t ntfs3 -o ro ...`.

## Commands

```text
rdrkit list IMAGE.rdr
    List indexed objects quickly.

rdrkit serve IMAGE.rdr --object N [--listen 127.0.0.1:11111]
    Export one object as a seekable read-only raw file over NFSv3.

rdrkit extract IMAGE.rdr --object N --output OBJECT.raw [--force]
    Materialize one sparse raw object. This can require substantial storage.

rdrkit inspect IMAGE.rdr [--max-records N]
    Low-level sequential diagnostics for format research.
```

Run `rdrkit --help` or `rdrkit <command> --help` for current options.

## How it works

```text
RDR footer
    ↓ directory pointer
archive directory (object id → index frame)
    ↓ zlib
compact chunk index (logical chunk → record offset and length)
    ↓ on-demand read
raw or zlib RDR data record
    ↓
seekable virtual .raw file over localhost NFSv3
    ↓
hdiutil (macOS) or loop device (Linux)
    ↓
host filesystem driver, mounted read-only
```

The implementation validates:

- outer file size and record magic;
- footer directory pointer and directory bounds;
- object id, compact-index encoding, geometry, and monotonic offsets;
- sampled first, middle, and last records before serving;
- every requested record's logical offset and decoded length.

There is no linear-scan fallback in the serving path. A missing, malformed, or
unsupported compact index produces an explicit error.

See [docs/rdr-format.md](docs/rdr-format.md) for the clean-room format notes,
record layouts, and the exact reader-selection logic.

## Current limitations

- Read-only operation only.
- Encrypted, incremental, split, and unknown RDR variants are not supported.
- The current frontend serves one object per process.
- Linux loop mounting requires host privileges and compatible filesystem tools.
- `inspect` and `extract` are diagnostic paths; `serve` uses the embedded index.

Support is intentionally conservative: the tool rejects formats it cannot
validate instead of returning potentially corrupted bytes.

## Security and privacy

- No telemetry or network service is enabled beyond the explicitly requested
  localhost listener.
- The NFS listener defaults to `127.0.0.1`; do not expose it to an untrusted
  network.
- Disk images, raw files, private keys, logs, and local environment files are
  excluded by `.gitignore`.
- CI runs secret scanning, dependency vulnerability checks, and license/source
  policy checks.

See [SECURITY.md](SECURITY.md) for private vulnerability reporting.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --locked
cargo build --release --locked
gitleaks detect --source . --no-banner --redact
```

Contributions must use synthetic or redistributable test data. Do not submit
proprietary binaries or disk images containing private information. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## Versioning and releases

The project follows [Semantic Versioning](https://semver.org/). Tags use
`vMAJOR.MINOR.PATCH`. Every tag builds and publishes these archives:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`

Each GitHub release also contains `SHA256SUMS`. Release details are maintained
in [CHANGELOG.md](CHANGELOG.md).

## License

Licensed under the [MIT License](LICENSE).
