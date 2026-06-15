# Contributing

Mesh-Talk is a peer-to-peer LAN chat app: a Rust ([Tauri](https://tauri.app/))
backend in `src-tauri/` and a Vue + Vite frontend in `frontend/`. A headless CLI
(`mesh-talk-cli`) drives the same core for testing without the GUI.

## Development setup

```bash
git clone https://github.com/Kingson4Wu/mesh-talk.git
cd mesh-talk
make dev            # installs Rust + Node deps and sets up git hooks
```

Common loops:

```bash
make tauri-dev      # run the desktop app (backend + frontend) with hot reload
make frontend-dev   # frontend only (Vite dev server)
cargo run -p mesh-talk --bin mesh-talk-cli -- --name alice --port 9000   # headless CLI
```

## Build, test, lint

| Command | What it does |
|---------|--------------|
| `make build` | `cargo build --release` |
| `make test` | Rust test suite (`cd src-tauri && cargo test`) |
| `make lint` | `cargo clippy -- -D warnings` |
| `make format` | `cargo fmt` + `prettier` on the frontend |
| `make check` | full health check (`scripts/check-health.sh`) |

> **CPU note.** The test suite generates RSA-2048 keys in many tests and cargo
> parallelizes to all cores. `.cargo/config.toml` caps `jobs` and
> `RUST_TEST_THREADS`; for ad-hoc runs prefer `cargo test -- --test-threads=2`.
> A `PreToolUse` hook in `.claude/settings.json` enforces this for Claude Code.

Filter tests while iterating: `cd src-tauri && cargo test contacts::`.

## Coding style

- **Rust**: `cargo fmt` (4-space indent, `snake_case` modules, `UpperCamelCase`
  types) and `cargo clippy -- -D warnings`. Keep unit tests inline in a
  `#[cfg(test)] mod tests`.
- **Frontend**: `npx prettier --write src/` in `frontend/`.
- Match the surrounding code; see `specifications/development_conventions.md`.

## Editor / Claude Code feedback

Code-quality feedback in this repo is **tool-agnostic and committed**, so you get
it on clone without configuring an editor:

- A `PostToolUse` hook in `.claude/settings.json` runs `cargo fmt` + `clippy` on
  Rust files Claude Code edits.
- CI (`.github/workflows/ci.yml`) gates formatting, Clippy (`-D warnings`),
  tests, the frontend build, and a `cargo audit` dependency scan.

## Architecture

See [`CONTEXT.md`](CONTEXT.md) for the domain model and layering, and
[`AGENTS.md`](AGENTS.md) for repository conventions. In short: the frontend talks
to the backend only through Tauri `commands`; networking changes (`udp`, `tcp`,
discovery, reconnection) should mirror the docs under `specifications/`.

## Commit & pull request guidelines

- Use [Conventional Commits](https://www.conventionalcommits.org/) with scopes,
  e.g. `feat(network): …`, `fix(contacts): …`. See
  `specifications/git_standards.md`.
- Squash WIP commits; link related issues.
- Run `make check` before requesting review, and state the validation commands
  you ran in the PR (minimum `make test`).
- Networking changes should ship an integration assertion proving discovery or
  relay behavior.

## Reporting issues

- 🐛 [Report a bug](https://github.com/Kingson4Wu/mesh-talk/issues/new?template=bug_report.yml)
- ✨ [Request a feature](https://github.com/Kingson4Wu/mesh-talk/issues/new?template=feature_request.yml)
- 🔒 Security: see [`SECURITY.md`](SECURITY.md) — do **not** open a public issue.
