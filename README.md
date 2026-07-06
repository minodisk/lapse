# lapse

A CLI tool that makes the EXIF `DateTimeOriginal` of burst-shot JPEGs unique by advancing them 1 second at a time in filename order.

Google Photos ignores the sub-second timestamps (`SubSecTimeOriginal`) of burst photos, so photos taken within the same second do not keep their display order. By making the timestamps unique at second granularity before uploading, this tool ensures the photos are ordered as shot.

## Behavior

1. Collects the JPEGs (`.jpg` / `.jpeg`, case-insensitive) **directly under** the given directory. Subdirectories are not recursed into.
2. Sorts them by **natural filename order** (`image_2.jpg` comes before `image_10.jpg`).
3. Assigns, in sort order, **new time = max(original time, previous file's new time + 1 second)**. The first file keeps its original time. Only burst shots collapsed into the same second are pushed forward 1 second at a time; original capture times of separate scenes with time gaps are preserved (only when a push-out catches up with the next scene is that scene shifted by the minimum necessary amount). The order is always monotonically increasing by filename.
4. In addition to `DateTimeOriginal` (0x9003), `DateTimeDigitized` (0x9004) and `DateTime` (0x0132) are set to the same value if they exist (missing tags are not created). Files are saved in place. Files that need no change (already unique) are not written at all, so re-running is idempotent.

### Lossless guarantee

The EXIF rewrite is done by **in-place replacement of the target tag values (fixed 19 bytes) inside the APP1 segment**. The JPEG is never decoded or rebuilt, so every other byte — including the image scan data — is bit-identical before and after processing (guaranteed by tests). Writes go to a temporary file followed by an atomic rename, so a crash mid-write never corrupts the original file.

## Installation

Download the prebuilt binary for your platform (Linux x86_64 / aarch64, macOS Apple Silicon / Intel, Windows x86_64) from [Releases](https://github.com/minodisk/lapse/releases/latest), extract the archive, and place `lapse` somewhere on your `PATH`.

Alternatively, build from source with Cargo:

```sh
cargo install --path .
```

## Usage

This is a destructive operation, so first check the timestamps to be written with `--dry-run`.

```sh
# Preview (no rewriting)
lapse --dry-run /path/to/photos

# Run
lapse /path/to/photos

# Run while printing before/after timestamps
lapse --verbose /path/to/photos
```

If no target JPEGs are found, the command fails. Files whose `DateTimeOriginal` cannot be read (e.g. no EXIF) are skipped individually and summarized at the end (exit code 1).

## Testing

```sh
cargo test
```

Sample JPEGs for testing (with EXIF containing Make / Model / all three date-time tags / GPS / MakerNote) are generated inside the tests, which verify:

1. **Lossless guarantee**: the file length is unchanged and the differing bytes are confined to the value areas of the target date-time tags (19 bytes × 3 locations). The scan data (entropy-coded section) is byte-identical, and the decoded pixel arrays match bit for bit.
2. **Other EXIF tags preserved**: all tags other than the date-time tags (including GPS and MakerNote) are unchanged in both value and set.
3. **Target tag update**: `DateTimeOriginal` becomes "base time + index seconds" in `YYYY:MM:DD HH:MM:SS` format. Natural-order sorting and minute carry-over are also verified.
4. **File integrity**: the file still decodes as a valid JPEG after processing.

## Implementation notes

- Pure Rust implementation. No dependency on external processes such as ExifTool.
- EXIF-writing crates (`little_exif` etc.) rebuild segments and risk corrupting MakerNote offsets, so instead a minimal in-house TIFF/IFD parser locates the tag value offsets for in-place replacement (see the selection notes at the top of `src/exif_patch.rs`).
- Second assignment is finalized sequentially after sorting; only file I/O and rewriting are parallelized with rayon.
