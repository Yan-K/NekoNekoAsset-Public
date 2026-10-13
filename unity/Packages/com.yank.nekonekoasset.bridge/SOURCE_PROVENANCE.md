# Smart Package source provenance

Source: https://github.com/Yan-K/AvatarToolbox
Commit: cb58555726b9a241dd58c7812f9e4058137d74a7 (package 1.8.0).
License: MIT, Copyright (c) 2024 Yan-K; retained in THIRD_PARTY_NOTICES.md.
Source base: Packages/com.yank.avatartoolbox/Editor/SmartPackage/.

The Bridge adapts only the importer subset listed below. It has no dependency on
AvatarToolbox's runtime, exporter, main shell or update functionality.

| Original | Bridge destination | Adaptation |
|---|---|---|
| Core/ProjectPackagePath.cs | Editor/SmartPackage/Core/ProjectPackagePath.cs | Independent namespace; preserve project-relative validation |
| IO/TarEntry.cs, TarReader.cs, GuidUtility.cs, UnityPackageEntry.cs, UnityPackageReader.cs | Editor/SmartPackage/IO/ (same basenames) | Independent namespace; bounded streaming payload staging |
| Importer/IAssetProbe.cs, SnapshotAssetProbe.cs, LoadedPackage.cs, ImportPlan.cs, ImportSession.cs, ImportConflictResolver.cs | Editor/SmartPackage/Importer/ (same basenames) | Independent namespace; explicit staged inputs, per-entry results/meta handling; UI conflict subset separated |
| Model/PackageAssetNode.cs, ConflictPolicy.cs | Editor/SmartPackage/Model/ (same basenames) | Independent namespace; importer-only model |
| SmartPackageTool.TreeView.cs | Editor/SmartPackage/Tree/ImportReviewWindow.TreeView.cs | Importer drawing subset; independent review window/localization |
| SmartPackageTool.Importer.cs | Editor/SmartPackage/UI/ImportReviewWindow.Packages.cs | Header/tallies/conflict-preview subset only |
| Importer/ImportSession.cs (ConflictDecisionWindow) | Editor/SmartPackage/UI/ImportConflictWindow.cs | Native overwrite/skip/all UI subset; no shell dependency |

Original Bridge code: transport DTO/server, discovery, receipts, review lifecycle,
loose Assets destination, Windows BCrypt SHA-256 binding, independent localization
and packaging tools. Update this ledger if the adapted upstream subset changes.

## Current writer adaptations

The fixed upstream ImportSession was compared at the commit above. Its writer
creates parents via Directory.CreateDirectory and has no reviewed-observation
comparison; the Bridge added that comparison for confirmed-plan safety. Archive
entry order is retained. Bridge now advances observations only for its own newly
created parents, preserving GUID/meta/content checks, and continues independent
entries after ordinary per-item failure (durable-checkpoint failures still stop).

Original Bridge changes: optional fingerprinted App locale, compact two-column
native review and per-file Assets destinations, counted progress, native terminal
results, and hash-guarded cleanup of new writes. Cleanup preserves overwritten
preexisting targets, later changes, and unprovable implicit parents. No additional
upstream files or dependencies were copied. ImportSession, ImportReviewWindow,
BridgeImport/Runtime/Types/Localization and new BridgeImportResult are adapted or
original Bridge changes; MIT notice and all preexisting stable GUIDs remain.

## Current preparation and review adaptations

The existing TAR/GUID/package reader and import-plan subset now verifies the
compressed digest during the metadata read, reuses a bounded read buffer and
reports byte/entry preparation progress. Parsed packages and initial previews
survive Editor busy/compile deferral. AssetDatabase observations are captured
on the main thread; filesystem target planning runs in the preparation worker.
Review/write guards remain. Runtime/receipt/review changes are original Bridge
work. No additional upstream file or dependency was copied; existing MIT notices
and stable GUIDs remain.

Loose preparation reuses its verified digest; Windows hashing uses the authored
BCrypt binding without changing SHA-256 values. Busy Editor preparation reports
an additive waiting flag and resumes through Editor updates. Review destinations
use owned drafts and one unified loose tree; coalesced background previews clone
mutable inputs and discard stale results, while confirmation still captures fresh
project observations. Target-shape and bulk selection checks avoid repeated
descendant scans. These are Bridge adaptations of the same pinned subset.
