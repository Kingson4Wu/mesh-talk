<a id="readme-top"></a>

# Mesh-Talk

[![CI](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/ci.yml/badge.svg)](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/ci.yml)
[![Fuzz](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/fuzz.yml/badge.svg)](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/fuzz.yml)
[![Mutants](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/mutants.yml/badge.svg)](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/mutants.yml)
[![Gitleaks](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/gitleaks.yml/badge.svg)](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/gitleaks.yml)
[![Scorecard](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/scorecard.yml/badge.svg)](https://github.com/Kingson4Wu/mesh-talk/actions/workflows/scorecard.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/Kingson4Wu/mesh-talk/badge)](https://scorecard.dev/viewer/?uri=github.com/Kingson4Wu/mesh-talk)
[![codecov](https://codecov.io/gh/Kingson4Wu/mesh-talk/branch/main/graph/badge.svg)](https://codecov.io/gh/Kingson4Wu/mesh-talk)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2021-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app/)
[![React](https://img.shields.io/badge/React-18-61DAFB?logo=react&logoColor=black)](https://react.dev/)
[![platform: macOS | Windows | Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-000000?logo=linux&logoColor=white)](#prerequisites)
[![DeepWiki](https://img.shields.io/badge/DeepWiki-docs-8A2BE2)](https://deepwiki.com/Kingson4Wu/mesh-talk)

A local network chat tool written in Rust that enables users to communicate directly with others on the same network using UDP broadcast and TCP connections.

<p align="center">
  <a href="CONTEXT.md"><strong>Domain & architecture »</strong></a>
  ·
  <a href="https://github.com/Kingson4Wu/mesh-talk/issues/new?template=bug_report.yml">Report Bug</a>
  ·
  <a href="https://github.com/Kingson4Wu/mesh-talk/issues/new?template=feature_request.yml">Request Feature</a>
</p>

## Features

- Local network communication without a central server
- Automatic peer discovery using UDP broadcast
- Real-time messaging between connected peers via TCP
- Command-line interface
- Built with async Rust using Tokio
- Modular architecture following professional Rust project structure

## Project Structure

```
mesh-talk/
├── crates/mesh-talk-core/  # UI-free protocol core / SDK foundation (no Tauri dep)
│   ├── src/
│   │   ├── lib.rs          # crate root (lib `mesh_talk_core`)
│   │   ├── node/           # the serverless node: orchestration
│   │   ├── identity/ transport/ discovery/ eventlog/ ratchet/ channel/ dm.rs file/ postoffice/
│   │   ├── storage/        # at-rest encryption (PBKDF2 + AES-GCM)
│   │   └── bin/mesh-talk-node.rs  # headless node CLI (--post-office relay mode)
│   ├── tests/              # cross-process integration tests
│   └── Cargo.toml
├── src-tauri/              # Tauri desktop shell — a thin layer over mesh-talk-core
│   ├── src/
│   │   ├── main.rs         # `mesh-talk` desktop binary
│   │   ├── lib.rs          # Tauri setup + IPC registration
│   │   ├── commands.rs     # auth IPC (chat_commands.rs = messaging IPC)
│   │   ├── events.rs tray.rs state.rs perf.rs
│   │   └── services/       # auth only
│   ├── Cargo.toml
│   └── tauri.conf.json
├── frontend/               # React + TS + Tailwind frontend
├── docs/ARCHITECTURE.md    # architecture reference
├── specifications/         # overview + process/convention docs
├── Makefile
└── Cargo.toml              # workspace config + shared [workspace.dependencies]
```

## Download & first run

Grab the package for your OS from the [**Releases**](https://github.com/Kingson4Wu/mesh-talk/releases)
page. The builds are **free and unsigned** (no paid Apple/Windows code-signing certificate), so
macOS and Windows show a one-time "unidentified developer" / SmartScreen prompt the first time —
this is expected for unsigned open-source software and does **not** mean the app is unsafe. Each
release also ships a `SHA256SUMS` list and a Sigstore `cosign` signature so you can verify the
download came from this project's CI. How to open, per platform:

- **Linux** — no prompt at all.
  - **AppImage** (portable, no install): `chmod +x Mesh-Talk_*.AppImage && ./Mesh-Talk_*.AppImage`
  - or install the `.deb` / `.rpm` (adds an app-menu entry): `sudo dpkg -i mesh-talk_*.deb`
- **macOS** — open the `.dmg`, drag **Mesh-Talk** to Applications. On first launch macOS blocks an
  unsigned app, so **right-click the app → Open → Open** (only needed once). If it says
  "damaged", clear the quarantine flag: `xattr -dr com.apple.quarantine /Applications/Mesh-Talk.app`.
- **Windows** — run the `.exe` (or `.msi`) installer. SmartScreen shows "Windows protected your
  PC" → click **More info → Run anyway** (only the first time). WebView2 is fetched automatically
  if missing.

After the one-time approval it behaves like any installed app (Start-menu / Applications / app-menu
shortcut, icon, double-click to launch). To remove the prompt entirely you'd need paid signing
(Apple Developer ID for macOS, a code-signing cert or the Microsoft Store for Windows); Linux is
always prompt-free.

## Build from source

## Prerequisites

- Rust 2021 edition or later
- Cargo package manager

## Installation

Clone the repository and build the project:

```bash
git clone https://github.com/yourusername/mesh-talk.git
cd mesh-talk
cargo build --release
```

## Usage

Run the application with your desired name and port:

```bash
cargo run -- --name YourName --port 8000
```

### Command Line Arguments

- `--name` or `-n`: Your display name in the chat
- `--port` or `-p`: The TCP port to listen on for incoming connections

## How It Works

1. The application creates a mesh network where each node can communicate directly with other nodes
2. UDP broadcast is used for peer discovery on port 8888
3. TCP connections are established between peers for reliable message delivery
4. Messages are broadcast to all connected peers in the network

## Technical Details

- Uses Tokio for async runtime and networking
- Implements UDP broadcast for peer discovery
- TCP for reliable peer-to-peer communication
- JSON serialization for message encoding
- Thread-safe peer management using Arc and Mutex
- Modular design with clear separation of concerns

## Dependencies

- tokio: Async runtime and networking
- serde: Serialization framework
- serde_json: JSON serialization
- clap: Command line argument parsing

## Development

This project follows a professional Rust project structure — a layered workspace where the
protocol core is its own crate (`mesh-talk-core`) and the desktop app is a thin shell over it:
- Node orchestration in `crates/mesh-talk-core/src/node/`
- Crypto in `identity/`, `transport/` (Noise), `ratchet/`, `channel/`, `dm.rs` (all in the core crate)
- Event log + sync in `crates/mesh-talk-core/src/eventlog/`; signed discovery in `discovery/`
- At-rest encryption in `crates/mesh-talk-core/src/storage/`; auth (app-only) in `src-tauri/src/services/`
- Full architecture reference: `docs/ARCHITECTURE.md`

## Automation and Code Quality

The project includes several automation tools to maintain code quality and consistency:

### Code Formatting and Linting
- Automatic code formatting with `cargo fmt` and `prettier`
- Linting with `clippy` for Rust code
- Pre-commit hooks to enforce code quality

### Development Automation
Use the provided Makefile for common development tasks:
```bash
make dev        # Run in development mode
make build      # Build for release
make test       # Run tests
make lint       # Run linting tools
make fix        # Automatically fix code issues
make format     # Format code
```

### Automated Maintenance
The project includes scripts for automated maintenance:
- `scripts/check-health.sh` - Run all quality checks
- `scripts/auto-maintain.sh` - Run regular maintenance tasks
- Pre-commit hooks that automatically format and fix code before committing

### CI/CD Integration
- GitHub Actions workflow that runs all quality checks
- Automatic security scanning for dependencies
- Build verification on multiple platforms

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for the dev
setup, build/test/lint commands, and conventions. The domain model and
architecture are documented in [CONTEXT.md](CONTEXT.md) and [AGENTS.md](AGENTS.md).

## Security

Please report vulnerabilities privately — see [SECURITY.md](SECURITY.md). Do not
open a public issue for security reports.

## License

This project is open source under the [MIT License](LICENSE).

<p align="right">(<a href="#readme-top">back to top</a>)</p>
