# Changelog

## 0.3.1 Beta - 2026-10-30

### Fixed

- One BOOTH product page that fails to load no longer fails the whole account sync. It is retried once after the other products finish; if it still fails, the sync ends needing action and keeps everything already saved. Responses cut off mid-transfer are retried, and incremental syncs skip product pages BOOTH has already removed.
- BOOTH music products with per-track downloads are recognized. Every track is listed once in both Library and purchases, and tracks are downloaded as MP3, which Unity imports directly.
- Product list: search, filters, sorting and language changes now respond immediately on large libraries instead of showing No matching assets for a long time.
- Clear search clears on the first click again; on wide windows the search box no longer slides the button away mid-click.

### Changed

- CJK text in the UI now uses bundled Noto Sans TC, JP, SC and KR; Settings > Appearance > Built-in fonts switches back to the Windows fonts.
- The processing icon keeps turning, slowly under reduced motion. A pending search shows Searching or Loading instead of an empty result, Activity dates share one right edge, long tag lists fold with Show all, and the blurred R-18 reveal covers the whole thumbnail.

### Release notes

- App and Unity Bridge share version 0.3.1 Beta. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- BOOTH music tracks are kept as MP3 only; FLAC and M4A versions are not downloaded.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.0 Beta - 2026-10-30

### Changed

- Refreshed the interface with floating panes, a compact selection dock, category and status icons, ambient detail previews and smoother motion.
- Grouped the theme picker into Dark, Light and Custom, and made the R-18 modes a segmented control.

### Added

- Eleven built-in themes: Noir, Midnight, Ocean, Forest, Sakura, Neon, Ink, High contrast, Lemon, Peach and Lilac.
- Appearance options for rounded corners and reduced motion. Reduced motion also stops spending fireworks.

### Release notes

- App and Unity Bridge share version 0.3.0 Beta. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- Existing support captures retain their original execution version after upgrading.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.2.6 Beta - 2026-10-30

### Fixed

- Startup and library recovery keep the selected location and setup drafts, show actionable storage errors, and prevent Retry from initializing the wrong folder. New installed profiles use a writable user-local default library location.
- Backup replacement, collection, saved-search and custom-theme deletion require confirmation. Editors and pending actions retain their captured targets, drafts and focus; tag edits preserve tags that were outside the edited scope.
- Store synchronization and batch local import continue past independent failed items. Forced updates bypass unused broken checkpoints, complete observed BOOTH grants can publish while public metadata is pending, and successful synchronization restores account status. Sync all continues after an account fails to start.
- Cancellation and Retry preserve newer job state and prevent duplicated execution. Worker polling and product status queries avoid decoding unrelated job history; obsolete source work and undo history stay bounded.
- Large downloads use connection and idle timeouts without a total-body deadline. Archive handling preserves verified outputs and originals on failure, reuses proven extracted files, and reads unmarked ZIP filenames as Japanese CP932 while honoring explicit UTF-8.
- Moving a library preserves additional user files and empty folders, validates its database snapshot, and excludes concurrent preview writes. Pending reset deletion can recover a hot SQLite journal under the library lock.
- Windows protected-session restore preserves parent-domain cookie scope. BOOTH sign-in allows the required Google, Apple and X authorization hosts; a file resolver's isolated authorization failure no longer discards an otherwise valid session.
- Base-avatar pins respect R-18 Hide and Only filters. Storage, archive and Unity errors retain actionable translations across all five languages.
- Unity import results reflect file import outcomes, independently of unrelated compiler errors. Interrupted transfers recover through the same durable request ID; proven snapshot cleanup, unreadable-request isolation and trusted-root checks preserve request evidence and user files.
- Optional preview and AI chat storage initialize on demand and handle trusted linked ancestors. Gemini structured requests use the documented JSON output enum without relaxing native result validation.

### Release notes

- App and Unity Bridge share version 0.2.6 Beta. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage. AI provider/model APIs were not called during this release's verification.

## 0.2.5 Beta - 2026-10-16

### Fixed

