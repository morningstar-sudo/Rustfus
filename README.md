# Rustfus

Rustfus is an independent, Apache-2.0 licensed Windows application for creating bootable USB media. It does not contain code, branding, boot records, or assets from Rufus.

## v0.1 target

- Microsoft Windows 10 22H2 and Windows 11 x64 installation media
- x86_64 hybrid Linux ISO images
- USB targets only, with system-disk exclusion and device revalidation
- Full read-back verification enabled by default
- A WPF user interface and a separately elevated worker

The destructive worker is deliberately fail-closed: an operation must be planned from a fresh device snapshot, confirmed by the user (a Yes/No prompt whose default is No), and revalidated immediately before opening a physical drive for writes.

## Layout

| Part | What it is |
|---|---|
| `apps/rustfus-gui` | The WPF front end (C#, no NuGet; built by `scripts/build-gui.ps1` with the compiler that ships with Windows). The window is `MainWindow.xaml`, embedded in `Rustfus.exe`. |
| `crates/rustfus-ffi` | `rustfus_ffi.dll`: the C ABI the front end calls (USB discovery, image analysis, and the authenticated session with the worker). |
| `apps/rustfus-worker` | The elevated worker, started with UAC only when an operation begins. |
| `crates/rustfus-core`, `-protocol`, `-image`, `-disk`, `-windows` | Shared types, IPC framing, image analysis, MBR/FAT32 layout, and the Windows storage layer. |

The release is three files (`Rustfus.exe`, `rustfus_ffi.dll`, `rustfus-worker.exe`) plus license files: about 1 MB zipped. The front end needs .NET Framework 4.8, which is part of Windows 10 22H2 and Windows 11; the Rust parts link the C runtime statically.

## Current implementation

The front end runs without elevation. It filters USB targets, analyses the image, and, when you confirm, starts an ephemeral elevated worker over an authenticated named pipe. The worker re-enumerates and fingerprints the selected disk immediately before the destructive phase, locks every target volume before dismounting any of them, aligns raw writes to the reported physical sector size, and requires a durable device flush before raw read-back verification. Windows file copies share one streaming buffer and cross a volume-level durability boundary before tree verification. Cancellation is propagated to the worker and honoured at safe boundaries.

Device identity is the Windows PnP instance ID resolved through SetupAPI; page-file and crash-dump locations are independently mapped from the Windows registry to physical disk extents before eligible targets are shown. The disks holding the running Windows installation are identified with native Win32 queries only (the `%SystemDrive%` volume extents, plus the system partition from both `HKLM\SYSTEM\Setup\SystemPartition` and `NtQuerySystemInformation`); discovery stops rather than guessing if any of that cannot be established or if Windows is running from a native-boot VHD.

Windows media analysis validates the required setup layout and requires the embedded `bootsect.exe` Authenticode signer organization to be Microsoft Corporation. Raw writes of hybrid images and read-back verification run as a two-thread pipeline, and image analysis uses all cores. Stale WIM split workspaces under `%TEMP%\Rustfus` are removed after seven days.

The interface supports ISO selection by dialog or drag and drop, the explicit erase confirmation, a per-session log in `%LOCALAPPDATA%\Rustfus\logs` (files older than 30 days are removed) with an **Export log** button, and worker progress (phase, percentage, throughput) with cancellation. `Rustfus.exe --self-test` checks the installation without opening a window; `Rustfus.exe --demo` runs with fake data and never touches a disk.

This is a release candidate. Protocol, disk-layout, image-analysis, fault-injection, Authenticode, and Windows-native tests are automated. The stable v0.1 tag remains gated on the physical-device, boot, and accessibility matrix in `RELEASE.md`. Do not use candidate builds on media containing data you need.

## Test suites

The portable suite covers planning, protocol framing, FAT32/MBR bytes (through a target that rejects partial-sector transfers, like a raw disk), image classification, the parallel fingerprint, verified copies, the write pipeline, cancellation, short writes, corrupt read-back, IPC authentication, and stale temporary-directory cleanup:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```

Windows-native tests additionally exercise Authenticode publisher validation, native system-disk detection, and ISO mounting. Two ignored integration tests create and attach their own dynamically expanding VHDX, obtain the physical disk number only from that VHD handle, and never open a real disk: one partitions, formats, writes a probe file and runs `chkdsk`; the other writes a raw image through the pipeline and reads it back. They must run in an elevated disposable environment:

```powershell
.\scripts\test-vhdx.ps1
```

The manually dispatched `hardware.yml` workflow targets a dedicated elevated self-hosted runner labelled `rustfus-disposable`.

## Build

On Windows with a Rust toolchain:

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc -p rustfus-ffi -p rustfus-worker
.\scripts\package.ps1
```

`package.ps1` builds the front end (`scripts\build-gui.ps1`), assembles `dist\Rustfus-<version>-windows-x64`, writes `THIRD-PARTY-NOTICES.txt`, and creates the ZIP and its SHA-256 sidecar. `scripts\smoke-test.ps1 -Archive <zip>` then checks the package the way a user receives it. The artifacts are unsigned.

GitHub Actions does the same on every push (`ci.yml` calls `package.yml`) and publishes a release for a `v<version>` tag (`release.yml`).

Rust code can also be built and checked from Ubuntu on WSL with the MSVC target and `cargo-xwin`:

```sh
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin --locked
./scripts/build-wsl.sh
```

The static C runtime is set in `.cargo/config.toml`; do not set `RUSTFLAGS`, which would override it. The app targets Windows 10 22H2 and Windows 11 x64; UAC is requested only when an operation is started.
