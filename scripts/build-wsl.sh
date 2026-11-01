#!/usr/bin/env bash
# Checks the Rust code and cross-builds the two Windows binaries from Ubuntu on WSL. The WPF front end
# (C#) and the ZIP are built on Windows with scripts/build-gui.ps1 and scripts/package.ps1.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

source "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --target x86_64-pc-windows-msvc --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo xwin build --release --locked --target x86_64-pc-windows-msvc -p rustfus-ffi -p rustfus-worker
