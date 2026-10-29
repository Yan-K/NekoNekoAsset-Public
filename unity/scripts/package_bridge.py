"""Package one Editor-only UPM folder as unitypackage, UPM TGZ, and VPM ZIP.

Python standard library only. All three archives preserve the source bytes and
stable Unity GUIDs. The unitypackage installs the same Packages/ identity.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile


PACKAGE_NAME = "com.yank.nekonekoasset.bridge"
GUID_PATTERN = re.compile(r"^guid:\s*([0-9a-f]{32})\s*$", re.MULTILINE)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def source_inventory(source: Path) -> tuple[dict, list[dict]]:
    source = source.resolve(strict=True)
    if source.is_symlink() or not source.is_dir():
        raise ValueError("Package source must be a regular directory")
    metadata = json.loads((source / "package.json").read_text(encoding="utf-8"))
    if metadata.get("name") != PACKAGE_NAME:
        raise ValueError("Unexpected package identity")
    entries = []
    guids = set()
    tracked = None
    repository = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=source,
                                capture_output=True, check=False)
    if repository.returncode == 0:
        inventory = subprocess.run(["git", "ls-files", "-z", "--", "."], cwd=source,
                                   capture_output=True, check=True)
        tracked = set(inventory.stdout.decode("utf-8").split("\0")) - {""}
        for relative in tracked:
            if not (source / relative).is_file():
                raise ValueError(f"Tracked package input missing: {relative}")
    for path in sorted(source.rglob("*"), key=lambda value: value.as_posix()):
        rel = path.relative_to(source).as_posix()
        if tracked is not None and rel not in tracked and not any(name.startswith(rel + "/") for name in tracked):
            continue
        if path.is_symlink():
            raise ValueError(f"Symlink is not allowed: {path.relative_to(source)}")
        if rel.endswith(".meta"):
            if not path.with_suffix("").exists():
                raise ValueError(f"Orphan meta: {rel}")
            continue
        if path.is_dir() and not any(path.iterdir()):
            raise ValueError(f"Empty package directory: {rel}")
        meta_path = path.with_name(path.name + ".meta")
        if not meta_path.is_file():
            raise ValueError(f"Stable meta missing: {rel}")
        meta = meta_path.read_bytes()
        match = GUID_PATTERN.search(meta.decode("utf-8"))
        if not match or match.group(1) in guids:
            raise ValueError(f"Missing or duplicate GUID: {rel}")
        guid = match.group(1)
        guids.add(guid)
        data = b"" if path.is_dir() else path.read_bytes()
        entries.append({"relativePath": rel, "packagePath": f"Packages/{PACKAGE_NAME}/{rel}",
                        "guid": guid, "isDirectory": path.is_dir(),
                        "sizeBytes": len(data), "sha256": sha256(data),
                        "metaSha256": sha256(meta), "data": data, "meta": meta})
    return metadata, entries


def tar_bytes(members: list[tuple[str, bytes]]) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
            for name, data in members:
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o644
                info.mtime = 0
                archive.addfile(info, io.BytesIO(data))
    return output.getvalue()


def unity_members(entries: list[dict]) -> list[tuple[str, bytes]]:
    result = []
    for entry in entries:
        guid = entry["guid"]
        if not entry["isDirectory"]:
            result.append((f"{guid}/asset", entry["data"]))
        result.extend([(f"{guid}/asset.meta", entry["meta"]),
                       (f"{guid}/pathname", entry["packagePath"].encode("utf-8"))])
    return result


def package_members(entries: list[dict], prefix: str) -> list[tuple[str, bytes]]:
    result = []
    for entry in entries:
        rel = entry["relativePath"]
        if not entry["isDirectory"]:
            result.append((prefix + rel, entry["data"]))
        result.append((prefix + rel + ".meta", entry["meta"]))
    return sorted(result)


def read_tar(path: Path) -> dict[str, bytes]:
    with tarfile.open(path, "r:gz") as archive:
        result = {}
        for member in archive.getmembers():
            if not member.isfile() or member.name in result:
                raise ValueError(f"Invalid or duplicate tar member: {member.name}")
            if member.name.startswith("/") or ".." in Path(member.name).parts:
                raise ValueError(f"Unsafe tar member: {member.name}")
            stream = archive.extractfile(member)
            assert stream is not None
            result[member.name] = stream.read()
        return result


def verify_exact(actual: dict[str, bytes], expected: list[tuple[str, bytes]], label: str) -> dict:
    required = dict(expected)
    if set(actual) != set(required):
        raise ValueError(f"{label} inventory differs from source")
    for name, data in required.items():
        if actual[name] != data:
            raise ValueError(f"{label} bytes differ: {name}")
    return {"members": len(actual), "allMemberBytesMatch": True}


def package(source: Path, output: Path) -> dict:
    metadata, entries = source_inventory(source)
    output.mkdir(parents=True, exist_ok=True)
    stem = f"{PACKAGE_NAME}-{metadata['version']}"
    paths = {"unitypackage": output / (stem + ".unitypackage"),
             "upm": output / (stem + ".tgz"), "vpm": output / (stem + ".zip")}
    u_members = unity_members(entries)
    t_members = package_members(entries, "package/")
    z_members = package_members(entries, "")
    paths["unitypackage"].write_bytes(tar_bytes(u_members))
    paths["upm"].write_bytes(tar_bytes(t_members))
    with zipfile.ZipFile(paths["vpm"], "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in z_members:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data)
    checks = {"unitypackage": verify_exact(read_tar(paths["unitypackage"]), u_members, "unitypackage"),
              "upm": verify_exact(read_tar(paths["upm"]), t_members, "UPM")}
    with zipfile.ZipFile(paths["vpm"]) as archive:
        if len(archive.namelist()) != len(set(archive.namelist())):
            raise ValueError("Duplicate VPM ZIP members")
        checks["vpm"] = verify_exact({name: archive.read(name) for name in archive.namelist()}, z_members, "VPM")
    checks["upm"]["packageJsonAtRequiredRoot"] = "package/package.json" in dict(t_members)
    checks["vpm"]["packageJsonAtRequiredRoot"] = "package.json" in dict(z_members)
    report = {"packageName": PACKAGE_NAME, "version": metadata["version"],
              "installPath": f"Packages/{PACKAGE_NAME}",
              "entries": [{key: value for key, value in entry.items() if key not in ("data", "meta")} for entry in entries],
              "archives": {kind: {"file": path.name, "sha256": sha256(path.read_bytes()),
                                    "sizeBytes": path.stat().st_size, **checks[kind]} for kind, path in paths.items()}}
    (output / (stem + ".inventory.json")).write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    report = package(args.source, args.output)
    print(json.dumps({"packageName": report["packageName"], "version": report["version"],
                      "sourceEntries": len(report["entries"]), "archives": report["archives"]}, indent=2))

