"""Refresh exact Rust dependency notices and the bundled resource inventory.

Python 3.11+ standard library only. Existing original legal bytes and recorded
offsets remain unchanged. New crates are fetched from crates.io and checked
against Cargo.lock. Missing legal documents stop the refresh before any write.
Frontend dependency changes require their own notice refresh.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import tarfile
import tomllib
from urllib.error import HTTPError
from urllib.parse import quote, urlparse
from urllib.request import Request, urlopen


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def encoded(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def fetch(url: str) -> bytes:
    with urlopen(Request(url, headers={"User-Agent": "NekoNekoAsset-license-refresh"}), timeout=30) as response:
        return response.read()


def legal_name(name: str) -> bool:
    path = PurePosixPath(name)
    if path.suffix.lower() in {".rs", ".c", ".h", ".cpp", ".py", ".js", ".ts", ".toml", ".json"}:
        return False
    return bool(re.match(r"^(license|licence|copying|copyright|notice)(?:[._-].*)?$", path.name, re.I))


def check_notices(ledger: dict, aggregate: bytes) -> None:
    for entry in ledger["cargo"]:
        for notice in entry["notices"]:
            if notice["aggregate"] != "rust/NOTICES.txt":
                raise ValueError("Unexpected Rust notice aggregate")
            start, size = notice["offset"], notice["bytes"]
            if start < 0 or size <= 0 or digest(aggregate[start:start + size]) != notice["sha256"]:
                raise ValueError(f"Original notice mismatch: {entry['name']}-{entry['version']}")


def current_packages(lock: dict) -> list[dict]:
    result = []
    for entry in lock["package"]:
        if "source" not in entry:
            continue
        if entry["source"] != "registry+https://github.com/rust-lang/crates.io-index" or not entry.get("checksum"):
            raise ValueError("Only checksum-pinned crates.io dependencies are supported")
        if not re.fullmatch(r"[A-Za-z0-9_-]+", entry["name"]) or not re.fullmatch(r"[A-Za-z0-9.+_-]+", entry["version"]):
            raise ValueError("Unsafe dependency identity")
        result.append(entry)
    return result


def archive_files(data: bytes, stem: str) -> dict[str, bytes]:
    result = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive:
            parts = PurePosixPath(member.name).parts
            if not parts or parts[0] != stem or ".." in parts or member.name.startswith("/"):
                raise ValueError("Unexpected crate archive path")
            if member.isdir():
                continue
            if not member.isfile():
                raise ValueError("Non-file crate archive member")
            name = "/".join(parts[1:])
            if name in result:
                raise ValueError("Duplicate crate archive member")
            stream = archive.extractfile(member)
            assert stream is not None
            result[name] = stream.read()
    return result


def upstream_legal(metadata: dict, vcs: dict, stem: str) -> tuple[list[tuple[str, bytes, str]], dict]:
    repository = metadata.get("repository", "")
    parsed = urlparse(repository)
    parts = parsed.path.strip("/").removesuffix(".git").split("/")
    commit = vcs.get("git", {}).get("sha1", "")
    if parsed.scheme != "https" or parsed.hostname != "github.com" or len(parts) != 2 or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError(f"Original standalone legal files missing: {stem}; no pinned GitHub source")
    folder = f"upstream/{parts[0]}_{parts[1]}/{commit}"
    documents, found = [], []
    for name in ("LICENSE", "LICENSE.txt", "LICENSE.md", "LICENSE-MIT", "LICENSE-APACHE", "LICENSE-APACHE-2.0", "COPYING", "COPYRIGHT", "NOTICE", "NOTICE.txt"):
        url = f"https://raw.githubusercontent.com/{parts[0]}/{parts[1]}/{commit}/{name}"
        try:
            data = fetch(url)
        except HTTPError as error:
            if error.code == 404:
                continue
            raise
        if not data.strip():
            continue
        path = f"{folder}/{name}"
        found.append((path, data, url))
        blob = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
        documents.append({"file": path, "sourceUrl": url, "sourceGitBlobSha": blob,
                          "sha256": digest(data), "bytes": len(data),
                          "kind": "original-upstream-root-legal-file-at-published-commit", "originalBytesPreserved": True})
    if not found:
        raise ValueError(f"Original standalone legal files missing: {stem}")
    supplemental = {"name": metadata["name"], "version": metadata["version"],
                    "declaredLicense": metadata.get("license"), "repository": repository,
                    "repositoryEvidence": "published Cargo.toml", "publishedGitSha": commit,
                    "pathInVcs": vcs.get("path_in_vcs", ""), "documents": documents,
                    "status": "published-commit-upstream-legal-files-preserved", "notes": []}
    return found, supplemental


def refresh(root: Path, check: bool) -> dict:
    native = root / "app/src-tauri"
    resources = native / "resources"
    licenses = resources / "licenses"
    ledger_path = licenses / "dependency-ledger.json"
    ledger = json.loads(ledger_path.read_bytes())
    aggregate = (licenses / "rust/NOTICES.txt").read_bytes()
    check_notices(ledger, aggregate)
    lock_bytes = (native / "Cargo.lock").read_bytes()
    lock_hash = digest(lock_bytes)
    packages = current_packages(tomllib.loads(lock_bytes.decode("utf-8")))
    pnpm_hash = digest((root / "app/pnpm-lock.yaml").read_bytes())
    if ledger["pnpmLockSha256"] != pnpm_hash:
        raise ValueError("Frontend lock changed; refresh original frontend notices before Rust-only refresh")
    known = {(entry["name"], entry["version"], entry["source"]): entry for entry in ledger["cargo"]}
    supplemental_path = licenses / "supplemental/index.json"
    supplemental = json.loads(supplemental_path.read_bytes())
    for package in supplemental["packages"]:
        for document in package["documents"]:
            data = (licenses / "supplemental" / document["file"]).read_bytes()
            if len(data) != document["bytes"] or digest(data) != document["sha256"]:
                raise ValueError("Original supplemental notice mismatch")
    writes: dict[Path, bytes] = {}
    added = []
    for package in packages:
        key = (package["name"], package["version"], package["source"])
        if key in known:
            if known[key]["crateSha256"] != package["checksum"]:
                raise ValueError("Recorded crate checksum differs from lock")
            source = known[key].get("includedSource")
            if source and digest((licenses / source).read_bytes()) != package["checksum"]:
                raise ValueError("Corresponding-source checksum differs from lock")
            continue
        stem = f"{package['name']}-{package['version']}"
        if check:
            raise ValueError(f"Missing exact dependency notices: {stem}")
        url = f"https://static.crates.io/crates/{package['name']}/{quote(stem, safe='')}.crate"
        data = fetch(url)
        if digest(data) != package["checksum"]:
            raise ValueError(f"Published archive checksum differs from lock: {stem}")
        members = archive_files(data, stem)
        metadata = tomllib.loads(members["Cargo.toml"].decode("utf-8"))["package"]
        if (metadata["name"], metadata["version"]) != key[:2]:
            raise ValueError("Published crate identity mismatch")
        vcs = json.loads(members.get(".cargo_vcs_info.json", b"{}"))
        notice_files = [(name, value, "published-crate") for name, value in sorted(members.items()) if legal_name(name) and value.strip()]
        entry = {"ecosystem": "cargo", "name": package["name"], "version": package["version"],
                 "declaredLicense": metadata.get("license"), "licenseFile": metadata.get("license-file"),
                 "authors": metadata.get("authors", []), "repository": metadata.get("repository"),
                 "source": package["source"], "sourceUrl": url, "crateSha256": package["checksum"],
                 "vcs": vcs, "scope": "lockfile-superset including conditional-platform and build dependencies", "notices": []}
        if not notice_files:
            upstream, supplement = upstream_legal(metadata, vcs, stem)
            supplement.update({"crateArchiveUrl": url, "crateArchiveSha256": package["checksum"]})
            supplemental["packages"].append(supplement)
            for path, value, _ in upstream:
                writes[licenses / "supplemental" / path] = value
                notice_files.append((path, value, "original-upstream-root-legal-file"))
            entry["publishedPackageOmitsStandaloneNotices"] = True
            ledger["packagesOmittingStandaloneNotices"].append(stem)
        for path, value, origin in notice_files:
            aggregate += f"\n===== {stem} :: {path} =====\n".encode("utf-8")
            entry["notices"].append({"origin": origin, "path": path, "aggregate": "rust/NOTICES.txt",
                                     "offset": len(aggregate), "bytes": len(value), "sha256": digest(value)})
            aggregate += value + b"\n"
        if entry.get("publishedPackageOmitsStandaloneNotices") or "MPL-2.0" in (entry["declaredLicense"] or ""):
            entry["includedSource"] = f"sources/cargo/{stem}.crate"
            writes[licenses / entry["includedSource"]] = data
        ledger["cargo"].append(entry)
        added.append(stem)
    if not check:
        ledger["cargo"].sort(key=lambda entry: (entry["name"], entry["version"]))
        ledger["cargoLockSha256"] = lock_hash
        ledger["scopeNotes"][0] = "Rust entries conservatively cover every exact current Cargo.lock package, including build-only and conditional-platform crates, and retain previously recorded original legal documents; they are not a claim that every entry ships in the Windows executable."
        supplemental["cargoLockSha256"] = lock_hash
        supplemental_bytes = encoded(supplemental)
        ledger["supplementalIndexSha256"] = digest(supplemental_bytes)
        writes[supplemental_path] = supplemental_bytes
        writes[licenses / "rust/NOTICES.txt"] = aggregate
        writes[ledger_path] = encoded(ledger)
    elif ledger["cargoLockSha256"] != lock_hash or supplemental["cargoLockSha256"] != lock_hash:
        raise ValueError("Lock provenance hash differs from final lock")
    elif ledger["supplementalIndexSha256"] != digest(supplemental_path.read_bytes()):
        raise ValueError("Supplemental index hash differs from ledger")
    check_notices(ledger, aggregate)
    inventory_path = licenses / "resource-inventory.json"
    inventory = json.loads(inventory_path.read_bytes())
    configuration = json.loads((native / "tauri.conf.json").read_bytes())
    inventory["applicationVersion"] = configuration["version"]
    inventory["mappings"] = configuration["bundle"]["resources"]
    contents = {}
    for source, destination in inventory["mappings"].items():
        source_root = native / source
        if not source_root.is_dir() or not source.startswith("resources/") or not destination.endswith("/"):
            raise ValueError("Expected explicit resource directory mapping")
        for path in source_root.rglob("*"):
            if path.is_symlink():
                raise ValueError("Resource symlink is not allowed")
            if path.is_file() and path != inventory_path:
                contents[path] = writes.get(path, path.read_bytes())
        for path, data in writes.items():
            if path != inventory_path and path.is_relative_to(source_root):
                contents[path] = data
    inventory["sourceFiles"] = [{"resourcePath": path.relative_to(resources).as_posix(), "bytes": len(data), "sha256": digest(data)} for path, data in sorted(contents.items())]
    if check:
        if json.loads(inventory_path.read_bytes()) != inventory:
            raise ValueError("Packaged resource inventory differs from current source")
    else:
        writes[inventory_path] = encoded(inventory)
        for path, data in writes.items():
            path.parent.mkdir(parents=True, exist_ok=True)
            if not path.exists() or path.read_bytes() != data:
                path.write_bytes(data)
    return {"checked": check, "currentCargoPackages": len(packages), "recordedCargoEntries": len(ledger["cargo"]),
            "addedPackages": added, "resourceFiles": len(inventory["sourceFiles"]), "cargoLockSha256": lock_hash}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--check", action="store_true", help="Verify current ledger and resources without downloads or writes")
    args = parser.parse_args()
    print(json.dumps(refresh(args.root.resolve(), args.check), indent=2))
