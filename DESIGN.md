# Patcher design

Patcher lets a mod author ship a mod without redistributing the game. The author diffs
an untouched copy of the game against their modded copy and ships the result: a small
patch file plus a patcher binary, or a single self-contained patcher. Players run it on
their own copy of the game.

## Decisions

| topic | decision |
|---|---|
| base versions | exactly one base version per patch (a v1→v2 mod update is just a patch whose base is modded v1) |
| deliverables | `Name.patch` + `Name-patcher.exe` (Windows) + `Name-patcher` (Linux); `--target`, `--no-standalone` |
| operations | modify, add, delete, copy/move (content-hash matched, near-zero cost), move+edit (diffed against the old path), empty-dir create/remove, Unix exec bit |
| priorities | patch size first, apply speed second, create speed last |
| selection | smallest encoding wins unless a cheaper-to-decode one is within the tolerance (5 % in `balanced`, 0 in `max`) |
| archives | opaque blobs; the `Transform` enum is the extension point for format-aware handling (e.g. PopCap `.pak`) |
| hashing | size + xxh3 always; BLAKE3 per file unless the author passes `--no-verify` |
| safety | stage into `.patcher-staging/`, verify every output, commit by rename, roll back on failure; originals kept in `.patcher-backup/` (`--revert`), `--no-backup` to skip |
| UI | installer window (Slint, software renderer) by default, console patcher with `--ui cli`; themes are TOML plus optional Slint, compiled by `slint-interpreter` at create time (to reject errors) and at start-up. See [docs/gui.md](docs/gui.md) and the [spec](docs/superpowers/specs/2026-10-16-gui-installer-design.md) |
| platforms | Windows and Linux; the console stub is pure Rust and cross-compiles to static musl with `rust-lld`; the GUI stub needs glibc on Linux (X11/Wayland are loaded at run time) and is built natively there |
| codecs | creator uses native libzstd (best ratios) and `lzma-rust2`; the stub decodes with a vendored `ruzstd` (plus a raw-prefix API) and `lzma-rust2` |
| large files | files over 512 MiB are encoded in 64 MiB chunks, each against a base window found by content-defined anchors |
| license | AGPL-3.0-or-later |

## Crates

- `patcher-core`: format, decoders, apply/verify/revert. Pure Rust, linked by the stub.
- `patcher-create`: scanning, classification, encoders, bsdiff, patch writer.
- `patcher-cli`: the `patcher` binary for authors (`create`, `apply`, `verify`, `revert`, `info`).
- `patcher-stub`: the console end-user binary, around 0.6 MB on Windows and 1.1 MB static
  on Linux. Its flow is a library function so the GUI stub's headless mode reuses it.
- `patcher-gui`: themes, the UI bundle, the built-in Slint library, the screen flow (a
  plain state machine), and the runtime that drives it. Linked by the GUI stub and the
  creator.
- `patcher-stub-gui`: the installer end-user binary, about 12 MB.
- `patcher-testgen`: deterministic synthetic game and mod scenarios.
- `xtask`: `dist`, `gen-corpus`, `bench`.
- `vendor/ruzstd`: ruzstd 0.9.1 (MIT) plus `FrameDecoder::set_raw_prefix`, needed to
  decode `zstd --patch-from` frames.

## Patch format (version 1)

All integers are little-endian. Offsets are relative to the start of the patch.

```text
Header (48 bytes)
  magic         8  "PTCHDB\0\x01"
  version       u32  = 1
  flags         u32  = 0
  index_offset  u64
  index_len     u64  compressed index size
  index_raw_len u64
  total_len     u64  header + blobs + index
Blobs            independent payloads, referenced by (offset, len)
Index            zstd frame of a bitcode-encoded `Index`
```

A standalone patcher is `stub ++ [UI bundle] ++ [patch] ++ footer`, and the 40-byte
footer is `ui_offset u64, ui_len u64, patch_offset u64, patch_len u64, "PTCHEND2"`; a
zero length means that part is absent. The 24-byte v1 footer
(`patch_offset, patch_len, "PTCHEND1"`) is still read. The stub reads its own executable,
finds the footer, and memory-maps the patch inside itself. The UI bundle is the theme's
folder (`"PUI1"`, decoded length, zstd frame of a bitcode file list).

