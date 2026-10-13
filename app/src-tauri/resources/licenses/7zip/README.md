# Bundled 7-Zip 26.04

NekoNekoAsset bundles `resources/tools/7zip/7z.exe` and `7z.dll`. The existing
bundle notice declares them unmodified and describes invocation as a separate
command-line tool. Both files report PE product/file version `26.04`.
The SHA256 hashes of both binaries and the bundled license match the files
extracted from the verified official 26.04 Windows x64 installer.

Copyright (C) 1999-2026 Igor Pavlov. The bundled program is licensed under
GNU LGPL 2.1 or later, with the unRAR restriction and BSD components described
in the original license documents. It is not covered by the application's MIT
license. See https://www.7-zip.org/ and https://www.7-zip.org/faq.html .

## Provenance

Official download page: https://www.7-zip.org/download.html
Official release: https://github.com/ip7z/7zip/releases/tag/26.04
Official Windows x64 installer:
https://github.com/ip7z/7zip/releases/download/26.04/7z2604-x64.exe

The locally retained installer has SHA256
`d54bf805f9f3704d1e8db2fa3498ae7ef2df0312b40b558e7c71c734430a665d`,
matching the official release asset digest. The integration owner extracted
its files with official `7zr.exe`, without installation or registry changes.
SHA256 comparison confirmed that extracted `7z.exe`, `7z.dll` and
`License.txt` match the bundled copies byte for byte.

Official matching source archive:
https://github.com/ip7z/7zip/releases/download/26.04/7z2604-src.tar.xz

The source archive was obtained on 2026-10-09. Its 1,560,440 bytes have SHA256
`9691944c0fe0d01bb49373a704fb983fd33bc98b1738695179dfbf99ac1734f6`,
matching the digest on the official release asset listing:
https://github.com/ip7z/7zip/releases/expanded_assets/26.04 .

| Bundled file | Bytes | SHA256 |
| --- | ---: | --- |
| `resources/tools/7zip/7z.exe` | 577536 | `95fff19b73eff77c87c0d7bd32822e43fb90dd2932459f962eb532d8dfb6ba53` |
| `resources/tools/7zip/7z.dll` | 1907712 | `899cd73d7fc60e1c1c35c7de0f5bed3dc69bfe2cecbd48014d868a347970a0b1` |

## Preserved files

- `7z2604-src.tar.xz`: the complete official release source archive, unchanged.
- `BINARY-LICENSE.txt`: unchanged copy of the bundled program's `License.txt`.
- `upstream/DOC/License.txt`: original source license with BSD 2-clause,
  BSD 3-clause and unRAR notices/conditions.
- `upstream/DOC/copying.txt`: original full GNU LGPL version 2.1 text.
- `upstream/DOC/unRarLicense.txt`: original unRAR license document.
- `upstream/DOC/readme.txt`: original source README, version and build notes.
- `SHA256SUMS`: hashes for the archive and preserved original documents.

No upstream source or licensing text was modified. Only this README and the
checksum ledger were authored for NekoNekoAsset. The source archive retains
all upstream source files, build scripts and notices.

## Release integration to verify

The final distributed application must retain the applicable copyright,
license and warranty notices, the full LGPL text, and corresponding source
access. Preserve the original BSD conditions and unRAR restriction, including
the restriction against recreating the RAR compression algorithm.

Include this directory with releases that distribute the bundled binaries, or
provide equivalent corresponding-source access from the same download place
as required by LGPL 2.1 section 4. The final installer/resource inclusion and
recipient access have not been checked in this scoped task. Replace this
ledger's binary/source versions and hashes together if the bundle changes.