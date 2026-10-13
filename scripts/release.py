"""Small, standard-library release helpers. Run from the repository root."""
from __future__ import annotations

import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "com.yank.nekonekoasset.bridge"
REPOSITORY = "Yan-K/NekoNekoAsset-Public"
PAGES = "https://yan-k.github.io/NekoNekoAsset-Public"
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\Z")
BUILD_PREFIXES = ("app/", f"unity/Packages/{PACKAGE}/", "unity/scripts/package_bridge.py",
                  "scripts/", ".github/workflows/", "rust-toolchain.toml", "release/config.json",
                  ".gitattributes")
FORBIDDEN_PREFIXES = ("AgentDocs/", ".tools/", ".local/", "work/", "tools/",
                      "unity/verification/", "app/verification/", "app/checks/", "app/portable/")
FORBIDDEN_FILES = {"AGENTS.md", "README.md", "app/scripts/test-native.ps1",
                   "app/scripts/package-portable.ps1"}
MARKER = "release/verification.json"


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", *args], cwd=ROOT)


def tracked_paths() -> list[str]:
    return sorted(git("ls-files", "-z").decode("utf-8").split("\0")[:-1])


def source_digest() -> str:
    # hash-object applies Git's clean filters, so LF/CRLF checkouts attest equally.
    digest = hashlib.sha256()
    paths = [path for path in tracked_paths() if path.startswith(BUILD_PREFIXES)]
    if not paths:
        raise ValueError("No tracked public build inputs found; stage source before attesting")
    for path in paths:
        if (ROOT / path).is_symlink():
            raise ValueError("Build input symlinks are not supported")
        blob = git("hash-object", "--path", path, path).strip()
        digest.update(path.encode("utf-8") + b"\0" + blob + b"\n")
    return digest.hexdigest()


def read_json(path: str | Path) -> dict:
    return json.loads((ROOT / path).read_text(encoding="utf-8"))


