"""Focused, authored regression checks for release safety (Python stdlib)."""
from __future__ import annotations

import base64
from datetime import datetime, timezone
import hashlib
import io
import json
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import release


def authored_signature(current="0.2.0"):
    packet = base64.b64encode(b"ED" + b"authored" + b"x" * 64).decode("ascii")
    global_signature = base64.b64encode(b"y" * 64).decode("ascii")
    text = f"untrusted comment: authored format fixture\n{packet}\ntrusted comment: timestamp:0\tfile:authored-setup.exe\tversion:{current}\n{global_signature}\n"
    return base64.b64encode(text.encode("ascii")).decode("ascii")


SIGNATURE = authored_signature()


class ReleaseChecks(unittest.TestCase):
    def setUp(self):
        self.base = release.ROOT / "build/release-checks"
        self.base.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="authored-", dir=self.base)
        self.root = Path(self.temporary.name)
        self.root_patch = patch.object(release, "ROOT", self.root)
        self.root_patch.start()

    def tearDown(self):
        self.root_patch.stop()
        if not self.root.resolve().is_relative_to(self.base.resolve()):
            raise ValueError("Test cleanup escaped its owned build directory")
        for path in [self.root, *self.root.rglob("*")]:
            if path.is_symlink() or getattr(path.lstat(), "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
                raise ValueError("Test cleanup refuses reparse-point traversal")
        self.temporary.cleanup()

    def write_json(self, path, data):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(data), encoding="utf-8")

    def marker(self):
        return {"formatVersion": 1, "version": "0.2.0", "sourceSha256": "a" * 64,
                "verifiedAt": datetime.now(timezone.utc).isoformat(), "checks": {
                    "storeMetadata": {"passed": True, "pageLimit": 5, "autoDownload": False,
                                      "fullScan": False, "sources": ["booth", "gumroad", "jinxxy"], "failures": 0},
                    "fileIdentity": {"passed": True, "failures": 0},
                    "appUnityTransfer": {"passed": True, "receivedBytesMatch": True,
                                         "importSucceeded": True, "durableReceipt": True}}}

    def test_versions_must_all_match(self):
        self.write_json("app/package.json", {"version": "0.2.0"})
        self.write_json("app/src-tauri/tauri.conf.json", {"version": "0.2.0"})
        self.write_json(f"unity/Packages/{release.PACKAGE}/package.json", {"version": "0.2.0"})
        (self.root / "app/src-tauri/Cargo.toml").write_text('[package]\nversion = "0.2.0"\n[lib]\n', encoding="utf-8")
        self.assertEqual(release.version(), "0.2.0")
        self.write_json("app/package.json", {"version": "0.2.1"})
        with self.assertRaises(ValueError):
            release.version()

    def test_digest_detects_changes_and_normalizes_checkout_line_endings(self):
        subprocess.run(["git", "init", "--quiet", self.root], check=True)
        subprocess.run(["git", "-C", self.root, "config", "core.autocrlf", "false"], check=True)
        (self.root / ".gitattributes").write_text("* text=auto\n", encoding="utf-8")
        app = self.root / "app"
        app.mkdir()
        source = app / "input.ts"
        source.write_bytes(b"line one\nline two\n")
        subprocess.run(["git", "-C", self.root, "add", "."], check=True)
        original = release.source_digest()
        source.write_bytes(b"line one\r\nline two\r\n")
        self.assertEqual(release.source_digest(), original)
        source.write_bytes(b"changed\n")
        self.assertNotEqual(release.source_digest(), original)

    def test_public_sources_reject_generated_python_cache(self):
        sources = ["scripts/release.py", "scripts/test_release.py", "unity/scripts/package_bridge.py"]
        release.check_public_paths(sources)
        for generated in ("scripts/__pycache__/release.cpython-312.pyc", "unity/scripts/__pycache__/unexpected.txt",
                          "release.pyc", "scripts/release.pyo"):
            with self.subTest(path=generated):
                with self.assertRaisesRegex(ValueError, "Generated"):
                    release.check_public_paths(sources + [generated])

    def test_completed_gate_passes_and_stale_gate_fails(self):
        marker = self.marker()
        self.write_json(release.MARKER, marker)
        with patch.object(release, "source_digest", return_value="a" * 64):
            release.check_verification("0.2.0")
        with patch.object(release, "source_digest", return_value="b" * 64):
            with self.assertRaises(ValueError):
                release.check_verification("0.2.0")

    def test_missing_store_alias_failure_or_missing_durable_receipt_blocks(self):
        for section, field, value in (("storeMetadata", "sources", ["booth", "gumroad"]),
                                      ("storeMetadata", "autoDownload", True),
                                      ("storeMetadata", "fullScan", True),
                                      ("fileIdentity", "failures", 1),
                                      ("appUnityTransfer", "durableReceipt", False)):
            with self.subTest(section=section, field=field):
                marker = self.marker()
                marker["checks"][section][field] = value
                self.write_json(release.MARKER, marker)
                with patch.object(release, "source_digest", return_value="a" * 64):
                    with self.assertRaises(ValueError):
                        release.check_verification("0.2.0")

    def artifacts(self):
        directory = self.root / "artifacts"
        directory.mkdir()
        current = "0.2.0"
        key_packet = base64.b64encode(b"ED" + b"authored" + b"x" * 32).decode("ascii")
        key = base64.b64encode(("untrusted comment: authored key\n" + key_packet + "\n").encode("ascii")).decode("ascii")
        self.write_json("app/src-tauri/tauri.conf.json", {"plugins": {"updater": {"pubkey": key}}})
        for name in release.asset_names(current) - {"SHA256SUMS.txt"}:
            (directory / name).write_bytes(b"authored artifact")
        (directory / f"NekoNekoAsset_{current}_x64-setup.exe.sig").write_text(SIGNATURE, encoding="ascii")
        inventory = {"packageName": release.PACKAGE, "version": current, "archives": {}}
        for kind, suffix in (("unitypackage", ".unitypackage"), ("upm", ".tgz"), ("vpm", ".zip")):
            name = f"{release.PACKAGE}-{current}{suffix}"
            inventory["archives"][kind] = {"file": name, "sha256": hashlib.sha256((directory / name).read_bytes()).hexdigest(),
                                            "allMemberBytesMatch": True}
        (directory / f"{release.PACKAGE}-{current}.inventory.json").write_text(json.dumps(inventory), encoding="utf-8")
        return directory

    def test_portable_or_internal_extra_asset_blocks(self):
        directory = self.artifacts()
        self.assertEqual(len(release.check_artifacts(directory, "0.2.0", False)), 6)
        (directory / "NekoNekoAsset_0.2.0_x64-portable.zip").write_bytes(b"authored")
        with self.assertRaises(ValueError):
            release.check_artifacts(directory, "0.2.0", False)

    def test_changed_bridge_archive_blocks(self):
        directory = self.artifacts()
        (directory / f"{release.PACKAGE}-0.2.0.tgz").write_bytes(b"different authored archive")
        with self.assertRaises(ValueError):
            release.check_artifacts(directory, "0.2.0", False)

    def test_missing_signature_blocks(self):
        directory = self.artifacts()
        (directory / "NekoNekoAsset_0.2.0_x64-setup.exe.sig").unlink()
        with self.assertRaises(ValueError):
            release.check_artifacts(directory, "0.2.0", False)

    def test_signature_bound_to_another_version_blocks(self):
        with self.assertRaises(ValueError):
            release.signature_value(SIGNATURE, "0.2.1")

    def test_signing_key_mismatch_blocks(self):
        packet = base64.b64encode(b"ED" + b"otherkey" + b"x" * 32).decode("ascii")
        key = base64.b64encode(("untrusted comment: authored key\n" + packet + "\n").encode("ascii")).decode("ascii")
        with self.assertRaises(ValueError):
            release.signature_value(SIGNATURE, "0.2.0", key)

    def published(self, current, draft=False):
        return {"tag_name": "v" + current, "draft": draft, "prerelease": True, "body": "Authored notes",
                "published_at": "2026-01-01T00:00:00Z", "html_url": "https://github.com/example/releases/v" + current,
                "assets": [{"name": name, "state": "uploaded", "browser_download_url": "https://example.test/" + name}
                           for name in sorted(release.asset_names(current))]}

    def test_pages_selects_numeric_highest_prerelease_and_ignores_draft(self):
        records = [self.published("0.9.0"), self.published("0.20.0", draft=True), self.published("0.10.0")]
        fake_api = unittest.mock.Mock(spec=release.GitHub)
        fake_api.request.return_value = records
        output = self.root / "pages"
        with patch.object(release, "GitHub", return_value=fake_api), \
                patch.object(release.urllib.request, "urlopen", return_value=io.BytesIO(authored_signature("0.10.0").encode())):
            release.pages(output)
        app = json.loads((output / "app/latest.json").read_text(encoding="utf-8"))
        bridge = json.loads((output / "bridge/latest.json").read_text(encoding="utf-8"))
        self.assertEqual(app["version"], "0.10.0")
        self.assertEqual(set(app["platforms"]), {"windows-x86_64"})
        self.assertEqual(bridge["version"], app["version"])
        self.assertTrue(bridge["unitypackageUrl"].endswith(".unitypackage"))

    def test_incomplete_latest_release_retains_existing_feed(self):
        latest = self.published("0.3.0")
        latest["assets"].pop()
        fake_api = unittest.mock.Mock(spec=release.GitHub)
        fake_api.request.return_value = [self.published("0.2.0"), latest]
        output = self.root / "pages"
        (output / "app").mkdir(parents=True)
        existing = output / "app/latest.json"
        existing.write_text("existing feed", encoding="utf-8")
        with patch.object(release, "GitHub", return_value=fake_api):
            with self.assertRaises(ValueError):
                release.pages(output)
        self.assertEqual(existing.read_text(encoding="utf-8"), "existing feed")

    def test_published_release_rerun_never_touches_assets(self):
        fake_api = unittest.mock.Mock(spec=release.GitHub)
        fake_api.release.return_value = {"draft": False}
        with patch.object(release, "check", return_value="0.2.0"), patch.object(release, "GitHub", return_value=fake_api):
            release.publish("v0.2.0", self.root / "does-not-exist")
        fake_api.request.assert_not_called()

    def test_published_tag_lookup_never_lists_or_changes_releases(self):
        api = object.__new__(release.GitHub)
        published = {"id": 7, "tag_name": "v0.2.0", "draft": False}
        api.request = unittest.mock.Mock(return_value=published)
        self.assertEqual(api.release("v0.2.0"), published)
        api.request.assert_called_once_with("releases/tags/v0.2.0")

    def test_draft_lookup_searches_all_pages_and_exact_tag(self):
        api = object.__new__(release.GitHub)
        first_page = [{"id": index, "tag_name": f"v0.1.{index}", "draft": True} for index in range(100)]
        draft = {"id": 101, "tag_name": "v0.2.0", "draft": True}
        api.request = unittest.mock.Mock(side_effect=[None, first_page, [draft, {"tag_name": "v0.2.01", "draft": True}]])
        self.assertEqual(api.release("v0.2.0"), draft)
        self.assertEqual([call.args[0] for call in api.request.call_args_list],
                         ["releases/tags/v0.2.0", "releases?per_page=100&page=1", "releases?per_page=100&page=2"])

    def test_duplicate_exact_tag_drafts_across_pages_block(self):
        api = object.__new__(release.GitHub)
        draft = {"id": 1, "tag_name": "v0.2.0", "draft": True}
        first_page = [draft] + [{"tag_name": f"v0.1.{index}", "draft": False} for index in range(99)]
        api.request = unittest.mock.Mock(side_effect=[None, first_page, [{"id": 2, "tag_name": "v0.2.0", "draft": True}]])
        with self.assertRaisesRegex(ValueError, "Multiple releases"):
            api.release("v0.2.0")

    def test_prepare_retry_reuses_listed_draft_without_duplicate_creation(self):
        api = object.__new__(release.GitHub)
        draft = {"id": 73, "tag_name": "v0.2.0", "draft": True}
        api.request = unittest.mock.Mock(side_effect=[None, [draft], draft])
        with patch.object(release, "check", return_value="0.2.0"), \
                patch.object(release, "GitHub", return_value=api), \
                patch.object(api, "assert_tag_checkout"), \
                patch.object(release, "git", return_value=b"authored-head"), \
                patch.object(release, "release_config", return_value={"prerelease": True, "notes": "Authored notes"}), \
                patch.object(release, "set_output"):
            release.prepare("v0.2.0")
        self.assertEqual(api.request.call_count, 3)
        self.assertEqual(api.request.call_args.args[:2], ("releases/73", "PATCH"))
        self.assertFalse(any(len(call.args) > 1 and call.args[1] == "POST" for call in api.request.call_args_list))


if __name__ == "__main__":
    unittest.main()
