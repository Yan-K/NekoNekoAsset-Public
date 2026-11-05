# Changelog

## 0.3.9 Beta - 2026-11-06

### Changed

- Automatic categories on BOOTH now follow the listing's own store category instead of its tags. A world stays in World when it is tagged Udon or Shader, and a 3D motion stays in Motion.
- Shaders, gimmicks and Udon systems, which BOOTH has no category for, now go to Shader, Gimmick and Udon when their title says so, or for tools their tags.
- Videos, moving backgrounds, VJ loops and particle effects tagged アニメーション no longer land in Motion. Outside BOOTH's 3D categories, a listing has to mention VRChat or Unity to get a VRChat category.
- BOOTH Library sync is much faster after its first full read: it stops once it reaches items it already knows, then checks BOOTH's recently updated list for sellers' new files. A sync still reads every page at least once every 7 days.
- Adding or changing a local asset's listing link in Edit now reads its current price too. It is read after the dialog closes, and a listing that is slow to answer or gone just leaves the price Unknown.

### Fixed

- BOOTH accounts with an empty or one-page Library, purchase, gift or free list no longer end every sync as incomplete, and an empty Library no longer fails first login.

### Release notes

- Categories you set yourself never change. An asset already in your library takes its new automatic category when its own listing or files change; nothing moves when you update or open the App.
- The Unity Bridge is unchanged and stays at [0.3.7 Beta](https://github.com/Yan-K/NekoNekoAsset-Public/releases/tag/v0.3.7); this release does not include Bridge files. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- The first BOOTH Library sync after updating still reads every page.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.8 Beta - 2026-11-05

### Changed

- A gift's paid price now reads Gift instead of Unknown, in libraries you already have too. Gifts still don't count toward Spending.
- Local assets are called Local File everywhere, including the account filter that used to say Offline files. Their paid price now says Local File too.
- Local assets you add from now on show the date you added them as Acquired.
- Local assets you add with a BOOTH, Gumroad or Jinxxy listing URL now show its current price. It is read after the import has finished, so it never slows an import down. A listing with several variants shows its cheapest, and a listing that is slow to answer or gone just leaves the price Unknown.

### Fixed

- Fetch accepts BOOTH links in Korean, Simplified Chinese and Traditional Chinese, such as `booth.pm/zh-tw/items/…`.

### Release notes

- The Unity Bridge is unchanged and stays at [0.3.7 Beta](https://github.com/Yan-K/NekoNekoAsset-Public/releases/tag/v0.3.7); this release does not include Bridge files. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained. Local assets added earlier keep their Unknown date and price.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.7 Beta - 2026-11-03

### Changed

- Avatar names are now found in store titles that carry sale, status or platform text, so more avatars show their real name and match their compatible assets.
- Assets that link to or name an avatar you own now count toward that avatar, and its duplicate rows are gone from Manage avatars.
- An asset in BOOTH's 3D character category, or one whose title calls itself a single named 3D model, now counts as an Avatar base whatever its tags say. Assets already in your library change only when their listing changes; until then you can set the category in Edit.

### Fixed

- BOOTH gifts show the date you received them. Libraries synced earlier gain their dates on the next sync.
- Unity Bridge: importing an update with Unity open now takes effect right away, without a restart.
- Unity Bridge: a package that can't be read says which file and why.

### Release notes

- The App and the Unity Bridge move to 0.3.7. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained. The first start of an existing library re-reads its stored tags once, which can take about half a minute on a hard disk.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.6 Beta - 2026-11-02

### Added

- Aqua, a light theme, in Settings › Appearance.

### Changed

- Library items are called assets, and store pages are called listings, in all five languages.
- Update and restart no longer fails on its first click while other work is briefly busy, and no longer waits for store syncs: they continue where they stopped after the restart. When other work has to finish first, the update prompt names it.
- Unity Bridge: the import review window gives a column only to what arrived, so packages alone or loose files alone fill the window.
- One asset a store can't fully provide, for example a removed listing, files that aren't ready yet or a page that answers with an error, no longer fails a sync or leaves it needing action. The sync completes and Activity and the Sync dialog show how many assets weren't fully read; those assets keep what they had.

### Fixed

- Signing in to BOOTH/pixiv, Gumroad or Jinxxy with Google, X, Facebook or Weibo no longer stays on the provider's page.
- Settings › Storage shows the library's used and free space.
- Sync and downloads work where antivirus or a school or company network inspects HTTPS connections, and they follow the Windows proxy settings.
- Brief connection drops, server errors and rate limits are retried for about 30 seconds instead of 6, including responses cut off partway.
- A sync or download no longer signs you out because one listing or file asked for sign-in, and a sync waits for a slow sign-in profile to load. Your sign-in stays saved until you log out.
- Syncs and downloads follow a store's pages and files to other HTTPS hosts, such as a new download server or a Jinxxy language subdomain.
- BOOTH: a passing 403 no longer leaves the account needing action, and Update during a sync waits for the sync instead of failing.
- Gumroad and Jinxxy: a sync no longer fails when a purchase arrives or a seller edits a listing while it runs.
- A finished download or extraction no longer fails when antivirus or Windows Search briefly opens the new file.
- Archives with UTF-8 names, folders that differ only in letter case or duplicate entries now extract; a clashing file is kept as `name_1`.
- Store files whose names Windows doesn't allow are saved under a readable name and can still be imported into Unity.
- Adding your own files works from redirected folders such as OneDrive's Desktop and Documents, including files that are only in the cloud, and waits briefly for a file another program is using.
- Errors say when a file is in use by another program or the library's drive went away, and Copy diagnostics in Activity tells certificate, DNS, connection and timeout failures apart.
- Unity Bridge: packages that write to `Packages/` or `ProjectSettings/` import correctly, including tar-packed packages, folders without `folderAsset`, updates of installed packages and long paths, and Unity registers new embedded packages right away.

### Release notes

- The App and the Unity Bridge move to 0.3.6. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.5 Beta - 2026-11-01

### Added

- Files you downloaded stay in a product's Files after its store stops offering them, for example when a creator replaces Product_1.0.0.zip with 1.0.1. They are marked Removed from store, still open and import into Unity, and are never downloaded again. Files removed this way before 0.3.5 appear again the first time you open your library.
- Store products accept your own files. Add files (+) in Files copies a file you already have, such as an older version, into that product without changing the original; archives are extracted as downloads are. These files are marked Added by you.
- When a download fails because its store no longer offers the file, the App now says so and offers Update and download, which updates the product and then downloads its current files.
- Custom categories: below Unclassified in the sidebar, + adds, renames and deletes your own categories. Only Edit, Batch Edit and dragging products onto them assign one; automatic classification never does, and deleting one returns its products to their automatic category.
- Settings › Appearance › One-handed drag actions, off by default, adds mouse box selection and dragging a selection onto a sidebar category.

### Fixed

- A download whose file the store removed no longer fails with "Required files are missing", which pointed to your computer instead of the store.
- Deleting a product's downloaded files keeps files the store no longer offers and files you added, because neither can be downloaded again. Delete those from their own row in Files.
- BOOTH: a sync no longer stops needing action at a free item that offers a VRoid Studio project or custom item file.
- The selection bar shows Import Unity only when a selected product has a downloaded file, at its right end, and keeps Clear, the count and Select all together.

### Release notes

- The Unity Bridge is unchanged and stays at [0.3.4 Beta](https://github.com/Yan-K/NekoNekoAsset-Public/releases/tag/v0.3.4); this release does not include Bridge files. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.4 Beta - 2026-10-31

### Added

- Downloads can be cancelled where they run: while a product downloads, its Download button becomes Cancel, a file queued on its own has its own cancel button in Files, the right-click and ⋯ menus cancel a selection, and Activity offers Cancel all downloads.
- Download all now asks before starting and shows how many products it will download.
- Downloaded files can be deleted from your computer, one file at a time in Files, or for a whole product or selection from Files, the right-click menu and the ⋯ menu. The products stay in your library and can be downloaded again; a file still open in another program is kept and can be deleted later.
- Settings › Features › Hide unavailable products, on by default, hides store products that offer no file to download, such as an ended free offer or a product whose files the creator removed, while none of their files are on your computer. Turned off, these products show Unavailable instead of Download.

### Fixed

- Settings › Storage › Move works again in a running App; since 0.2.6 it stopped with only Action failed. Move errors now explain the cause and which location is affected, and Storage shows the move while it runs.

### Release notes

- App and Unity Bridge share version 0.3.4 Beta. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- A product that still lists files its store no longer serves stays visible; downloading it reports the store's error.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.3 Beta - 2026-10-31

### Fixed

- BOOTH: the sync that runs when the App starts, including the first start after an update, no longer downloads every order receipt again or rewrites products that did not change. Receipts already saved are reused, also when an update interrupted the previous sync.
- Compatible-avatar filters respond immediately again, and the avatar list opens faster. Since 0.2.5, every click reread the Base avatar names that Windows cannot read aloud, taking about a third of a second per name.
- Compatible avatars taken from a product's variant or from a name you entered now show the products they count, instead of an empty list.
- Opening a library is faster. Ready badges are rechecked only for products with downloads, and the sync history is read only when needed and then stored in a compact form.

### Release notes

- App and Unity Bridge share version 0.3.3 Beta. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- The first list after opening a library still reads every product once.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.2 Beta - 2026-10-31

### Fixed

- Product list: search, compatible-avatar and account filters respond immediately on large libraries kept on a NAS or network share. A filter no longer rereads every product's stored file details, and products changed during a sync or download refresh one at a time instead of the whole list.
- Japanese text with the built-in fonts no longer breaks inside words, such as すべてのアセット in the sidebar. Product titles and status notices keep their current line breaks.
- In Japanese, Choose local file in Import stays on one line and the account filter reads 全アカウント instead of being cut off.
- In narrow windows, Settings tabs and the R-18 modes use two rows instead of cutting off labels, and AI settings show the model beside the provider.

### Release notes

- App and Unity Bridge share version 0.3.2 Beta. Existing catalogue databases, device profiles, source checkpoints, identifiers and managed paths are retained.
- The first list after opening a library still reads every product once.
- Bounded source verification does not establish full-library throughput or complete uncapped source coverage.

## 0.3.1 Beta - 2026-10-30

### Fixed

- One BOOTH product page that fails to load no longer fails the whole account sync. It is retried once after the other products finish; if it still fails, the sync ends needing action and keeps everything already saved. Responses cut off mid-transfer are retried, and incremental syncs skip product pages BOOTH has already removed.
- BOOTH music products with per-track downloads are recognized. Every track is listed once in both Library and purchases, and tracks are downloaded as MP3, which Unity imports directly.
- BOOTH no longer occasionally asks you to sign in again during normal use. When an older copy of BOOTH's sign-in cookie was stored beside the current one, a sync could send only the older copy, fail and then discard the working sign-in.
- Product list: search, filters, sorting and language changes now respond immediately on large libraries instead of showing No matching assets for a long time.
- Clear search clears on the first click again; on wide windows the search box no longer slides the button away mid-click.
- Product thumbnails no longer look jagged when a large store preview is cropped to fit its card.
- Long kanji avatar names are read in short pieces. Some Windows versions return no reading once Japanese text passes about 20 characters, well under their documented limit.

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
