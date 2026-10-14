# Changelog

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