def version() -> str:
    versions = [read_json("app/package.json")["version"],
                read_json("app/src-tauri/tauri.conf.json")["version"],
                read_json(f"unity/Packages/{PACKAGE}/package.json")["version"]]
    # Only the first Cargo [package] section is relevant.
    cargo = (ROOT / "app/src-tauri/Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo.split("[lib]", 1)[0], re.MULTILINE)
    versions.append(match.group(1) if match else "")
    if len(set(versions)) != 1 or not VERSION.fullmatch(versions[0]):
        raise ValueError("App, Cargo, Tauri and Bridge versions must equal one numeric SemVer")
    return versions[0]


def release_config() -> dict:
    config = read_json("release/config.json")
    if set(config) != {"prerelease", "notes"} or type(config["prerelease"]) is not bool:
        raise ValueError("Release config requires only prerelease (boolean) and notes (text)")
    if not isinstance(config["notes"], str) or not config["notes"].strip():
        raise ValueError("Release notes are required")
    return config


def check_verification(current: str) -> None:
    marker = read_json(MARKER)
    if marker.get("formatVersion") != 1 or marker.get("version") != current:
        raise ValueError("Release verification marker has the wrong format or version")
    if marker.get("sourceSha256") != source_digest():
        raise ValueError("Release verification is stale for these tracked build inputs")
    verified = datetime.fromisoformat(marker["verifiedAt"].replace("Z", "+00:00"))
    if verified.tzinfo is None or verified > datetime.now(timezone.utc):
        raise ValueError("Release verification needs a valid, completed UTC timestamp")
    checks = marker.get("checks", {})
    stores = checks.get("storeMetadata", {})
    if not (stores.get("passed") is True and stores.get("pageLimit") == 5
            and stores.get("autoDownload") is False and stores.get("fullScan") is False
            and stores.get("failures") == 0
            and sorted(stores.get("sources", [])) == ["booth", "gumroad", "jinxxy"]):
        raise ValueError("The required bounded three-store metadata verification did not pass")
    files = checks.get("fileIdentity", {})
    if files.get("passed") is not True or files.get("failures") != 0:
        raise ValueError("The raw DTO/count/entitlement file-identity gate did not pass")
    unity = checks.get("appUnityTransfer", {})
    if any(unity.get(key) is not True for key in
           ("passed", "receivedBytesMatch", "importSucceeded", "durableReceipt")):
        raise ValueError("The current App sender / actual Unity import / durable receipt gate did not pass")


def check_public_paths(paths: list[str]) -> None:
    if any(path in FORBIDDEN_FILES or path.startswith(FORBIDDEN_PREFIXES) for path in paths):
        raise ValueError("Internal documentation, tooling or portable inputs are tracked in the public repository")
    if any(part in {"node_modules", "target", "dist", "__pycache__"} for path in paths for part in Path(path).parts):
        raise ValueError("Generated build inputs must not be tracked")
    if any(path.lower().endswith((".pyc", ".pyo")) for path in paths):
        raise ValueError("Generated Python bytecode must not be tracked")
    if any(path.endswith((".sqlite", ".sqlite3", ".db", ".cookies", ".cookie", ".key", ".pem")) for path in paths):
        raise ValueError("Private databases or credential files must not be tracked")


def check(tag: str | None, verification: bool, allow_internal: bool = False) -> str:
    current = version()
    if tag and tag != f"v{current}":
        raise ValueError("Release tag must equal the shared App and Bridge version")
    config = read_json("app/src-tauri/tauri.conf.json")
    updater = config.get("plugins", {}).get("updater", {})
    if config["bundle"].get("targets") != ["nsis"] or config["bundle"].get("createUpdaterArtifacts") is not True:
        raise ValueError("Public builds require NSIS only and signed Tauri v2 updater artifacts")
    if updater.get("endpoints") != [PAGES + "/app/latest.json"] or not updater.get("pubkey"):
        raise ValueError("The fixed public updater endpoint and public signing key are required")
    public_key = base64.b64decode(updater["pubkey"], validate=True)
    if not public_key.startswith(b"untrusted comment:") or len(public_key) < 64:
        raise ValueError("Invalid Tauri updater public signing key format")
    if updater.get("dangerousInsecureTransportProtocol"):
        raise ValueError("Insecure updater transport is not allowed")
    if updater.get("requireSignedVersion") is not True:
        raise ValueError("Updates must require a version-bound trusted signature")
    release_config()
    if not allow_internal:
        check_public_paths(tracked_paths())
    if verification:
        if MARKER not in tracked_paths():
            raise ValueError("A tracked, current local release verification marker is required")
        check_verification(current)
    return current


def asset_names(current: str) -> set[str]:
    app = f"NekoNekoAsset_{current}_x64-setup.exe"
    bridge = f"{PACKAGE}-{current}"
    return {app, app + ".sig", bridge + ".unitypackage", bridge + ".tgz",
            bridge + ".zip", bridge + ".inventory.json", "SHA256SUMS.txt"}


def signature_value(value: str, expected_version: str, public_key: str | None = None) -> str:
    value = value.strip()
    decoded = base64.b64decode(value, validate=True)
    lines = decoded.decode("ascii").splitlines()
    if len(lines) != 4 or not lines[0].startswith("untrusted comment:") or not lines[2].startswith("trusted comment: "):
        raise ValueError("Invalid Tauri updater signature format")
    packet = base64.b64decode(lines[1], validate=True)
    if len(packet) != 74 or len(base64.b64decode(lines[3], validate=True)) != 64:
        raise ValueError("Invalid minisign signature packet")
    signed_versions = [field.removeprefix("version:") for field in lines[2][17:].split("\t") if field.startswith("version:")]
    if signed_versions != [expected_version]:
        raise ValueError("Updater signature must bind exactly the released version")
    if public_key:
        key_lines = base64.b64decode(public_key, validate=True).decode("ascii").splitlines()
        key = base64.b64decode(key_lines[1], validate=True)
        if len(key) != 42 or key[2:10] != packet[2:10]:
            raise ValueError("Updater signature key does not match the configured public key")
    return value


def collect_app(bundle: Path, output: Path) -> None:
    current = version()
    installers = list(bundle.glob("*.exe"))
    if len(installers) != 1 or any(bundle.glob("*.zip")):
        raise ValueError("Expected exactly one NSIS installer and no portable ZIP")
    installer = installers[0]
    signature = installer.with_name(installer.name + ".sig")
    signature_value(signature.read_text(encoding="ascii"), current,
                    read_json("app/src-tauri/tauri.conf.json")["plugins"]["updater"]["pubkey"])
    output.mkdir(parents=True, exist_ok=True)
    name = f"NekoNekoAsset_{current}_x64-setup.exe"
    shutil.copyfile(installer, output / name)
    shutil.copyfile(signature, output / (name + ".sig"))


def check_artifacts(directory: Path, current: str, with_checksums: bool) -> list[Path]:
    expected = asset_names(current) - (set() if with_checksums else {"SHA256SUMS.txt"})
    files = sorted(directory.iterdir())
    if any(not path.is_file() or path.is_symlink() for path in files) or {p.name for p in files} != expected:
        raise ValueError("Release assets differ from the installer/signature/three Bridge formats/inventory allowlist")
    if any(path.stat().st_size == 0 for path in files):
        raise ValueError("A release asset is empty")
    signature_value((directory / f"NekoNekoAsset_{current}_x64-setup.exe.sig").read_text(encoding="ascii"), current,
                    read_json("app/src-tauri/tauri.conf.json")["plugins"]["updater"]["pubkey"])
    inventory = json.loads((directory / f"{PACKAGE}-{current}.inventory.json").read_text(encoding="utf-8"))
    if inventory["packageName"] != PACKAGE or inventory["version"] != current:
        raise ValueError("Bridge inventory version or identity does not match")
    if set(inventory["archives"]) != {"unitypackage", "upm", "vpm"}:
        raise ValueError("All three Bridge archives need verified inventory records")
    for details in inventory["archives"].values():
        path = directory / details["file"]
        if path.name not in expected or hashlib.sha256(path.read_bytes()).hexdigest() != details["sha256"]:
            raise ValueError("Bridge archive differs from its verified source inventory")
        if details.get("allMemberBytesMatch") is not True:
            raise ValueError("Bridge archive byte verification did not pass")
    return files


class GitHub:
    def __init__(self) -> None:
        self.repo = os.environ.get("GITHUB_REPOSITORY", REPOSITORY)
        if self.repo != REPOSITORY:
            raise ValueError("Publishing is restricted to the NekoNekoAsset public repository")
        self.token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        if not self.token:
            raise ValueError("GitHub token is required for release operations")

    def request(self, route: str, method: str = "GET", data=None, upload: Path | None = None):
        host = "uploads.github.com" if upload else "api.github.com"
        url = f"https://{host}/repos/{self.repo}/{route}"
        headers = {"Authorization": "Bearer " + self.token, "Accept": "application/vnd.github+json",
                   "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "NekoNekoAsset-release"}
        body = None
        if upload:
            body = upload.read_bytes()
            headers["Content-Type"] = "application/octet-stream"
        elif data is not None:
            body = json.dumps(data).encode("utf-8")
            headers["Content-Type"] = "application/json"
        try:
            with urllib.request.urlopen(urllib.request.Request(url, data=body, headers=headers, method=method), timeout=180) as response:
                result = response.read()
                return json.loads(result) if result else None
        except urllib.error.HTTPError as error:
            if method == "GET" and error.code == 404:
                return None
            # Do not dump arbitrary API bodies or request headers into CI logs.
            raise ValueError(f"GitHub {method} failed with HTTP {error.code}") from None

    def release(self, tag: str):
        published = self.request("releases/tags/" + urllib.parse.quote(tag, safe=""))
        if published is not None:
            return published
        # The tag endpoint exposes published releases only. Authenticated listing
        # also exposes drafts to the workflow's contents-write token.
        found = None
        page = 1
        while True:
            batch = self.request(f"releases?per_page=100&page={page}")
            if not isinstance(batch, list):
                raise ValueError("Unable to list releases while looking for the prepared draft")
            for candidate in batch:
                if candidate.get("tag_name") == tag:
                    if found is not None:
                        raise ValueError("Multiple releases have the exact requested tag")
                    found = candidate
            if len(batch) < 100:
                return found
            page += 1

    def assert_tag_checkout(self, tag: str) -> None:
        reference = self.request("git/ref/tags/" + urllib.parse.quote(tag, safe=""))
        if not reference:
            raise ValueError("The release tag is missing")
        obj = reference["object"]
        while obj["type"] == "tag":
            obj = self.request("git/tags/" + obj["sha"])["object"]
        if obj["type"] != "commit" or obj["sha"] != git("rev-parse", "HEAD").decode().strip():
            raise ValueError("Release checkout differs from the remote tag commit")


def set_output(**values) -> None:
    if destination := os.environ.get("GITHUB_OUTPUT"):
        with open(destination, "a", encoding="utf-8") as stream:
            for key, value in values.items():
                stream.write(f"{key}={value}\n")


def prepare(tag: str) -> None:
    current = check(tag, True)
    api = GitHub()
    api.assert_tag_checkout(tag)
    existing = api.release(tag)
    if existing and not existing["draft"]:
        set_output(published="true", version=current)
        print("Release already published; artifacts remain immutable. Refreshing Pages only.")
        return
    config = release_config()
    details = {"tag_name": tag, "target_commitish": git("rev-parse", "HEAD").decode().strip(),
               "name": f"NekoNekoAsset {current}" + (" Beta" if config["prerelease"] else ""),
               "body": config["notes"], "draft": True, "prerelease": config["prerelease"]}
    if existing:
        api.request(f"releases/{existing['id']}", "PATCH", details)
    else:
        api.request("releases", "POST", details)
    set_output(published="false", version=current)
    print("Draft release ready; nothing is public until both build jobs pass.")


def publish(tag: str, directory: Path) -> None:
    current = check(tag, True)
    api = GitHub()
    api.assert_tag_checkout(tag)
    release = api.release(tag)
    if not release:
        raise ValueError("The prepared draft release is missing")
    if not release["draft"]:
        print("Published release is immutable; no artifacts changed.")
        return
    files = check_artifacts(directory, current, False)
    checksums = "".join(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n" for path in files)
    (directory / "SHA256SUMS.txt").write_text(checksums, encoding="ascii", newline="\n")
    files = check_artifacts(directory, current, True)
    expected = asset_names(current)
    if any(asset["name"] not in expected for asset in release["assets"]):
        raise ValueError("Draft has an unexpected asset; refusing to publish internal or portable files")
    for asset in release["assets"]:
        api.request(f"releases/assets/{asset['id']}", "DELETE")
    for path in files:
        api.request(f"releases/{release['id']}/assets?name=" + urllib.parse.quote(path.name, safe=""), "POST", upload=path)
    uploaded = api.release(tag)["assets"]
    if {asset["name"] for asset in uploaded} != expected or any(asset["state"] != "uploaded" for asset in uploaded):
        raise ValueError("Draft upload is incomplete")
    api.request(f"releases/{release['id']}", "PATCH", {"draft": False})
    print(f"Published {tag} with {len(files)} verified assets.")


def published_releases(api: GitHub) -> list[dict]:
    result = []
    page = 1
    while True:
        batch = api.request(f"releases?per_page=100&page={page}")
        if not isinstance(batch, list):
            raise ValueError("Unable to list public releases")
        result.extend(release for release in batch if not release["draft"]
                      and release["tag_name"].startswith("v") and VERSION.fullmatch(release["tag_name"][1:]))
        if len(batch) < 100:
            return result
        page += 1


def pages(output: Path) -> None:
    api = GitHub()
    releases = published_releases(api)
    if not releases:
        raise ValueError("No published App/Bridge release exists")
    release = max(releases, key=lambda value: tuple(map(int, value["tag_name"][1:].split("."))))
    current = release["tag_name"][1:]
    assets = {asset["name"]: asset for asset in release["assets"] if asset["state"] == "uploaded"}
    if set(assets) != asset_names(current):
        raise ValueError("Newest published release is incomplete; keep the existing update feed")
    installer = f"NekoNekoAsset_{current}_x64-setup.exe"
    # Signature assets are public. Fetch without an authentication header across redirects.
    with urllib.request.urlopen(assets[installer + ".sig"]["browser_download_url"], timeout=60) as response:
        signature = response.read(65537)
    if len(signature) > 65536:
        raise ValueError("Updater signature asset is too large")
    signature = signature_value(signature.decode("ascii"), current)
    documents = {"app/latest.json": {"version": current, "notes": release["body"] or "",
                 "pub_date": release["published_at"], "platforms": {"windows-x86_64": {
                     "url": assets[installer]["browser_download_url"], "signature": signature}}},
                 "bridge/latest.json": {"version": current,
                     "unitypackageUrl": assets[f"{PACKAGE}-{current}.unitypackage"]["browser_download_url"],
                     "releaseUrl": release["html_url"]}}
    for relative, document in documents.items():
        path = output / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (output / ".nojekyll").write_text("", encoding="utf-8")
    print(f"Pages feed regenerated from highest published version {current} (including Beta releases).")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("source-digest")
    validate = commands.add_parser("check")
    validate.add_argument("--tag")
    validate.add_argument("--require-verification", action="store_true")
    validate.add_argument("--allow-internal", action="store_true", help="Local integration only; never used by public CI")
    preflight = commands.add_parser("prepare")
    preflight.add_argument("--tag", required=True)
    collect = commands.add_parser("collect-app")
    collect.add_argument("--bundle", type=Path, required=True)
    collect.add_argument("--output", type=Path, required=True)
    publishing = commands.add_parser("publish")
    publishing.add_argument("--tag", required=True)
    publishing.add_argument("--artifacts", type=Path, required=True)
    feed = commands.add_parser("pages")
    feed.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "source-digest":
        print(source_digest())
    elif args.command == "check":
        print("Release inputs valid: " + check(args.tag, args.require_verification, args.allow_internal))
    elif args.command == "prepare":
        prepare(args.tag)
    elif args.command == "collect-app":
        collect_app(args.bundle, args.output)
    elif args.command == "publish":
        publish(args.tag, args.artifacts)
    elif args.command == "pages":
        pages(args.output)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, FileNotFoundError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f"Release check failed: {error}", file=sys.stderr)
        sys.exit(1)
