//! Image fingerprinting and media classification independent of privileged I/O.

use rustfus_core::{
    Architecture, ErrorCode, Fingerprint, ImageAnalysis, ImageKind, OperationError,
};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

mod tree_hash;
pub use tree_hash::{ImageScan, fingerprint_image_file};

pub const FAT32_MAX_FILE_BYTES: u64 = u32::MAX as u64;
pub const WIM_SPLIT_THRESHOLD_BYTES: u64 = 3_800 * 1024 * 1024;
pub const STREAM_BUFFER_BYTES: usize = 4 * 1024 * 1024;
const ISO_PRIMARY_VOLUME_DESCRIPTOR_OFFSET: u64 = 16 * 2048;

pub fn fingerprint_file(path: &Path) -> Result<Fingerprint, OperationError> {
    fingerprint_file_with_progress(path, |_| Ok(()))
}

pub fn fingerprint_file_with_progress(
    path: &Path,
    on_bytes: impl FnMut(u64) -> Result<(), OperationError>,
) -> Result<Fingerprint, OperationError> {
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    fingerprint_file_with_buffer(path, &mut buffer, on_bytes)
}

pub fn fingerprint_file_with_buffer(
    path: &Path,
    buffer: &mut [u8],
    mut on_bytes: impl FnMut(u64) -> Result<(), OperationError>,
) -> Result<Fingerprint, OperationError> {
    if buffer.is_empty() {
        return Err(OperationError::new(
            ErrorCode::Internal,
            "The fingerprint buffer must not be empty",
        ));
    }
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    loop {
        let count = file.read(buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        on_bytes(count as u64)?;
    }
    Ok(Fingerprint(hasher.finalize().into()))
}

/// Copies a regular file while hashing the source, then optionally performs a
/// durable flush and hashes the destination again for independent read-back
/// verification. Callers that set `verify` to false must provide their own
/// durability boundary before treating a batch as complete.
pub fn copy_file_verified(
    source: &Path,
    destination: &Path,
    verify: bool,
    on_bytes: impl FnMut(u64),
) -> Result<[u8; 32], OperationError> {
    let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    copy_file_verified_with_buffer(source, destination, verify, &mut buffer, on_bytes)
}

pub fn copy_file_verified_with_buffer(
    source: &Path,
    destination: &Path,
    verify: bool,
    buffer: &mut [u8],
    mut on_bytes: impl FnMut(u64),
) -> Result<[u8; 32], OperationError> {
    if buffer.is_empty() {
        return Err(OperationError::new(
            ErrorCode::Internal,
            "The copy buffer must not be empty",
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let mut input = File::open(source).map_err(io_error)?;
    let mut output = File::create(destination).map_err(io_error)?;
    let mut hasher = Sha256::new();
    loop {
        let count = input.read(buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(io_error)?;
        hasher.update(&buffer[..count]);
        on_bytes(count as u64);
    }
    output.flush().map_err(io_error)?;
    let expected: [u8; 32] = hasher.finalize().into();

    if verify {
        output.sync_all().map_err(io_error)?;
        drop(output);
        let actual = fingerprint_file_with_buffer(destination, buffer, |_| Ok(()))?.0;
        if actual != expected {
            return Err(OperationError::new(
                ErrorCode::VerificationFailed,
                format!(
                    "Read-back verification failed for {}",
                    destination.display()
                ),
            ));
        }
    }
    Ok(expected)
}

pub fn analyze_hybrid_iso(path: &Path) -> Result<ImageAnalysis, OperationError> {
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() {
        return Err(OperationError::new(
            ErrorCode::InvalidImage,
            "The selected image is not a regular file",
        ));
    }
    let mut file = File::open(path).map_err(io_error)?;
    probe_hybrid_iso(&mut file, metadata.len())?;
    drop(file);
    let scan = fingerprint_image_file(path, true)?;
    if !scan.has_x64_boot {
        let message = if scan.has_arm64_boot {
            "The hybrid ISO contains ARM64 boot media; v0.1 accepts only x86_64 images"
        } else {
            "The hybrid ISO does not expose an x86_64 EFI boot image"
        };
        return Err(OperationError::new(
            if scan.has_arm64_boot {
                ErrorCode::UnsupportedArchitecture
            } else {
                ErrorCode::InvalidImage
            },
            message,
        ));
    }
    let fingerprint = scan.fingerprint;
    Ok(ImageAnalysis {
        path: path.to_path_buf(),
        kind: ImageKind::HybridDisk,
        architecture: Architecture::X86_64,
        size_bytes: metadata.len(),
        fingerprint,
        required_bytes: metadata.len(),
        temporary_bytes: 0,
        warnings: vec![
            "The target will inherit the partition layout embedded in the hybrid image".into(),
        ],
    })
}

pub fn probe_hybrid_iso<R: Read + Seek>(reader: &mut R, length: u64) -> Result<(), OperationError> {
    if length < ISO_PRIMARY_VOLUME_DESCRIPTOR_OFFSET + 2048 {
        return Err(invalid_image("The file is too small to be an ISO image"));
    }

    let mut mbr = [0_u8; 512];
    reader.seek(SeekFrom::Start(0)).map_err(io_error)?;
    reader.read_exact(&mut mbr).map_err(io_error)?;
    if mbr[510..512] != [0x55, 0xAA] {
        return Err(invalid_image("The ISO does not contain an MBR signature"));
    }
    let has_partition = (0..4).any(|index| {
        let offset = 446 + index * 16;
        let partition_type = mbr[offset + 4];
        let sectors = u32::from_le_bytes(mbr[offset + 12..offset + 16].try_into().unwrap());
        partition_type != 0 && sectors != 0
    });
    if !has_partition {
        return Err(invalid_image("The ISO has no hybrid MBR partition entry"));
    }

    reader
        .seek(SeekFrom::Start(ISO_PRIMARY_VOLUME_DESCRIPTOR_OFFSET))
        .map_err(io_error)?;
    let mut descriptor = [0_u8; 7];
    reader.read_exact(&mut descriptor).map_err(io_error)?;
    if descriptor[0] != 1 || &descriptor[1..6] != b"CD001" || descriptor[6] != 1 {
        return Err(invalid_image(
            "The ISO9660 primary volume descriptor is missing",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsMediaManifest {
    pub analysis: ImageAnalysis,
    pub root: PathBuf,
    pub bootsect_path: PathBuf,
    pub install_image: InstallImage,
    pub files: Vec<MediaFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UbuntuMediaManifest {
    pub analysis: ImageAnalysis,
    pub root: PathBuf,
    pub files: Vec<MediaFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallImage {
    Wim { path: PathBuf, split_required: bool },
    Esd { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFile {
    pub relative_path: PathBuf,
    pub size_bytes: u64,
}

pub fn analyze_mounted_windows_iso(
    iso_path: &Path,
    mounted_root: &Path,
) -> Result<WindowsMediaManifest, OperationError> {
    let bootmgr = required_child(mounted_root, &["bootmgr"])?;
    let bootx64 = required_child(mounted_root, &["efi", "boot", "bootx64.efi"])?;
    let boot_wim = required_child(mounted_root, &["sources", "boot.wim"])?;
    let bootsect_path = required_child(mounted_root, &["boot", "bootsect.exe"])?;
    for path in [&bootmgr, &bootx64, &boot_wim, &bootsect_path] {
        if !path.is_file() {
            return Err(invalid_image(
                "A required Windows Setup file is not a regular file",
            ));
        }
    }

    let install_wim = child_case_insensitive(mounted_root, &["sources", "install.wim"]);
    let install_esd = child_case_insensitive(mounted_root, &["sources", "install.esd"]);
    let install_image = match (install_wim, install_esd) {
        (Some(path), _) => {
            let size = fs::metadata(&path).map_err(io_error)?.len();
            InstallImage::Wim {
                path,
                split_required: size > WIM_SPLIT_THRESHOLD_BYTES,
            }
        }
        (None, Some(path)) => {
            let size = fs::metadata(&path).map_err(io_error)?.len();
            if size > FAT32_MAX_FILE_BYTES {
                return Err(OperationError::new(
                    ErrorCode::UnsupportedLargeFile,
                    "install.esd exceeds the FAT32 file-size limit and cannot be split in v0.1",
                ));
            }
            InstallImage::Esd { path }
        }
        (None, None) => {
            return Err(invalid_image(
                "The ISO contains neither sources/install.wim nor sources/install.esd",
            ));
        }
    };

    let canonical_root = mounted_root.canonicalize().map_err(io_error)?;
    let mut files = Vec::new();
    let mut required_bytes = 0_u64;
    for entry in WalkDir::new(&canonical_root).follow_links(false) {
        let entry = entry.map_err(|error| {
            OperationError::new(ErrorCode::Io, format!("Could not enumerate ISO: {error}"))
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = entry.metadata().map_err(|error| {
            OperationError::new(
                ErrorCode::Io,
                format!("Could not inspect ISO file: {error}"),
            )
        })?;
        let relative_path = entry
            .path()
            .strip_prefix(&canonical_root)
            .map_err(|_| invalid_image("A mounted ISO path escaped its root"))?
            .to_path_buf();
        let is_split_wim = matches!(
            &install_image,
            InstallImage::Wim {
                path,
                split_required: true
            } if same_file_path(entry.path(), path)
        );
        if metadata.len() > FAT32_MAX_FILE_BYTES && !is_split_wim {
            return Err(OperationError::new(
                ErrorCode::UnsupportedLargeFile,
                format!(
                    "{} exceeds the FAT32 file-size limit",
                    relative_path.display()
                ),
            ));
        }
        required_bytes = required_bytes.saturating_add(metadata.len());
        files.push(MediaFile {
            relative_path,
            size_bytes: metadata.len(),
        });
    }

    let iso_metadata = fs::metadata(iso_path).map_err(io_error)?;
    let temporary_bytes = match &install_image {
        InstallImage::Wim {
            path,
            split_required: true,
        } => fs::metadata(path).map_err(io_error)?.len() + 512 * 1024 * 1024,
        _ => 0,
    };
    let analysis = ImageAnalysis {
        path: iso_path.to_path_buf(),
        kind: ImageKind::WindowsInstaller,
        architecture: Architecture::X86_64,
        size_bytes: iso_metadata.len(),
        fingerprint: fingerprint_image_file(iso_path, false)?.fingerprint,
        required_bytes: required_bytes.saturating_add(64 * 1024 * 1024),
        temporary_bytes,
        warnings: vec![
            "Standard media layout and signed boot tools are checked; whole-ISO provenance is not asserted"
                .into(),
        ],
    };

    Ok(WindowsMediaManifest {
        analysis,
        root: canonical_root,
        bootsect_path,
        install_image,
        files,
    })
}

pub fn analyze_mounted_ubuntu_iso(
    iso_path: &Path,
    mounted_root: &Path,
) -> Result<UbuntuMediaManifest, OperationError> {
    let info_path = required_child(mounted_root, &[".disk", "info"])?;
    let bootx64 = required_child(mounted_root, &["efi", "boot", "bootx64.efi"])?;
    let kernel = required_child(mounted_root, &["casper", "vmlinuz"])?;
    let grub = required_child(mounted_root, &["boot", "grub", "grub.cfg"])?;
    for path in [&info_path, &bootx64, &kernel, &grub] {
        if !path.is_file() {
            return Err(invalid_image(
                "A required Ubuntu installation file is not a regular file",
            ));
        }
    }
    let info = fs::read_to_string(&info_path).map_err(io_error)?;
    if !info.to_ascii_lowercase().contains("ubuntu") {
        return Err(invalid_image(
            "The mounted image does not identify itself as Ubuntu installation media",
        ));
    }

    let canonical_root = mounted_root.canonicalize().map_err(io_error)?;
    let mut files = Vec::new();
    let mut required_bytes = 0_u64;
    let mut oversized_file = None;
    for entry in WalkDir::new(&canonical_root).follow_links(false) {
        let entry = entry.map_err(|error| {
            OperationError::new(ErrorCode::Io, format!("Could not enumerate ISO: {error}"))
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = entry.metadata().map_err(|error| {
            OperationError::new(
                ErrorCode::Io,
                format!("Could not inspect ISO file: {error}"),
            )
        })?;
        let relative_path = entry
            .path()
            .strip_prefix(&canonical_root)
            .map_err(|_| invalid_image("A mounted ISO path escaped its root"))?
            .to_path_buf();
        if metadata.len() > FAT32_MAX_FILE_BYTES {
            oversized_file.get_or_insert_with(|| relative_path.clone());
        }
        required_bytes = required_bytes.saturating_add(metadata.len());
        files.push(MediaFile {
            relative_path,
            size_bytes: metadata.len(),
        });
    }

    let iso_metadata = fs::metadata(iso_path).map_err(io_error)?;
    let mut warnings = vec![
        "Ubuntu autoinstall media is rebuilt as a UEFI-bootable FAT32 volume; legacy BIOS boot is not available"
            .into(),
    ];
    if let Some(path) = oversized_file {
        warnings.push(format!(
            "Ubuntu autoinstall is unavailable because {} exceeds FAT32's 4 GiB per-file limit; raw image writing remains available",
            path.display()
        ));
        required_bytes = 0;
    } else {
        required_bytes = required_bytes.saturating_add(64 * 1024 * 1024);
    }
    let analysis = ImageAnalysis {
        path: iso_path.to_path_buf(),
        kind: ImageKind::UbuntuInstaller,
        architecture: Architecture::X86_64,
        size_bytes: iso_metadata.len(),
        fingerprint: fingerprint_image_file(iso_path, false)?.fingerprint,
        required_bytes,
        temporary_bytes: 0,
        warnings,
    };
    Ok(UbuntuMediaManifest {
        analysis,
        root: canonical_root,
        files,
    })
}

fn required_child(root: &Path, components: &[&str]) -> Result<PathBuf, OperationError> {
    child_case_insensitive(root, components).ok_or_else(|| {
        invalid_image(&format!(
            "Required ISO path is missing: {}",
            components.join("/")
        ))
    })
}

fn child_case_insensitive(root: &Path, components: &[&str]) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for component in components {
        let entries = fs::read_dir(&current).ok()?;
        let entry = entries.filter_map(Result::ok).find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(component)
        })?;
        current = entry.path();
    }
    Some(current)
}

fn same_file_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().ok() == right.canonicalize().ok()
}

fn invalid_image(message: &str) -> OperationError {
    OperationError::new(ErrorCode::InvalidImage, message)
}

fn io_error(error: io::Error) -> OperationError {
    OperationError::new(ErrorCode::Io, error.to_string())
        .with_platform_code(error.raw_os_error().map_or(0, |code| code as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn hybrid_probe_requires_iso_descriptor_and_partition() {
        let mut image = vec![0_u8; 17 * 2048];
        image[510..512].copy_from_slice(&[0x55, 0xAA]);
        image[446 + 4] = 0x17;
        image[446 + 12..446 + 16].copy_from_slice(&100_u32.to_le_bytes());
        let descriptor = 16 * 2048;
        image[descriptor] = 1;
        image[descriptor + 1..descriptor + 6].copy_from_slice(b"CD001");
        image[descriptor + 6] = 1;
        let length = image.len() as u64;
        assert!(probe_hybrid_iso(&mut Cursor::new(image), length).is_ok());
    }

    #[test]
    fn ordinary_iso_without_hybrid_partition_is_rejected() {
        let mut image = vec![0_u8; 17 * 2048];
        image[510..512].copy_from_slice(&[0x55, 0xAA]);
        let descriptor = 16 * 2048;
        image[descriptor] = 1;
        image[descriptor + 1..descriptor + 6].copy_from_slice(b"CD001");
        image[descriptor + 6] = 1;
        let length = image.len() as u64;
        assert_eq!(
            probe_hybrid_iso(&mut Cursor::new(image), length)
                .unwrap_err()
                .code,
            ErrorCode::InvalidImage
        );
    }

    /// A minimal hybrid ISO that passes the structural probe, with `payload` appended.
    fn hybrid_iso_with(payload: &[u8]) -> Vec<u8> {
        let mut image = vec![0_u8; 17 * 2048];
        image[510..512].copy_from_slice(&[0x55, 0xAA]);
        image[446 + 4] = 0x17;
        image[446 + 12..446 + 16].copy_from_slice(&100_u32.to_le_bytes());
        let descriptor = 16 * 2048;
        image[descriptor] = 1;
        image[descriptor + 1..descriptor + 6].copy_from_slice(b"CD001");
        image[descriptor + 6] = 1;
        image.extend_from_slice(payload);
        image
    }

    fn analyze_bytes(bytes: &[u8]) -> Result<ImageAnalysis, OperationError> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.iso");
        fs::write(&path, bytes).unwrap();
        analyze_hybrid_iso(&path)
    }

    #[test]
    fn hybrid_architecture_probe_rejects_arm64() {
        let image = hybrid_iso_with(b"efi/boot/bootaa64.efi;1");
        assert_eq!(
            analyze_bytes(&image).unwrap_err().code,
            ErrorCode::UnsupportedArchitecture
        );
    }

    #[test]
    fn hybrid_without_any_efi_loader_is_invalid() {
        let image = hybrid_iso_with(b"nothing to boot here");
        assert_eq!(
            analyze_bytes(&image).unwrap_err().code,
            ErrorCode::InvalidImage
        );
    }

    #[test]
    fn hybrid_architecture_marker_can_cross_a_read_boundary() {
        // Put the marker so that it straddles the 4 MiB chunk boundary of the image.
        let mut image = hybrid_iso_with(&[]);
        let boundary = 4 * 1024 * 1024;
        image.resize(boundary - 4, b'x');
        image.extend_from_slice(b"bootx64.efi;1");
        let analysis = analyze_bytes(&image).unwrap();
        assert_eq!(analysis.kind, ImageKind::HybridDisk);
        assert_eq!(analysis.size_bytes, image.len() as u64);
    }

    #[test]
    fn analysis_fingerprint_changes_with_the_image_and_is_repeatable() {
        let first = analyze_bytes(&hybrid_iso_with(b"BOOTX64.EFI a")).unwrap();
        let again = analyze_bytes(&hybrid_iso_with(b"BOOTX64.EFI a")).unwrap();
        let other = analyze_bytes(&hybrid_iso_with(b"BOOTX64.EFI b")).unwrap();
        assert_eq!(first.fingerprint, again.fingerprint);
        assert_ne!(first.fingerprint, other.fingerprint);
    }

    #[test]
    fn mounted_windows_layout_is_detected() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("mounted");
        fs::create_dir_all(root.join("efi/boot")).unwrap();
        fs::create_dir_all(root.join("sources")).unwrap();
        fs::create_dir_all(root.join("boot")).unwrap();
        fs::write(root.join("bootmgr"), b"boot").unwrap();
        fs::write(root.join("efi/boot/bootx64.efi"), b"efi").unwrap();
        fs::write(root.join("sources/boot.wim"), b"wim").unwrap();
        fs::write(root.join("sources/install.esd"), b"esd").unwrap();
        fs::write(root.join("boot/bootsect.exe"), b"exe").unwrap();
        let iso = temp.path().join("windows.iso");
        fs::write(&iso, b"iso fingerprint").unwrap();

        let manifest = analyze_mounted_windows_iso(&iso, &root).unwrap();
        assert_eq!(manifest.analysis.kind, ImageKind::WindowsInstaller);
        assert!(matches!(manifest.install_image, InstallImage::Esd { .. }));
        assert_eq!(manifest.files.len(), 5);
    }

    #[test]
    fn mounted_ubuntu_layout_is_detected_and_sized_for_fat32() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("mounted");
        fs::create_dir_all(root.join(".disk")).unwrap();
        fs::create_dir_all(root.join("efi/boot")).unwrap();
        fs::create_dir_all(root.join("casper")).unwrap();
        fs::create_dir_all(root.join("boot/grub")).unwrap();
        fs::write(root.join(".disk/info"), b"Ubuntu-Server 24.04 LTS").unwrap();
        fs::write(root.join("efi/boot/bootx64.efi"), b"efi").unwrap();
        fs::write(root.join("casper/vmlinuz"), b"kernel").unwrap();
        fs::write(
            root.join("boot/grub/grub.cfg"),
            b"linux /casper/vmlinuz ---",
        )
        .unwrap();
        let iso = temp.path().join("ubuntu.iso");
        fs::write(&iso, b"iso fingerprint").unwrap();

        let manifest = analyze_mounted_ubuntu_iso(&iso, &root).unwrap();
        assert_eq!(manifest.analysis.kind, ImageKind::UbuntuInstaller);
        assert_eq!(manifest.files.len(), 4);
        assert!(manifest.analysis.required_bytes > 64 * 1024 * 1024);

        fs::File::options()
            .write(true)
            .open(root.join("casper/vmlinuz"))
            .unwrap()
            .set_len(FAT32_MAX_FILE_BYTES + 1)
            .unwrap();
        let oversized = analyze_mounted_ubuntu_iso(&iso, &root).unwrap();
        assert_eq!(oversized.analysis.kind, ImageKind::UbuntuInstaller);
        assert_eq!(oversized.analysis.required_bytes, 0);
        assert!(
            oversized
                .analysis
                .warnings
                .iter()
                .any(|warning| warning.contains("raw image writing remains available"))
        );
    }

    #[test]
    fn verified_copy_detects_and_preserves_content() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("nested/destination.bin");
        let bytes = vec![0x5A_u8; 65_537];
        fs::write(&source, &bytes).unwrap();
        let mut copied = 0_u64;
        let hash =
            copy_file_verified(&source, &destination, true, |count| copied += count).unwrap();
        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(hash, Sha256::digest(&bytes).as_slice());
        assert_eq!(fs::read(destination).unwrap(), bytes);
    }
}
