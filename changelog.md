# Changelog

## 0.2.5 Beta - 2026-10-16

### Fixed

- BOOTH receipt parsing recognizes JPY amounts and the current Order Number and Created At summary labels, restoring proven paid prices and acquisition dates on English, Korean, Simplified Chinese and Traditional Chinese pages. Japanese receipt parsing retains the same order, product and file binding.
- Copied diagnostics preserve the original execution version of existing 0.2.4 jobs after upgrading.

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
