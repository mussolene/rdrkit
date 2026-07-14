# RDR format notes

This document describes only behavior independently observed in user-owned RDR
images and the validation rules implemented by `rdrkit`. It is not an official
format specification. Proprietary source code or binaries are not included in
this repository.

All integers described below are little-endian.

## Reader selection

The normal serving path is index-directed:

1. Read and validate the 52-byte outer file header.
2. Locate the directory-pointer record (`flags = 0x00000003`) near the footer.
3. Follow its relative offset to the archive directory
   (`flags = 0x00000008`).
4. Select the entry whose `object_id` matches the requested object and whose
   `frame_type` is `0x90`.
5. Decode that compact chunk-index frame.
6. Validate its geometry and sampled data records.
7. Serve reads by looking up the requested logical chunk directly.

If any required record or index is absent, malformed, out of bounds, or uses an
unknown encoding, the operation fails. `serve` and `list` never fall back to a
linear scan.

`inspect` is a separate diagnostic command that deliberately walks record
headers. It is not part of reader selection and is not called by `serve`.

## Common record prefix

Most records begin with:

| Offset | Size | Meaning |
| ---: | ---: | --- |
| `0x00` | 4 | Magic `0xd754da33` |
| `0x04` | 4 | Total record length |
| `0x08` | 4 | Record flags/type |

Every referenced record is checked against the physical archive size before it
is read.

## Archive directory

The directory-pointer payload contains:

| Offset | Size | Meaning |
| ---: | ---: | --- |
| `0x0c` | 8 | Directory offset relative to byte 52 |
| `0x14` | 4 | Directory record length |

The archive-directory payload starts after its 12-byte common prefix and is an
array of 20-byte entries:

| Size | Meaning |
| ---: | --- |
| 8 | Record offset relative to byte 52 |
| 4 | Record length |
| 4 | Object id |
| 4 | Frame type |

Observed frame types include compact indexes (`0x90`), extended indexes
(`0x93`), and object metadata (`0x98`). `rdrkit` requires the compact `0x90`
entry for random access.

## Compact chunk index

Two compact-index encodings are supported:

- raw: record flags `0x08000090`, 20-byte frame header;
- zlib: record flags `0x08040290`, 24-byte frame header.

After optional decompression, the payload begins with a 28-byte header:

| Offset | Size | Meaning |
| ---: | ---: | --- |
| `0x00` | 8 | Logical object size |
| `0x10` | 8 | Logical chunk size |
| `0x18` | 4 | Chunk count |

The header is followed by `chunk_count` entries of 12 bytes:

| Size | Meaning |
| ---: | --- |
| 8 | Data-record offset relative to byte 52 |
| 4 | Total data-record length |

The implementation requires strictly increasing physical offsets and verifies
that `ceil(logical_size / chunk_size) == chunk_count`.

## Data records

### Raw data (`0x18000020`)

The record header is 36 bytes. The logical offset is stored at byte 20 and the
logical length at byte 28. The payload follows at byte 36.

### Zlib data (`0x18040220`)

The record header is 40 bytes. The declared decoded length is stored at byte
12, the logical offset at byte 24, and the logical length at byte 32. The zlib
payload follows at byte 40.

The two decoded-length fields must agree, and the actual decompressed byte count
must equal the declared logical length.

## Validation before serving

When an object is opened, `rdrkit` validates the first, second, middle, and last
indexed records when present. During every read it validates the referenced
record's physical length, logical offset, and logical length before returning
decoded bytes.

This conservative behavior is intentional. Unknown variants must be added with
fixtures and explicit validation rather than inferred silently.