The `Index` (see `crates/patcher-core/src/format.rs`) holds:

- `meta`: name, version, author, description, URL, and the creating tool.
- `base_fingerprint`: BLAKE3 over the sorted (path, size, xxh3) list of every scanned base file.
- `base_files`: each base file the patch reads, overwrites or deletes, with its `Check`
  (size, xxh3, optional BLAKE3).
- `writes`: output path, `Check`, exec bit, and a `Source`:
  - `Base(i)`: byte-identical copy of a base file.
  - `Segments([..])`: a concatenation of segments. Each segment has a decoded length, a
    blob, a `Codec`, a `Transform` and an optional base slice.
  - `Solid { group, offset }`: a slice of a decoded solid group.
- `deletes`, `solid` groups, `dirs_create`, `dirs_remove`.

Codecs:

| codec | meaning |
|---|---|
| `Raw` | stored |
| `Full(Zstd \| Lzma2)` | compressed alone |
| `Ref(Zstd)` | zstd frame with the base slice as raw prefix (`--patch-from`) |
| `Ref(Lzma2)` | raw LZMA2 stream with the base slice as preset dictionary |
| `Bsdiff(Zstd \| Lzma2)` | bsdiff control/diff/extra streams, LEB128 framed, then compressed |

Transforms: `None`, or `BcjX86`, a reversible E8/E9 rel32→absolute filter applied to the
base slice and the output. It runs as an extra candidate on PE/ELF x86 files.

Every path is validated on load: relative, `/`-separated, with no `..`, `.`, empty
component, drive letter or backslash. A patch therefore cannot write outside the game
folder.

## Creating a patch

1. Scan both trees. Default excludes, `.patcherignore` (gitignore syntax) and
   `--exclude` globs apply to both sides.
2. Hash everything (size + xxh3-128) and classify each modded file:
   - unchanged: skipped;
   - same content exists in base: copy;
   - same path: modify;
   - deleted base file with the same name and a similar size: move+edit;
   - small new file (4 MiB or less): solid;
   - anything else: added.
3. Encode in parallel under a memory budget (`--max-mem`, default 75 % of RAM). Each
   file tries:
   - full zstd and LZMA2;
   - zstd-ref and LZMA2-ref;
   - bsdiff with zstd or LZMA2;
   - raw;
   - the same set again behind BCJ for x86 executables.

   The selection rule picks the result. New small files are sorted by extension and
   packed into solid groups of up to 64 MiB.
4. Stream the blobs to the file, then write the index and fill in the header.

## Applying a patch

1. Resolve every base file. Off Windows, paths are matched case-insensitively, and an
   ambiguous match is an error.
2. Check size, xxh3 and (if present) BLAKE3 for each one. On a mismatch, report
   "already applied" if every output is already in place, otherwise list what differs.
   A patch that only adds files also reports "already applied" when the base matches and
   every output is in place.
3. Decode every output into `.patcher-staging/<n>` in parallel, largest first. Memory is
   bounded by a byte budget (half of RAM), and each output's check is verified. Decoders
   bound every allocation and output length by what the index declares.
4. Unmap the base files, then commit:
   - write the manifest;
   - move the originals of overwritten and deleted files into `.patcher-backup/files/`
     (with `--no-backup`, delete the deleted ones);
   - remove vanished directories, deepest first, if empty;
   - create parent directories, then rename the staged files into place (sequentially: parallel renames contend on NTFS directory locks);
   - create new empty directories;
   - rewrite the manifest.

   This order lets a file replace a directory and the other way round. On any error the
   steps are undone in reverse. If the undo itself fails, the backup is kept and the error
   says the originals are still in `.patcher-backup/files/`.

   Only a commit with a backup is atomic. With `--no-backup` overwritten and deleted
   files are gone once replaced, so a failure part way can leave a partly patched game.

`revert` reads the manifest, removes the files the patch added and the directories it
created, recreates the ones it removed, moves the originals back, and deletes the backup.
