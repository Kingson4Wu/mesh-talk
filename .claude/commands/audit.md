---
description: Audit Mesh-Talk — quality gates, dependency vulnerabilities, and secret scan
argument-hint: "[quick|full]"
allowed-tools: Bash, Read
---

Run the project's quality and security gates and summarize findings. Mirrors what
CI enforces (`.github/workflows/ci.yml`, `gitleaks.yml`).

Argument: `$ARGUMENTS` — `quick` (default) or `full`.

> Always prefix cargo with `nice -n 10` and throttle tests
> (`-- --test-threads=2`); the `.claude` CPU guard enforces this.

## quick (default)

- `cd src-tauri && nice -n 10 cargo fmt --all -- --check` — formatting.
- `cd src-tauri && nice -n 10 cargo clippy --all-targets -- -D warnings` — lints.
- `cd src-tauri && nice -n 10 cargo test -- --test-threads=2` — tests.

## full

Everything in `quick`, plus:

- **Rust deps**: `cargo audit` (install with `cargo install --locked cargo-audit`
  if missing) — RustSec advisories.
- **Frontend deps**: `cd frontend && npm audit --audit-level=high`.
- **Secrets**: `gitleaks git . --config .gitleaks.toml --redact -v` (install the
  gitleaks CLI if missing) — credential-shaped strings in the tree/history.
- **Full health check**: `make check` (chains fmt, clippy, prettier, eslint,
  tests, builds) — heavy; run when you want the same checks as the
  `Code Quality and Health Check` workflow.

## Report

List each gate as ✅/❌ with the key finding. For any failure, name the file and
the specific issue. Do not claim "all clear" unless every gate you ran passed —
quote the command output.
