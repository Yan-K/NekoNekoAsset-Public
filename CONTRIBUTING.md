# Contributing to NekoNekoAsset

Original project code is MIT licensed. Keep the existing compact interface and
workflow; discuss substantial product or dependency changes in a GitHub issue.
Preserve third-party licenses, notices and stable Unity `.meta` GUIDs.

## Build

Windows x64 is the App distribution target. Install Visual Studio 2022 Build
Tools with Desktop development with C++, WebView2, Node.js **24.19.0**, pnpm
**11.25.0**, Python **3.14.8**, and Rust via rustup. `rust-toolchain.toml` selects
Rust **1.99.0** and the Windows MSVC target. Use the committed lockfiles.

From the repository root:

```powershell
pnpm --dir app install --frozen-lockfile
pnpm --dir app build
cargo test --locked --lib --manifest-path app/src-tauri/Cargo.toml
python scripts/test_release.py
python unity/scripts/package_bridge.py unity/Packages/com.yank.nekonekoasset.bridge build/bridge
```

Run the App with `pnpm --dir app tauri dev`. Release packaging requires the
maintainer's updater signing key; unsigned local builds can use
`pnpm --dir app tauri build --no-bundle`. The Bridge packager uses only Python's
standard library and verifies bytes, archive structure and GUIDs without a Unity
license. Packaging checks do not establish native Unity import behavior.

## Changes and verification

Preserve meaningful existing tests. Add focused tests for deterministic logic
with meaningful regression risk; no new test framework or broad testing gate is
required. Inspect rendered UI and relevant keyboard, loading and error states for
visual changes. Never treat a successful build as proof of visual quality.

Use synthetic catalogues and authored assets when developing or reporting bugs.
Do not commit or attach personal catalogue records, account/order information,
cookies, tokens, keys, browser profiles or acquired asset files. Keep diagnostics
aggregate and redact personal values from screenshots and logs.

Starting with **0.2.0 Beta**, installed users' persistent data is a compatibility
contract. Avoid database/schema/storage-format changes unless necessary. Every
necessary incompatible persistent-format change must include an explicit,
versioned migration from supported previous public versions, safe failure
handling and focused preservation/reopen checks. Never silently recreate, reset
or discard an existing user's database. A routine App update must retain the
catalogue, library paths, user edits, settings and protected device sessions.

## Maintainer releases

App and Bridge share one numeric version (`0.2.1`) and one tag (`v0.2.1`). Keep
`app/package.json`, Cargo, Tauri config and Bridge `package.json` in agreement.
Use a patch increment for routine releases. A minor or major increment requires
the project owner's explicit agreement before changing the version.

Starting with 0.2.1, every release needs an entry in the root `changelog.md` and
the same notes on its GitHub Release page. Keep release sections newest first,
with a heading such as `## 0.2.1 Beta - 2026-10-14`, then describe the actual
user-facing changes. The newest section must match the shared version and have
nonempty notes; duplicate version sections are rejected. Release tooling copies
that complete heading and section into the draft and published Release body.
The App update feed uses the published Release body, so release notes have one
source of truth. `release/config.json` contains only the Beta `prerelease` flag.

Windows releases contain the NSIS installer and its Tauri
signature; no portable App is distributed. Bridge assets include `.unitypackage`,
UPM `.tgz`, ZIP and the verified inventory. VCC listing is not yet configured.

Before tagging, the maintainer must perform bounded metadata-only real-session
checks in an empty isolated catalogue, with automatic downloads OFF: five pages
per available BOOTH Library/purchases/gifts/free-history, Gumroad buyer
library/bundle and Jinxxy inventory source, or a verified shorter natural end.
Drain details, compare observed facts with actual database/UI projections and
owned variants/files, and validate raw file lists/counts against the entitlement
union. Unexplained missing facts, duplicated aliases or count mismatches block a
release. Record aggregate publication timing, counts, requests and backoff; no
full-library scan or asset download is required or authorized by this check.
Also verify the current App Rust sender against an isolated actual Unity Editor:
received bytes, import result and durable same-ID receipt must pass. Unavailable
sessions/licensing block release instead of being counted as a pass.

Stage the final public build inputs, run `python scripts/release.py source-digest`,
then record the completed checks in the tracked `release/verification.json`:

```json
{
  "formatVersion": 1,
  "version": "0.2.1",
  "sourceSha256": "<source-digest output>",
  "verifiedAt": "<completed UTC timestamp, RFC 3339>",
  "checks": {
    "storeMetadata": {
      "passed": true,
      "pageLimit": 5,
      "autoDownload": false,
      "fullScan": false,
      "sources": ["booth", "gumroad", "jinxxy"],
      "failures": 0
    },
    "fileIdentity": {"passed": true, "failures": 0},
    "appUnityTransfer": {
      "passed": true,
      "receivedBytesMatch": true,
      "importSucceeded": true,
      "durableReceipt": true
    }
  }
}
```

This is a maintainer attestation after actual local checks; CI never receives
private sessions or local verification harnesses. Include only aggregate
evidence. Build-input changes invalidate the digest. Run
`python scripts/release.py check --tag v0.2.1 --require-verification`, commit the
source and marker, then push the matching tag. The Release workflow builds both
components, uploads the complete checked set to a draft, publishes it together,
and updates GitHub Pages `/app/latest.json` and `/bridge/latest.json` directly.
Published assets are immutable. Rerunning a published tag refreshes Pages only;
fixes require a new version. Pages selects the highest published numeric version,
including Beta prereleases, so older-tag reruns cannot downgrade the update feed.

Configure GitHub Pages to use Actions and allow the `github-pages` environment
to deploy version tags. Store the Tauri updater private key and optional password
in repository secrets `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; retain a secure backup outside Git. The
matching public key is in Tauri config. Release signatures bind their App version
in the signed trusted comment; manual signing must pass `--app-version`. Updater
signatures authenticate updates;
the first Beta does not include a Windows Authenticode publisher certificate.
