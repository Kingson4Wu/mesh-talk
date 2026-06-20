#!/bin/bash

# Unified health check script for Mesh-Talk project
# This script runs all code quality checks and validations

set -e  # Exit on any error

echo "Running Mesh-Talk health checks..."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    case $1 in
        "success")
            echo -e "${GREEN}✓${NC} $2"
            ;;
        "warning")
            echo -e "${YELLOW}⚠${NC} $2"
            ;;
        "error")
            echo -e "${RED}✗${NC} $2"
            ;;
        *)
            echo "$2"
            ;;
    esac
}

# Function to check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Check if we're in the project root
if [ ! -f "Makefile" ] || [ ! -d "src-tauri" ] || [ ! -d "frontend" ]; then
    print_status "error" "Please run this script from the project root directory"
    exit 1
fi

# Check Rust toolchain
print_status "success" "Checking Rust toolchain..."
if ! command_exists cargo; then
    print_status "error" "Cargo not found. Please install Rust: https://www.rust-lang.org/"
    exit 1
fi

# Check Node.js and npm
print_status "success" "Checking Node.js and npm..."
if ! command_exists node || ! command_exists npm; then
    print_status "error" "Node.js or npm not found. Please install Node.js: https://nodejs.org/"
    exit 1
fi

# Install dependencies if not already installed
print_status "success" "Installing dependencies..."
make install-deps >/dev/null 2>&1 || true
make frontend-install >/dev/null 2>&1 || true

# Check code formatting (Rust)
print_status "success" "Checking Rust code formatting..."
# Run from the workspace root: `cargo fmt --all` invoked inside a member crate does NOT
# format the sibling crate, so it must run here to cover BOTH crates (mirrors CI).
if ! cargo fmt --all -- --check; then
    print_status "error" "Rust code formatting issues found. Run 'cargo fmt --all' to fix."
    exit 1
fi

# Apply automatic code formatting fixes (Rust)
print_status "success" "Applying automatic Rust code formatting fixes..."
if ! cargo fmt --all; then
    print_status "warning" "Failed to apply some Rust code formatting fixes automatically."
fi

# Check code formatting (Frontend)
print_status "success" "Checking frontend code formatting..."
if ! cd frontend && npx prettier --check src/; then
    print_status "error" "Frontend code formatting issues found. Run 'npx prettier --write src/' to fix."
    exit 1
fi
cd ..

# Apply automatic code formatting fixes (Frontend)
print_status "success" "Applying automatic frontend code formatting fixes..."
if ! cd frontend && npx prettier --write src/; then
    print_status "warning" "Failed to apply some frontend code formatting fixes automatically."
fi
cd ..

# Run Clippy (Rust linter). MUST mirror CI: --all-targets covers tests/benches/bins'
# test modules — without it, lints in test code only surface on CI (and break it).
print_status "success" "Running Rust linter (Clippy)..."
if ! cd src-tauri && cargo clippy --workspace --all-targets -- -D warnings; then
    print_status "error" "Rust linting issues found. Please fix Clippy warnings."
    exit 1
fi
cd ..

# Apply automatic Clippy fixes
print_status "success" "Applying automatic Clippy fixes..."
if ! cd src-tauri && cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged; then
    print_status "warning" "Failed to apply some Clippy fixes automatically."
fi
cd ..

# Spelling (typos) — mirrors CI's crate-ci/typos. Warn-skip if not installed.
print_status "success" "Checking spelling (typos)..."
if command_exists typos; then
    if ! typos; then
        print_status "error" "Spelling issues found (see _typos.toml to allowlist domain terms)."
        exit 1
    fi
else
    print_status "warning" "typos not installed (cargo install typos-cli) — skipping; CI will still check."
fi

# Supply-chain policy (cargo-deny) — mirrors CI. Warn-skip if not installed.
print_status "success" "Checking supply-chain policy (cargo-deny)..."
if command_exists cargo-deny; then
    if ! cd src-tauri && cargo deny check; then
        print_status "error" "cargo-deny policy violation (advisories/bans/licenses/sources)."
        exit 1
    fi
    cd ..
