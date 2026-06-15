---
description: Cut a Mesh-Talk release — bump version, tag, and let CI build/sign the bundles
argument-hint: "[patch|minor|major|X.Y.Z]"
allowed-tools: Bash, Read, Edit
---

Releases are tag-driven: pushing a `vX.Y.Z` tag triggers
`.github/workflows/release.yml`, which builds macOS (x64 + arm64) and Windows
bundles, generates SHA256SUMS, signs them with cosign, and uploads to the
GitHub Release. For a local dev run use `/dev`.

Argument: `$ARGUMENTS` — a bump kind (`patch`/`minor`/`major`) or an explicit
`X.Y.Z`. Default `patch`.

## Steps

1. **Pre-flight**: working tree clean, on `main` (or release branch), and
   `make check` green. Do NOT release with a dirty tree or red CI.
2. **Compute the new version** from the current `version` in
   `src-tauri/Cargo.toml` and the requested bump.
3. **Bump in both places** so they stay in sync:
   - `src-tauri/Cargo.toml` → `version = "X.Y.Z"`
   - `src-tauri/tauri.conf.json` → `"version": "X.Y.Z"`
   Update `Cargo.lock` (`cargo update -p mesh-talk --precise X.Y.Z` or
   `nice -n 10 cargo check`).
4. **Commit**: `chore(release): vX.Y.Z`.
5. **Tag + push**:
   ```bash
   git tag vX.Y.Z
   git push origin <branch> && git push origin vX.Y.Z
   ```
6. **Watch** the `Tauri Release` workflow; confirm bundles + `*.bundle`
   signatures attached to the GitHub Release.

## Report

State the old → new version, the files bumped, the tag pushed, and a link to
the running release workflow. If pre-flight failed, stop and say why — never tag
over a red build.
