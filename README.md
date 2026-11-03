# Rustfus

Rustfus is an independent, Apache-2.0 licensed Windows application for creating bootable USB media. It does not contain code, branding, boot records, or assets from Rufus.

## v0.1 target

- Microsoft Windows 10 22H2 and Windows 11 x64 installation media
- x86_64 Ubuntu installation media, including optional zero-touch `NoCloud` autoinstall for UEFI systems
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

The interface supports ISO selection by dialog or drag and drop, the explicit erase confirmation, a per-session log in `%LOCALAPPDATA%\Rustfus\logs` (files older than 30 days are removed) with an **Export log** button, and worker progress (phase, percentage, throughput) with cancellation. After a Windows ISO is recognized, the **Windows unattended setup** panel opens Rustfus' own full visual editor. Its tabs cover architecture, Setup and installed-system locales, time zone, machine identity, local administrator credentials and auto-logon, supported OOBE choices, product key, hardware-compatibility overrides, optional destructive UEFI/GPT disk automation, diagnostic-data and system policies, and commands for the `windowsPE`, `specialize`, and first-logon stages. Fields with a finite valid set use non-editable selection lists, while genuinely free-form values receive format and length validation. The final tab shows a read-only `autounattend.xml` preview; Rustfus validates it locally and writes it at the USB root without opening or depending on an external generator.

Linux images never receive Windows configuration. Generated Windows XML is validated at the UI/worker boundary, written to the USB root, and participates in read-back verification when full verification is enabled.

Recognized Ubuntu media has its own **Ubuntu zero-touch autoinstall** panel. Its full visual editor exposes identity and locally hashed account credentials, locale, keyboard, time zone, Netplan, curtin storage, APT, Ubuntu Pro, SSH, packages, snaps, drivers, codecs, updates, OEM packages, installer refresh/source, kernel, crash dumps, Active Directory, interactive sections, reporting, target cloud-init data, debconf, IBM Z devices, and early/late/error commands. Open-schema sections use focused YAML editors, and the final generated `user-data` remains editable so every current or future Subiquity key is available without waiting for a Rustfus release. Complete existing `user-data` and optional `meta-data` files can still be imported instead.

Rustfus adds the resulting configuration as a `NoCloud` seed, patches the GRUB/ISOLINUX kernel arguments, and updates the ISO checksum manifest. This flow follows the layout used by [ubuntu-autoinstall-generator](https://github.com/covertsh/ubuntu-autoinstall-generator). Because Rustfus rebuilds the target as FAT32 without bundling third-party ISO mastering tools, autoinstall output is UEFI-only and is disabled when a source file exceeds FAT32's 4 GiB per-file limit. The same recognized Ubuntu ISO can still be written byte-for-byte and retain its original boot modes with autoinstall disabled.

`Rustfus.exe --self-test` checks the installation without opening a window; `Rustfus.exe --demo` runs with fake data and never touches a disk. Passwords entered for unattended setup are necessarily stored in plain text in `autounattend.xml` or Ubuntu `user-data` on the removable media; the UI calls this out before the operation. Windows configurations can contain commands that run as SYSTEM or at first logon, so review the generated XML before writing the USB.

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

GitHub Actions does the same on every push (`ci.yml` calls `package.yml`), and every push to `main` that passes CI is published as a timestamped pre-release such as `20261102.014500` (`release.yml`; see `RELEASE.md`).

Rust code can also be built and checked from Ubuntu on WSL with the MSVC target and `cargo-xwin`:

```sh
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin --locked
./scripts/build-wsl.sh
```

The static C runtime is set in `.cargo/config.toml`; do not set `RUSTFLAGS`, which would override it. The app targets Windows 10 22H2 and Windows 11 x64; UAC is requested only when an operation is started.