else
    print_status "warning" "cargo-deny not installed — skipping; CI will still check."
fi

# Unused dependencies (cargo-machete) — mirrors CI. Warn-skip if not installed.
print_status "success" "Checking for unused dependencies (cargo-machete)..."
if command_exists cargo-machete; then
    # Run from the workspace root so BOTH crates (core + app) are scanned.
    if ! cargo machete; then
        print_status "error" "cargo-machete found unused dependencies."
        exit 1
    fi
else
    print_status "warning" "cargo-machete not installed — skipping; CI will still check."
fi

# Secret scan (gitleaks) — mirrors CI. Warn-skip if not installed.
print_status "success" "Scanning for secrets (gitleaks)..."
if command_exists gitleaks; then
    if ! gitleaks git . --config .gitleaks.toml --redact -v >/dev/null 2>&1; then
        print_status "error" "gitleaks found a potential secret (see .gitleaks.toml to allowlist)."
        exit 1
    fi
else
    print_status "warning" "gitleaks not installed — skipping; CI will still check."
fi

# Shellcheck scripts — mirrors CI.
print_status "success" "Linting shell scripts (shellcheck)..."
if command_exists shellcheck; then
    if ! shellcheck --severity=warning scripts/*.sh .claude/hooks/*.sh 2>/dev/null; then
        print_status "error" "shellcheck found warnings."
        exit 1
    fi
else
    print_status "warning" "shellcheck not installed — skipping; CI will still check."
fi

# Run ESLint (Frontend linter). ESLint 9 uses flat config (eslint.config.js);
# the `--ext` flag was removed, so run the package script which lints the project.
print_status "success" "Running frontend linter (ESLint)..."
if ! cd frontend && npm run lint; then
    print_status "error" "Frontend linting issues found. Please fix ESLint errors."
    exit 1
fi
cd ..

# Run frontend unit tests (Vitest)
print_status "success" "Running frontend tests..."
if ! cd frontend && npm test; then
    print_status "error" "Frontend tests failed. Please fix the failing tests."
    exit 1
fi
cd ..

# Run tests
print_status "success" "Running tests..."
if ! cd src-tauri && cargo test --workspace; then
    print_status "error" "Tests failed. Please fix test issues."
    exit 1
fi
cd ..

# Check for security vulnerabilities (Rust)
print_status "success" "Checking for Rust security vulnerabilities..."
if ! command_exists cargo-audit; then
    print_status "warning" "cargo-audit not found. Installing..."
    if cargo install cargo-audit; then
        print_status "success" "cargo-audit installed successfully"
        if ! cd src-tauri && cargo audit; then
            print_status "warning" "Security vulnerabilities found in Rust dependencies."
        fi
        cd ..
    else
        print_status "error" "Failed to install cargo-audit"
        exit 1
    fi
else
    if ! cd src-tauri && cargo audit; then
        print_status "warning" "Security vulnerabilities found in Rust dependencies."
    fi
    cd ..
fi

# Check for security vulnerabilities (Node.js)
print_status "success" "Checking for Node.js security vulnerabilities..."
if ! command_exists npx; then
    print_status "error" "npx not found. Please install Node.js: https://nodejs.org/"
    exit 1
fi

# Ensure audit-ci is installed
if ! cd frontend && npx audit-ci --config audit-ci.json; then
    print_status "warning" "Security vulnerabilities found in Node.js dependencies or audit-ci.json not found."
fi
cd .. || true

# Build the project
print_status "success" "Building the project..."
if ! cd src-tauri && cargo build --workspace --verbose; then
    print_status "error" "Build failed. Please fix build issues."
    exit 1
fi
cd ..

# Build the frontend
print_status "success" "Building the frontend..."
if ! cd frontend && npm run build; then
    print_status "error" "Frontend build failed. Please fix build issues."
    exit 1
fi
cd ..

print_status "success" "All health checks passed!"
exit 0