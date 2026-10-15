# Changelog

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
