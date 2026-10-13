# Tauri NSIS utility plugin 0.5.3

Tauri CLI 2.12.1 pins the official `nsis_tauri_utils.dll` release 0.5.3.
The release tag resolves to `13d9edd27b69310e108d6fbd49f90992f8a05390`.
The official DLL's published SHA1 was verified in memory; its observed SHA256
is `5ba143b5db4a87d32d6e7802e033330aae56cbceabe0d1e3ba41948385ad4709`.
The DLL was not saved or executed during this notice task. This directory
contains licensing documents and provenance only.

`upstream/nsis-tauri-utils/<commit>/` preserves the original MIT and Apache
licenses and project README at the exact release commit. No standalone NOTICE
file exists in the inspected pinned repository tree. `provenance.json` records
all original document hashes and exact inspected source URLs/hashes.

The combined plugin includes `nsis-process` source. Its process-launch helper
explicitly identifies a port from Chromium `base/win/elevation_util.cc` at
`36e1c43ace542988d624bd1bc0813c184482d2ab`. The corresponding original
BSD license and the file's Copyright 2024 The Chromium Authors legal header
are preserved under `upstream/chromium/<commit>/`. These notices supplement
the plugin's MIT/Apache notices; retain them in distributed installer materials.
No Chromium implementation code was copied.

Pinned source manifests declare `windows-sys = 0.61.2`, `semver = 1.0` without
default features, and local `nsis-plugin-api`. The application's existing
Rust notice inventory already covers these registry dependency families.
The release source has no Cargo.lock, so that inventory is not proof of the
prebuilt DLL's exact patch versions. The release workflow also does not pin
its Rust toolchain; the application's Rust 1.99 attribution is not asserted
to identify this plugin's core/alloc build. These limits remain explicit in
`provenance.json`. No dependency version or absent attribution was invented.