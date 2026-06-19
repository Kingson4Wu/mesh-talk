# Technology Stack

## Backend (Rust, edition 2021)
- **Tauri 2** — desktop shell (native webview).
- **Tokio** — async runtime (TCP + UDP).
- **Crypto**: `ed25519-dalek` (signing), `x25519-dalek` (DH), `snow` (Noise_XX),
  `aes-gcm` / ChaChaPoly (AEAD), `sha2` + `hkdf` (KDFs), `pbkdf2` (at-rest key
  derivation, 600k rounds). Note: `rand` stays at **0.8** (the dalek crates need
  `rand_core` 0.6).
- **Serialization**: `bincode` (wire + at-rest records), `serde` / `serde_json` (IPC).
- **CLI**: `clap` (the `mesh-talk-node` headless/post-office binary).

## Frontend
- **Vue 3** + **Pinia** + **Vue Router** (hash history), built with **Vite**.

## Tooling / CI
- Cargo, `rustfmt`, **Clippy (`--all-targets -D warnings`)**, `cargo test`,
  `cargo-deny`, `cargo-machete`, `typos`, `gitleaks`, `shellcheck`, ESLint/Prettier —
  all run by `scripts/check-health.sh` (the pre-commit gate) and mirrored in CI.

## Platforms
macOS, Windows, Linux.

> See **[`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md)** for how these fit together.