- BOOTH receipt parsing recognizes JPY amounts and the current Order Number and Created At summary labels, restoring proven paid prices and acquisition dates on English, Korean, Simplified Chinese and Traditional Chinese pages. Japanese receipt parsing retains the same order, product and file binding.
- Copied diagnostics preserve the original execution version of existing 0.2.4 jobs after upgrading.
- Windows Japanese-name reading stays below the native text limit, retries empty native results within a fixed limit, rejects incomplete readings and caches only successful results.

### Added

- Copied BOOTH diagnostics distinguish Japanese, English, Korean, Simplified Chinese and Traditional Chinese page-language counters, without including private page contents or raw locale strings.

### Release notes

- App and Unity Bridge share version 0.2.5 Beta. The catalogue database format, device profiles and library paths are retained.
- This patch fixes the confirmed receipt-language issue. Unresolved download controls or unproven source closure can still require attention during synchronization.

## 0.2.4 Beta - 2026-10-15

### Added

- Copied BOOTH diagnostics distinguish details that were never recorded, an attempt that has not started and missing or unsupported capture data. The report identifies the App version that executed a newly captured attempt separately from the version copying the report.
- BOOTH reports include the latest observed source-closure counts, page-language groups even when parsing fails early, and separate reasons why digital binding, receipt layout, unit price, quantity or BOOST evidence was not recognized. Missing date proof is counted separately. Reports retain only fixed statuses and operational counts, without private page contents or locale strings.

### Fixed

- Active-task diagnostics no longer present a stale terminal error as the current failure.

### Release notes

- App and Unity Bridge share version 0.2.4 Beta. The existing catalogue database format and library paths are retained.
- BOOTH paid-price and acquisition-date acceptance rules are unchanged. This update improves diagnosis; it does not expand supported website languages or claim to resolve the reported missing-price/date cause.

## 0.2.3 Beta - 2026-10-15

### Added

- Activity can manually copy a local diagnostic report for failed tasks, tasks needing attention and completed store synchronizations. Reports provide known failure reasons, the last operation stage and operational counts, without credentials, account/order/product details, URLs or local paths.
- BOOTH diagnostics include source traversal, parsed pages/details, HTTP status, retry, timeout and backoff counts, plus receipt price/date proof counters. Older jobs clearly indicate when attempt details were not captured.

### Fixed

- Diagnostic counts remain separate between retry attempts and accumulate across accounts during selected BOOTH updates. Login diagnostics retain the actual check stage, and older jobs do not borrow facts from a newer account synchronization.

### Release notes

- App and Unity Bridge share version 0.2.3 Beta. The existing catalogue database format and library paths are retained.
- BOOTH paid-price and acquisition-date parsing is unchanged in this release.

## 0.2.2 Beta - 2026-10-14

### Added

- Manage avatars, Edit and Batch Edit share a taller, shop-grouped avatar picker with name/shop search and a Select all checkbox. The heading and search stay visible while the list scrolls.
- Manage avatars supports editing avatar and shop display names from each row's pencil button.
- Batch Edit supports compatible-avatar selections with checked, unchecked and mixed states. Saving adds or removes only the avatars you changed, preserving each product's other selections and existing unmatched names.

### Fixed

- Base-avatar choices refresh as products arrive or their categories change, without reopening the library. Bundled and uncertain names remain selectable, and cleaner name/edition labels retain existing avatar identities and overrides.
- Batch Edit's R-18 switch reflects mixed selections and keeps each product's existing value until changed.

## 0.2.1 Beta - 2026-10-14

### Added

- Reopening the Windows App restores the last screen, position, normal window size and maximized state. If the saved screen is no longer available, the window opens within an available screen.

### Fixed

- Gumroad products require an explicit VRChat or VRC mention in their title or visible description before receiving a VRChat category. Generic product types, engine names and tags alone no longer qualify. The rule applies to new sync observations; existing categories are not recalculated on startup, and manual categories are preserved. BOOTH and Jinxxy classification are unchanged.

### Release notes

- App and Unity Bridge share version 0.2.1 Beta. Windows releases contain the installer and signed in-app update; Bridge downloads include unitypackage, UPM and VPM packages.
- This update retains the existing catalogue database format and library paths.
