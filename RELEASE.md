# Release process

Rustfus releases are built from an annotated `v<version>` tag. The tag version must exactly match `[workspace.package].version` in `Cargo.toml`; `release.yml` checks this before it spends any build time.

## Automated gates

`release.yml` runs the same workflow as every push (`ci.yml`) and publishes only if all of it passes:

1. Formatting and `cargo-deny`.
2. Clippy with `-D warnings` and the workspace tests, on Linux and natively on Windows (so `cfg(windows)` code is linted and tested).
3. The x64 release build of `rustfus-ffi` and `rustfus-worker` with `--locked`, the front end build (`scripts/build-gui.ps1`, warnings are errors), and the ZIP with its SHA-256 sidecar and `THIRD-PARTY-NOTICES.txt` (`scripts/package.ps1`).
4. The package smoke test (`scripts/smoke-test.ps1`): the checksum matches, the package is complete, no binary imports the Visual C++ runtime, `Rustfus.exe --self-test` exits 0 (Rust library loads, USB scan runs, worker is present), and the window stays up in `--demo` mode.
5. The ZIP and its SHA-256 sidecar are attached to the GitHub release.

## Manual acceptance gates

Before tagging a stable release, record successful results for:

- The elevated disposable VHDX tests in `scripts/test-vhdx.ps1` (FAT32 pipeline with `chkdsk`, and raw write with read-back).
- Windows 10 22H2 x64 boot in UEFI Secure Boot and Legacy BIOS modes.
- Windows 11 x64 boot in UEFI Secure Boot mode.
- Current Ubuntu, Fedora, and Debian x86_64 hybrid images in the boot modes supported by each image.
- Windows media with `install.wim` below and above the FAT32 file-size limit.
- Twenty write/verify cycles on disposable media and five distinct physical USB devices.
- USB removal, cancellation, lock failure, short-write, and corrupt-readback cases.
- Dragging an ISO onto the window, the Yes/No erase confirmation (default No), and **Export log**.
- Keyboard navigation, screen-reader labels, and high-contrast mode.
- Start-up on Windows 10 22H2 and Windows 11 machines that have no Visual C++ Redistributable and no Rust toolchain installed.

Never run the physical-device matrix on media containing user data. Release binaries are unsigned unless the release operator signs `Rustfus.exe`, `rustfus_ffi.dll`, and `rustfus-worker.exe` before packaging; Windows SmartScreen warns about unsigned downloads.
