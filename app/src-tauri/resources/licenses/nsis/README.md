# NSIS 3.11 installer components

The exact installed Tauri CLI 2.12.1 Windows bundler pins NSIS 3.11.
The public binary ZIP was verified against that source's SHA1 and recorded with
SHA256 in provenance.json. No NSIS compiler, stub or DLL was copied into app
resources; the bundler itself embeds the installer components.

Original upstream/COPYING contains the copyright, zlib/libpng, bzip2, complete
Common Public License 1.0 and special LZMA linking exception. Original Modern UI 2
and NSISdl notices are retained separately, unchanged. The default installer uses
LZMA; preserving the complete upstream document does not claim bzip2 is enabled.
The corresponding NSIS source release is 3.11, recorded in provenance.json.

The Tauri nsis_tauri_utils plugin has separate versioned notices in
../nsis-tauri-utils/. Keep both notice directories with the NSIS release.
The final release owner still verifies actual installer resource inclusion.

The complete official nsis-3.11-src.tar.bz2 is included unchanged. Its SHA256
was checked against the exact SourceForge release page; the original source
COPYING matches the binary ZIP COPYING byte for byte.

Original Tauri installer template/helper terms from CLI2.12.1 are retained
in tauri-installer/, including the upstream package-level LICENSE.spdx.
Their exact primary source URLs and SHA256 are recorded.
