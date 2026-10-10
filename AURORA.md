# Aurora Silicon fork

The `aurora-m1n1` branch starts at upstream rust-fatfs revision
`4eccb50d011146fbed20e133d33b22f3c27292e7`, the revision previously pinned by
m1n1. It preserves upstream's MIT licence and the original authorship of the
three changes imported from aurora-silicon/m1n1.

The branch carries three changes absent from upstream master at `2aefc2a`:

- An optional `explicit-fat32` feature accepts a structurally valid FAT32 BPB
  on a volume below the usual FAT32 cluster-count threshold. This is a
  compatibility extension for explicitly formatted small FAT32 volumes, not
  a change to the default cluster-count-based interpretation.
- Public `DirEntry::eq_name` exposes the existing long-name and short-alias
  matcher so callers can check for duplicate matches using the filesystem's
  own name rules.
- File reads coalesce adjacent clusters from the FAT chain into one storage
  read. Fragmented chains and directory streams retain their separate reads.

`tests/aurora.rs` uses synthetic disk images to verify the compatibility
feature, both name forms, contiguous and fragmented reads, short underlying
reads, EOF, and seeks across cluster boundaries. The contiguous-read test
requires one 64 KiB data read; the original pinned upstream reads only one
512-byte cluster on the first call. This demonstrates the larger transfer
without claiming a measured hardware speedup.

Run the focused checks with:

```sh
cargo test --lib --test read --test write --test aurora
cargo test --lib --test read --test write --test aurora --features explicit-fat32
cargo check --no-default-features --features alloc,lfn,explicit-fat32
```
