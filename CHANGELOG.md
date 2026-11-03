# Changelog

All notable changes to Rustfus are documented here.

## 0.1.0 - Unreleased

### Added

- Ubuntu zero-touch autoinstall for recognized x86_64 Ubuntu ISOs. A full multi-tab GUI covers every current Subiquity section, hashes account passwords locally as salted SHA-512 crypt, provides structured YAML editors for open-schema options, and keeps the complete generated YAML editable; importing existing `user-data` and optional `meta-data` remains available. The worker creates a `NoCloud` seed, patches GRUB/ISOLINUX boot arguments, refreshes affected `md5sum.txt` entries, flushes the FAT32 volume, and includes every generated file in optional read-back verification. Automated Ubuntu media boots via UEFI; disabling automation preserves the original ISO through the raw-write path.
- A native Windows unattended-setup editor with tabs for regional settings, machine identity, accounts and OOBE, product key, compatibility overrides, optional UEFI/GPT disk automation, privacy/system policies, commands, and a read-only XML preview. Finite option sets use non-editable lists and free-form values are validated. It generates and validates `autounattend.xml` entirely inside Rustfus, writes it at the USB root, and includes it in durable read-back verification.
- A WPF front end (C#, .NET Framework 4.8 as shipped with Windows 10 22H2 and Windows 11, compiled with the C# compiler that ships with Windows) with Mica and dark mode, ISO drag and drop, a Yes/No erase confirmation whose default answer is No, a per-session log under `%LOCALAPPDATA%\Rustfus\logs` with log export, and `--self-test` / `--demo` switches.
- `rustfus_ffi.dll`: the C ABI between the front end and the Rust core (USB discovery, image analysis, and the authenticated session with the elevated worker).
- An ephemeral elevated worker, authenticated and versioned named-pipe IPC with plan revalidation, and cancellation honoured at safe boundaries.
- USB-only discovery using SetupAPI and native Win32 boot/system-disk detection (no WMI); discovery stops when the system disks cannot be identified or Windows runs from a native-boot VHD.
- Raw write of x86_64 hybrid Linux ISO images with full read-back verification.
- Windows installer media creation using MBR, FAT32, trusted `bootsect.exe`, and WIMGAPI splitting.
- Fault-injection tests, disposable-VHDX acceptance tests (FAT32 pipeline, and raw write plus read-back on a real block device), and a portable ZIP (about 1 MB) with a SHA-256 sidecar and `THIRD-PARTY-NOTICES.txt`, built and smoke-tested by GitHub Actions. Every push to `main` that passes CI is published automatically as a pre-release tagged with a UTC timestamp (`v<version>-<yyyyMMdd>.<HHmmss>`).

### Changed

- Release ZIPs no longer include repository Markdown files; legal notices remain as `LICENSE` and `THIRD-PARTY-NOTICES.txt`.
- The C runtime is linked statically, so the package runs on machines without the Visual C++ Redistributable.
- Image analysis is multi-core: the image fingerprint is a SHA-256 tree hash over 4 MiB chunks (so it is no longer the plain SHA-256 of the ISO), and hybrid images are read once for both the fingerprint and the EFI boot-loader check. Analysing a 4 GB hybrid ISO went from about 9 s to about 1.4 s.
- Raw image writes overlap reading and hashing the source with writing the target, and hashing with reading during read-back verification (a bounded two-thread pipeline; blocks, progress and errors stay in image order). Measured against throttled targets: about 5% faster at 100 MB/s, 18% at 400 MB/s and 42% at 1500 MB/s.
- Raw verification waits for a successful device flush, volume enumeration fails closed on incomplete Win32 results, Windows media verification crosses a volume-level durability boundary, and file copies share one streaming buffer.
- Dependency diet: WMI, `tracing` and the WinUI runtime are gone. The worker links 35 instead of 52 crates and shrank from 1.7 MB to about 1 MB; the lock file has 58 instead of 108 packages.
- Build profile: Thin LTO with release overflow checks; dev builds keep line tables only.

### Fixed

- Windows ISO analysis: ISOs attach as CD-ROM devices (not physical drives) and need `GET_INFO` access to resolve, so they now mount, are recognised by `bootmgr` / `sources\boot.wim`, and report their real error instead of falling back to the hybrid-image message.
- The FAT32 formatter wrote the 32-byte volume-label entry straight to the disk. A raw disk only accepts whole-sector transfers, so formatting a real (or virtual) disk failed with `ERROR_INVALID_PARAMETER`. It now writes the whole first sector of the root directory, and unit tests use a target that rejects partial-sector transfers like a raw disk does.
- Raw-image read-back verification read only the image bytes of the last block, which fails on a raw disk when the image length is not a multiple of the sector size. It now reads the zero padding too and hashes only the image bytes.
- Volume listing for a target disk failed when the machine had volumes without physical disk extents (optical drives, network or RAM volumes).
