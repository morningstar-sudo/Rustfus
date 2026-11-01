# Release process

There is no manual tagging step. Every push to `main` that passes CI is published automatically as a **pre-release** whose tag is `v<workspace version>-<UTC date>.<UTC time>` (for example `v0.1.0-20261102.014500`), created at the exact commit CI tested. The ZIP is named `Rustfus-<version>-<date>.<time>-windows-x64.zip` and carries a `.sha256` sidecar. Pushes that only change Markdown files do not run CI and therefore do not release.

## Automated gates

`ci.yml` runs on every push and pull request; `release.yml` runs only after a successful CI run for a push to `main` and publishes the artifact that run built. Nothing is rebuilt at release time.

1. Formatting and `cargo-deny`.
2. Clippy with `-D warnings` and the workspace tests, on Linux and natively on Windows (so `cfg(windows)` code is linted and tested).
3. The x64 release build of `rustfus-ffi` and `rustfus-worker` with `--locked`, the front end build (`scripts/build-gui.ps1`, warnings are errors), and the ZIP with its SHA-256 sidecar and `THIRD-PARTY-NOTICES.txt` (`scripts/package.ps1`).
4. The package smoke test (`scripts/smoke-test.ps1`): the checksum matches, the package is complete, no binary imports the Visual C++ runtime, `Rustfus.exe --self-test` exits 0 (Rust library loads, USB scan runs, worker is present), and the window stays up in `--demo` mode.
5. At release time the checksum is verified again, the files are renamed with the timestamp (the ZIP itself is not touched), and the tag, the release and its generated notes are created. Re-running the job for the same CI run finds the existing release and does nothing.

Old pre-releases are never deleted automatically; remove ones you no longer want from the Releases page.

## Making a stable release

Automated releases stay pre-releases because the manual acceptance gates below are not part of CI. When they have all been recorded, publish a stable version yourself: bump `[workspace.package].version`, merge, then edit the release that CI created for that commit and clear **Set as a pre-release** (or create a release by hand from that tag).

## Manual acceptance gates

Before calling a build stable, record successful results for:

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
