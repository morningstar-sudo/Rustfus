#![allow(
    clippy::borrow_as_ptr,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use crate::platform::{
    MountedIso, flush_volume, lock_target_volumes, open_physical_disk, update_disk_properties,
    wait_for_volume_mount_path,
};
use crate::{CancellationToken, StreamProgress};
use rustfus_core::{
    DeviceSnapshot, ErrorCode, ImageAnalysis, OperationError, OperationPhase, OperationPlan,
    WriteMode,
};
use rustfus_disk::{Fat32Layout, MbrPartition, format_empty_fat32, write_mbr};
use rustfus_image::{
    InstallImage, STREAM_BUFFER_BYTES, UbuntuMediaManifest, WindowsMediaManifest,
    analyze_hybrid_iso, analyze_mounted_ubuntu_iso, analyze_mounted_windows_iso,
    copy_file_verified_with_buffer, fingerprint_file_with_buffer,
};
use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::fs;
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

type Handle = *mut c_void;
type Bool = i32;
const INVALID_HANDLE_VALUE: Handle = -1_isize as Handle;
const WIM_GENERIC_READ: u32 = 0x8000_0000;
const WIM_OPEN_EXISTING: u32 = 3;
const WTD_UI_NONE: u32 = 2;
const WTD_REVOKE_NONE: u32 = 0;
const WTD_CHOICE_FILE: u32 = 1;
const WTD_STATEACTION_VERIFY: u32 = 1;
const WTD_STATEACTION_CLOSE: u32 = 2;
const WTD_CACHE_ONLY_URL_RETRIEVAL: u32 = 0x0000_1000;
const CERT_NAME_ATTR_TYPE: u32 = 3;
const V01_MAXIMUM_DISK_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const INVALID_CALLBACK_VALUE: u32 = u32::MAX;
const WIM_MSG_SUCCESS: u32 = 0;
const WIM_MSG_ABORT_IMAGE: u32 = u32::MAX;

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct WinTrustFileInfo {
    struct_size: u32,
    file_path: *const u16,
    file: Handle,
    known_subject: *const Guid,
}

#[repr(C)]
struct WinTrustData {
    struct_size: u32,
    policy_callback_data: *mut c_void,
    sip_client_data: *mut c_void,
    ui_choice: u32,
    revocation_checks: u32,
    union_choice: u32,
    file_info: *mut WinTrustFileInfo,
    state_action: u32,
    state_data: Handle,
    url_reference: *const u16,
    provider_flags: u32,
    ui_context: u32,
}

#[repr(C)]
struct CertContext {
    _private: [u8; 0],
}

#[repr(C)]
struct CryptProviderCertHeader {
    struct_size: u32,
    certificate: *const CertContext,
}

#[link(name = "wintrust")]
unsafe extern "system" {
    fn WinVerifyTrust(window: Handle, action_id: *const Guid, trust_data: *mut c_void) -> i32;
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CertGetNameStringW(
        certificate: *const CertContext,
        name_type: u32,
        flags: u32,
        type_parameter: *mut c_void,
        name: *mut u16,
        name_length: u32,
    ) -> u32;
}

unsafe extern "system" {
    fn LoadLibraryW(file_name: *const u16) -> Handle;
    fn GetProcAddress(module: Handle, procedure_name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: Handle) -> Bool;
    fn GetDiskFreeSpaceExW(
        directory_name: *const u16,
        free_bytes_available: *mut u64,
        total_bytes: *mut u64,
        total_free_bytes: *mut u64,
    ) -> Bool;
}

type WimCreateFile = unsafe extern "system" fn(
    path: *const u16,
    desired_access: u32,
    creation_disposition: u32,
    flags_and_attributes: u32,
    compression_type: u32,
    creation_result: *mut u32,
) -> Handle;
type WimSplitFile = unsafe extern "system" fn(Handle, *const u16, *mut i64, u32) -> Bool;
type WimCloseHandle = unsafe extern "system" fn(Handle) -> Bool;
type WimRegisterMessageCallback =
    unsafe extern "system" fn(Handle, *mut c_void, *mut c_void) -> u32;
type WimUnregisterMessageCallback = unsafe extern "system" fn(Handle, *mut c_void) -> Bool;
type TrustProviderDataFromState = unsafe extern "system" fn(Handle) -> *mut c_void;
type TrustSignerFromChain = unsafe extern "system" fn(*mut c_void, u32, Bool, u32) -> *mut c_void;
type TrustCertFromChain =
    unsafe extern "system" fn(*mut c_void, u32) -> *const CryptProviderCertHeader;

pub fn analyze_image(path: &Path) -> Result<ImageAnalysis, OperationError> {
    let mounted = match MountedIso::attach(path) {
        Ok(mounted) => mounted,
        // A hybrid image may still be a valid raw image even if Windows cannot attach it, but if
        // it is not, the user needs to hear about the mount failure too.
        Err(mount_error) => {
            return analyze_hybrid_iso(path)
                .map_err(|error| explain_mount_failure(error, &mount_error));
        }
    };
    if looks_like_windows_media(&mounted.root) {
        // It is a Windows installer: report its real problems instead of falling back to raw.
        let manifest = analyze_mounted_windows_iso(path, &mounted.root)?;
        verify_trusted_executable(&manifest.bootsect_path)?;
        return Ok(manifest.analysis);
    }
    if looks_like_ubuntu_media(&mounted.root) {
        return Ok(analyze_mounted_ubuntu_iso(path, &mounted.root)?.analysis);
    }
    analyze_hybrid_iso(path)
}

/// Windows installation media always carries `bootmgr` and `sources\boot.wim`.
fn looks_like_windows_media(root: &Path) -> bool {
    root.join("bootmgr").exists() || root.join("sources").join("boot.wim").exists()
}

fn looks_like_ubuntu_media(root: &Path) -> bool {
    root.join(".disk").join("info").is_file()
        && root.join("casper").join("vmlinuz").is_file()
        && root.join("boot").join("grub").join("grub.cfg").is_file()
}

fn explain_mount_failure(error: OperationError, mount_error: &OperationError) -> OperationError {
    OperationError {
        message: format!(
            "{}; attaching it as Windows installation media also failed ({})",
            error.message, mount_error.message
        ),
        ..error
    }
}

pub fn cleanup_stale_temporary_directories() -> Result<usize, OperationError> {
    cleanup_stale_under(
        &std::env::temp_dir().join("Rustfus"),
        SystemTime::now(),
        Duration::from_secs(7 * 24 * 60 * 60),
    )
}

fn cleanup_stale_under(
    base: &Path,
    now: SystemTime,
    maximum_age: Duration,
) -> Result<usize, OperationError> {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(io_operation_error(error)),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(io_operation_error)?;
        let file_type = entry.file_type().map_err(io_operation_error)?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let metadata = fs::metadata(entry.path().join(".rustfus-operation"))
            .or_else(|_| entry.metadata())
            .map_err(io_operation_error)?;
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= maximum_age);
        if old_enough {
            fs::remove_dir_all(entry.path()).map_err(io_operation_error)?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn create_windows_installer<F>(
    plan: &OperationPlan,
    device: &DeviceSnapshot,
    unattend: Option<&rustfus_core::UnattendConfig>,
    cancellation: &CancellationToken,
    mut progress: F,
) -> Result<(), OperationError>
where
    F: FnMut(StreamProgress),
{
    if plan.mode != WriteMode::WindowsMbrFat32 {
        return Err(OperationError::new(
            ErrorCode::Internal,
            "A non-Windows operation was sent to the Windows media pipeline",
        ));
    }
    check_cancelled(cancellation, false)?;
    let mounted = MountedIso::attach(&plan.source)?;
    let manifest = analyze_mounted_windows_iso(&plan.source, &mounted.root)?;
    if manifest.analysis.fingerprint != plan.image_fingerprint {
        return Err(OperationError::new(
            ErrorCode::ImageChanged,
            "The Windows ISO changed after the operation was planned",
        ));
    }
    verify_trusted_executable(&manifest.bootsect_path)?;
    ensure_temporary_space(manifest.analysis.temporary_bytes)?;

    emit(&mut progress, OperationPhase::Prepare, 0, 1, Instant::now());
    let target_root = prepare_fat32_target(device, &plan.volume_label)?;
    check_cancelled(cancellation, true)?;
    run_bootsect(&manifest.bootsect_path, &target_root)?;
    let unattend = unattend
        .map(rustfus_core::UnattendConfig::to_xml_bytes)
        .transpose()?;
    copy_windows_tree(
        &manifest,
        &target_root,
        plan.verify,
        unattend.as_deref(),
        cancellation,
        progress,
    )?;
    Ok(())
}

pub fn create_ubuntu_installer<F>(
    plan: &OperationPlan,
    device: &DeviceSnapshot,
    config: &rustfus_core::UbuntuAutoinstallConfig,
    cancellation: &CancellationToken,
    mut progress: F,
) -> Result<(), OperationError>
where
    F: FnMut(StreamProgress),
{
    if plan.mode != WriteMode::UbuntuUefiFat32 {
        return Err(OperationError::new(
            ErrorCode::Internal,
            "A non-Ubuntu operation was sent to the Ubuntu media pipeline",
        ));
    }
    config.validate()?;
    check_cancelled(cancellation, false)?;
    let mounted = MountedIso::attach(&plan.source)?;
    let manifest = analyze_mounted_ubuntu_iso(&plan.source, &mounted.root)?;
    if manifest.analysis.required_bytes == 0 {
        return Err(OperationError::new(
            ErrorCode::UnsupportedLargeFile,
            "Ubuntu autoinstall cannot use this image because it contains a file larger than FAT32's 4 GiB per-file limit",
        ));
    }
    if manifest.analysis.fingerprint != plan.image_fingerprint {
        return Err(OperationError::new(
            ErrorCode::ImageChanged,
            "The Ubuntu ISO changed after the operation was planned",
        ));
    }

    emit(&mut progress, OperationPhase::Prepare, 0, 1, Instant::now());
    let target_root = prepare_fat32_target(device, &plan.volume_label)?;
    check_cancelled(cancellation, true)?;
    copy_ubuntu_tree(
        &manifest,
        &target_root,
        config,
        plan.verify,
        cancellation,
        progress,
    )
}

fn copy_ubuntu_tree<F>(
    manifest: &UbuntuMediaManifest,
    target_root: &Path,
    config: &rustfus_core::UbuntuAutoinstallConfig,
    verify: bool,
    cancellation: &CancellationToken,
    mut progress: F,
) -> Result<(), OperationError>
where
    F: FnMut(StreamProgress),
{
    let total = manifest
        .files
        .iter()
        .map(|file| file.size_bytes)
        .sum::<u64>()
        .saturating_add(config.bytes_to_write());
    let started = Instant::now();
    let mut completed = 0_u64;
    let mut io_buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    let mut written_files = Vec::<(PathBuf, [u8; 32], u64)>::new();
    let mut patched = Vec::<(String, Vec<u8>)>::new();
    let mut md5sum_source = None;

    for file in &manifest.files {
        check_cancelled(cancellation, true)?;
        let key = relative_key(&file.relative_path);
        if matches!(key.as_str(), "nocloud/user-data" | "nocloud/meta-data") {
            continue;
        }
        let source = manifest.root.join(&file.relative_path);
        if key == "md5sum.txt" {
            md5sum_source = Some(source);
            continue;
        }
        let destination = target_root.join(&file.relative_path);
        if is_ubuntu_boot_config(&key) {
            let original = fs::read_to_string(&source).map_err(io_operation_error)?;
            let modified = patch_ubuntu_boot_config(&key, &original)?;
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(io_operation_error)?;
            }
            fs::write(&destination, modified.as_bytes()).map_err(io_operation_error)?;
            let bytes = modified.into_bytes();
            let expected: [u8; 32] = Sha256::digest(&bytes).into();
            completed = completed.saturating_add(bytes.len() as u64);
            emit(
                &mut progress,
                OperationPhase::Write,
                completed,
                total.max(completed),
                started,
            );
            written_files.push((destination, expected, bytes.len() as u64));
            patched.push((key, bytes));
        } else {
            let expected = copy_file_verified_with_buffer(
                &source,
                &destination,
                false,
                &mut io_buffer,
                |count| {
                    completed = completed.saturating_add(count);
                    emit(
                        &mut progress,
                        OperationPhase::Write,
                        completed,
                        total.max(completed),
                        started,
                    );
                },
            )?;
            written_files.push((destination, expected, file.size_bytes));
        }
    }

    if !patched.iter().any(|(path, _)| path == "boot/grub/grub.cfg") {
        return Err(OperationError::new(
            ErrorCode::InvalidImage,
            "Ubuntu boot/grub/grub.cfg was not patched for autoinstall",
        ));
    }

    for (name, bytes) in [
        ("user-data", config.user_data.as_bytes()),
        ("meta-data", config.meta_data.as_bytes()),
    ] {
        let destination = target_root.join("nocloud").join(name);
        fs::create_dir_all(destination.parent().unwrap()).map_err(io_operation_error)?;
        fs::write(&destination, bytes).map_err(io_operation_error)?;
        let expected: [u8; 32] = Sha256::digest(bytes).into();
        completed = completed.saturating_add(bytes.len() as u64);
        emit(
            &mut progress,
            OperationPhase::Write,
            completed,
            total.max(completed),
            started,
        );
        written_files.push((destination, expected, bytes.len() as u64));
    }

    if let Some(source) = md5sum_source {
        let original = fs::read_to_string(source).map_err(io_operation_error)?;
        let updated = update_ubuntu_md5s(&original, &patched);
        let destination = target_root.join("md5sum.txt");
        fs::write(&destination, updated.as_bytes()).map_err(io_operation_error)?;
        let expected: [u8; 32] = Sha256::digest(updated.as_bytes()).into();
        written_files.push((destination, expected, updated.len() as u64));
    }

    flush_volume(target_root)?;
    verify_written_files(
        written_files,
        verify,
        cancellation,
        &mut io_buffer,
        &mut progress,
    )
}

fn relative_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn is_ubuntu_boot_config(path: &str) -> bool {
    matches!(
        path,
        "boot/grub/grub.cfg" | "boot/grub/loopback.cfg" | "isolinux/txt.cfg"
    )
}

fn patch_ubuntu_boot_config(path: &str, original: &str) -> Result<String, OperationError> {
    if original.contains("autoinstall") && original.contains("ds=nocloud") {
        return Ok(original.to_owned());
    }
    if !original.contains("---") {
        if path != "boot/grub/grub.cfg" {
            return Ok(original.to_owned());
        }
        return Err(OperationError::new(
            ErrorCode::InvalidImage,
            format!("Ubuntu boot configuration {path} has no kernel argument terminator"),
        ));
    }
    let data_source = if path.starts_with("boot/grub/") {
        r" autoinstall ds=nocloud\;s=/cdrom/nocloud/ ---"
    } else {
        " autoinstall ds=nocloud;s=/cdrom/nocloud/ ---"
    };
    Ok(original.replace("---", data_source))
}

fn update_ubuntu_md5s(original: &str, patched: &[(String, Vec<u8>)]) -> String {
    let mut output = String::with_capacity(original.len());
    for line in original.lines() {
        let replacement = patched.iter().find(|(path, _)| {
            line.get(34..)
                .map(str::trim)
                .is_some_and(|listed| listed.trim_start_matches("./").eq_ignore_ascii_case(path))
        });
        if let Some((_, bytes)) = replacement {
            let digest = md5::Md5::digest(bytes);
            output.push_str(&hex::encode(digest));
            output.push_str(&line[32..]);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output
}

fn verify_written_files<F>(
    written_files: Vec<(PathBuf, [u8; 32], u64)>,
    verify: bool,
    cancellation: &CancellationToken,
    io_buffer: &mut [u8],
    progress: &mut F,
) -> Result<(), OperationError>
where
    F: FnMut(StreamProgress),
{
    if !verify {
        return Ok(());
    }
    let verify_total = written_files.iter().map(|(_, _, size)| size).sum::<u64>();
    let verify_started = Instant::now();
    let mut verified = 0_u64;
    for (destination, expected, _) in written_files {
        check_cancelled(cancellation, true)?;
        let actual = fingerprint_file_with_buffer(&destination, io_buffer, |count| {
            check_cancelled(cancellation, true)?;
            verified = verified.saturating_add(count);
            emit(
                progress,
                OperationPhase::Verify,
                verified,
                verify_total,
                verify_started,
            );
            Ok(())
        })?
        .0;
        if actual != expected {
            return Err(OperationError::new(
                ErrorCode::VerificationFailed,
                format!(
                    "Read-back verification failed for {} (expected {}, got {})",
                    destination.display(),
                    hex::encode(expected),
                    hex::encode(actual)
                ),
            ));
        }
    }
    Ok(())
}

fn ensure_temporary_space(required_bytes: u64) -> Result<(), OperationError> {
    if required_bytes == 0 {
        return Ok(());
    }
    let temp = wide_null(&std::env::temp_dir());
    let mut available = 0_u64;
    let success = unsafe {
        GetDiskFreeSpaceExW(
            temp.as_ptr(),
            &mut available,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if success == 0 {
        return Err(io_operation_error(io::Error::last_os_error()));
    }
    if available < required_bytes {
        return Err(OperationError::new(
            ErrorCode::InsufficientCapacity,
            format!(
                "WIM splitting requires {} MiB of temporary space but only {} MiB is available",
                required_bytes / 1024 / 1024,
                available / 1024 / 1024
            ),
        ));
    }
    Ok(())
}

fn prepare_fat32_target(
    device: &DeviceSnapshot,
    volume_label: &str,
) -> Result<PathBuf, OperationError> {
    if device.capacity_bytes > V01_MAXIMUM_DISK_BYTES {
        return Err(OperationError::new(
            ErrorCode::InsufficientCapacity,
            "Windows media targets larger than 2 TiB are not supported in v0.1",
        ));
    }
    let sector_size = u16::try_from(device.logical_sector_size).map_err(|_| {
        OperationError::new(
            ErrorCode::Internal,
            "Logical sector size exceeds FAT32 limits",
        )
    })?;
    let total_sectors = device.capacity_bytes / u64::from(sector_size);
    let start_lba = (1024_u64 * 1024).div_ceil(u64::from(sector_size));
    if total_sectors <= start_lba || total_sectors - start_lba > u64::from(u32::MAX) {
        return Err(OperationError::new(
            ErrorCode::InsufficientCapacity,
            "The USB disk is outside the supported MBR size range",
        ));
    }
    let start_lba = u32::try_from(start_lba).map_err(|_| {
        OperationError::new(ErrorCode::Internal, "Partition start exceeds MBR limits")
    })?;
    let sector_count = u32::try_from(total_sectors - u64::from(start_lba)).map_err(|_| {
        OperationError::new(
            ErrorCode::InsufficientCapacity,
            "USB disk exceeds MBR limits",
        )
    })?;
    let partition_bytes = u64::from(sector_count) * u64::from(sector_size);
    let layout = Fat32Layout::calculate(partition_bytes, sector_size, start_lba)
        .map_err(disk_operation_error)?;

    let locks = lock_target_volumes(device.disk_number)?;
    let mut disk = open_physical_disk(device.disk_number, true)?;
    let random = Uuid::new_v4();
    let disk_signature = u32::from_le_bytes(random.as_bytes()[..4].try_into().unwrap());
    let volume_id = u32::from_le_bytes(random.as_bytes()[4..8].try_into().unwrap());
    write_mbr(
        &mut disk,
        disk_signature,
        MbrPartition::fat32_lba(start_lba, sector_count),
    )
    .map_err(disk_operation_error)?;
    format_empty_fat32(&mut disk, &layout, volume_id, volume_label)
        .map_err(disk_operation_error)?;
    disk.sync_all().map_err(io_operation_error)?;
    update_disk_properties(&disk)?;
    drop(disk);
    drop(locks);
    wait_for_volume_mount_path(device.disk_number, Duration::from_secs(20))
}

fn run_bootsect(bootsect: &Path, target_root: &Path) -> Result<(), OperationError> {
    verify_trusted_executable(bootsect)?;
    let root = target_root.to_string_lossy();
    let bytes = root.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' {
        return Err(OperationError::new(
            ErrorCode::Io,
            "Windows did not assign a drive letter to the target volume",
        ));
    }
    let drive = &root[..2];
    let status = Command::new(bootsect)
        .args(["/nt60", drive, "/mbr", "/force"])
        .status()
        .map_err(io_operation_error)?;
    if !status.success() {
        return Err(OperationError::new(
            ErrorCode::Io,
            format!("bootsect.exe failed with status {status}"),
        ));
    }
    Ok(())
}

fn copy_windows_tree<F>(
    manifest: &WindowsMediaManifest,
    target_root: &Path,
    verify: bool,
    unattend: Option<&[u8]>,
    cancellation: &CancellationToken,
    mut progress: F,
) -> Result<(), OperationError>
where
    F: FnMut(StreamProgress),
{
    let split_wim = match &manifest.install_image {
        InstallImage::Wim {
            path,
            split_required: true,
        } => Some(path.clone()),
        _ => None,
    };
    let total = manifest
        .files
        .iter()
        .map(|file| file.size_bytes)
        .sum::<u64>()
        .saturating_add(unattend.map_or(0, |bytes| bytes.len() as u64));
    let started = Instant::now();
    let mut completed = 0_u64;
    let mut io_buffer = vec![0_u8; STREAM_BUFFER_BYTES];
    let mut written_files = Vec::<(PathBuf, [u8; 32], u64)>::new();
    for file in &manifest.files {
        check_cancelled(cancellation, true)?;
        let source = manifest.root.join(&file.relative_path);
        if split_wim
            .as_deref()
            .is_some_and(|wim| same_path(&source, wim))
        {
            continue;
        }
        let destination = target_root.join(&file.relative_path);
        let expected = copy_file_verified_with_buffer(
            &source,
            &destination,
            false,
            &mut io_buffer,
            |count| {
                completed = completed.saturating_add(count);
                emit(
                    &mut progress,
                    OperationPhase::Write,
                    completed,
                    total.max(completed),
                    started,
                );
            },
        )?;
        written_files.push((destination, expected, file.size_bytes));
    }

    if let Some(wim) = split_wim {
        check_cancelled(cancellation, true)?;
        let temp = TemporaryDirectory::new()?;
        split_wim_file(&wim, temp.path(), cancellation)?;
        let mut parts = fs::read_dir(temp.path())
            .map_err(io_operation_error)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("swm"))
            })
            .collect::<Vec<_>>();
        parts.sort();
        if parts.is_empty() {
            return Err(OperationError::new(
                ErrorCode::Io,
                "WIMGAPI did not produce any split WIM parts",
            ));
        }
        for part in parts {
            check_cancelled(cancellation, true)?;
            let name = part.file_name().ok_or_else(|| {
                OperationError::new(ErrorCode::Internal, "Split WIM part has no filename")
            })?;
            let destination = target_root.join("sources").join(name);
            let part_size = fs::metadata(&part).map_err(io_operation_error)?.len();
            let expected = copy_file_verified_with_buffer(
                &part,
                &destination,
                false,
                &mut io_buffer,
                |count| {
                    completed = completed.saturating_add(count);
                    emit(
                        &mut progress,
                        OperationPhase::Write,
                        completed,
                        total.max(completed),
                        started,
                    );
                },
            )?;
            written_files.push((destination, expected, part_size));
        }
    }

    if let Some(bytes) = unattend {
        check_cancelled(cancellation, true)?;
        let destination = target_root.join("autounattend.xml");
        fs::write(&destination, bytes).map_err(io_operation_error)?;
        let expected: [u8; 32] = Sha256::digest(bytes).into();
        completed = completed.saturating_add(bytes.len() as u64);
        emit(
            &mut progress,
            OperationPhase::Write,
            completed,
            total.max(completed),
            started,
        );
        written_files.push((destination, expected, bytes.len() as u64));
    }

    // Every destination file handle is closed at this point. A single volume
    // durability boundary avoids per-file flush stalls while ensuring that
    // verification starts only after cached writes reached the storage stack.
    flush_volume(target_root)?;

    if verify {
        let verify_total = written_files.iter().map(|(_, _, size)| size).sum::<u64>();
        let verify_started = Instant::now();
        let mut verified = 0_u64;
        for (destination, expected, _) in written_files {
            check_cancelled(cancellation, true)?;
            let actual = fingerprint_file_with_buffer(&destination, &mut io_buffer, |count| {
                check_cancelled(cancellation, true)?;
                verified = verified.saturating_add(count);
                emit(
                    &mut progress,
                    OperationPhase::Verify,
                    verified,
                    verify_total,
                    verify_started,
                );
                Ok(())
            })?
            .0;
            if actual != expected {
                return Err(OperationError::new(
                    ErrorCode::VerificationFailed,
                    format!(
                        "Read-back verification failed for {} (expected {}, got {})",
                        destination.display(),
                        hex::encode(expected),
                        hex::encode(actual)
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn split_wim_file(
    source: &Path,
    output_dir: &Path,
    cancellation: &CancellationToken,
) -> Result<(), OperationError> {
    let api = WimApi::load()?;
    let source = wide_null(source);
    let first_part = wide_null(&output_dir.join("install.swm"));
    let mut creation_result = 0_u32;
    let handle = unsafe {
        (api.create_file)(
            source.as_ptr(),
            WIM_GENERIC_READ,
            WIM_OPEN_EXISTING,
            0,
            0,
            &mut creation_result,
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io_operation_error(io::Error::last_os_error()));
    }
    let guard = WimHandle {
        handle,
        close: api.close_handle,
    };
    let callback = wim_cancellation_callback as *const () as *mut c_void;
    let callback_index = unsafe {
        (api.register_callback)(
            guard.handle,
            callback,
            ptr::from_ref(cancellation).cast_mut().cast(),
        )
    };
    if callback_index == INVALID_CALLBACK_VALUE {
        return Err(io_operation_error(io::Error::last_os_error()));
    }
    let _callback_guard = WimCallbackGuard {
        handle: guard.handle,
        callback,
        unregister: api.unregister_callback,
    };
    let mut part_size = 3_800_i64 * 1024 * 1024;
    let success = unsafe { (api.split_file)(guard.handle, first_part.as_ptr(), &mut part_size, 0) };
    if success == 0 {
        check_cancelled(cancellation, true)?;
        return Err(io_operation_error(io::Error::last_os_error()));
    }
    check_cancelled(cancellation, true)
}

unsafe extern "system" fn wim_cancellation_callback(
    _message_id: u32,
    _wparam: usize,
    _lparam: isize,
    user_data: *mut c_void,
) -> u32 {
    if user_data.is_null() {
        return WIM_MSG_SUCCESS;
    }
    let cancellation = unsafe { &*user_data.cast::<CancellationToken>() };
    if cancellation.is_cancelled() {
        WIM_MSG_ABORT_IMAGE
    } else {
        WIM_MSG_SUCCESS
    }
}

fn verify_trusted_executable(path: &Path) -> Result<(), OperationError> {
    let path_wide = wide_null(path);
    let mut file_info = WinTrustFileInfo {
        struct_size: mem::size_of::<WinTrustFileInfo>() as u32,
        file_path: path_wide.as_ptr(),
        file: ptr::null_mut(),
        known_subject: ptr::null(),
    };
    let mut trust_data = WinTrustData {
        struct_size: mem::size_of::<WinTrustData>() as u32,
        policy_callback_data: ptr::null_mut(),
        sip_client_data: ptr::null_mut(),
        ui_choice: WTD_UI_NONE,
        revocation_checks: WTD_REVOKE_NONE,
        union_choice: WTD_CHOICE_FILE,
        file_info: &mut file_info,
        state_action: WTD_STATEACTION_VERIFY,
        state_data: ptr::null_mut(),
        url_reference: ptr::null(),
        provider_flags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ui_context: 0,
    };
    let action = Guid {
        data1: 0x00AA_C56B,
        data2: 0xCD44,
        data3: 0x11D0,
        data4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
    };
    let status =
        unsafe { WinVerifyTrust(INVALID_HANDLE_VALUE, &action, (&raw mut trust_data).cast()) };
    let result = if status != 0 {
        Err(OperationError::new(
            ErrorCode::SignatureInvalid,
            format!("{} does not have a trusted signature", path.display()),
        )
        .with_platform_code(status as u32))
    } else {
        verify_microsoft_publisher(trust_data.state_data, path)
    };
    trust_data.state_action = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &raw const action,
            (&raw mut trust_data).cast(),
        );
    }
    result
}

fn verify_microsoft_publisher(state_data: Handle, path: &Path) -> Result<(), OperationError> {
    if state_data.is_null() {
        return Err(signature_error(
            path,
            "WinVerifyTrust returned no signer state",
        ));
    }
    let library_name = wide_null(Path::new("wintrust.dll"));
    let module = unsafe { LoadLibraryW(library_name.as_ptr()) };
    if module.is_null() {
        return Err(signature_error(path, "Could not load Wintrust.dll"));
    }
    let library = DynamicLibrary(module);
    let provider_from_state = unsafe {
        mem::transmute::<*mut c_void, TrustProviderDataFromState>(load_symbol(
            library.0,
            c"WTHelperProvDataFromStateData".as_ptr().cast(),
        )?)
    };
    let signer_from_chain = unsafe {
        mem::transmute::<*mut c_void, TrustSignerFromChain>(load_symbol(
            library.0,
            c"WTHelperGetProvSignerFromChain".as_ptr().cast(),
        )?)
    };
    let cert_from_chain = unsafe {
        mem::transmute::<*mut c_void, TrustCertFromChain>(load_symbol(
            library.0,
            c"WTHelperGetProvCertFromChain".as_ptr().cast(),
        )?)
    };
    let provider = unsafe { provider_from_state(state_data) };
    if provider.is_null() {
        return Err(signature_error(
            path,
            "Could not inspect Authenticode provider data",
        ));
    }
    let signer = unsafe { signer_from_chain(provider, 0, 0, 0) };
    if signer.is_null() {
        return Err(signature_error(path, "The Authenticode signer is missing"));
    }
    let provider_cert = unsafe { cert_from_chain(signer, 0) };
    if provider_cert.is_null() {
        return Err(signature_error(path, "The signer certificate is missing"));
    }
    let certificate = unsafe { (*provider_cert).certificate };
    if certificate.is_null() {
        return Err(signature_error(path, "The signer certificate is invalid"));
    }
    let organization = certificate_organization(certificate)?;
    if !organization.eq_ignore_ascii_case("Microsoft Corporation") {
        return Err(signature_error(
            path,
            &format!("Signer organization is {organization:?}, not Microsoft Corporation"),
        ));
    }
    Ok(())
}

fn certificate_organization(certificate: *const CertContext) -> Result<String, OperationError> {
    let organization_oid = c"2.5.4.10";
    let type_parameter = organization_oid.as_ptr().cast_mut().cast();
    let required = unsafe {
        CertGetNameStringW(
            certificate,
            CERT_NAME_ATTR_TYPE,
            0,
            type_parameter,
            ptr::null_mut(),
            0,
        )
    };
    if required <= 1 {
        return Err(OperationError::new(
            ErrorCode::SignatureInvalid,
            "The signer certificate has no organization attribute",
        ));
    }
    let mut name = vec![0_u16; required as usize];
    let written = unsafe {
        CertGetNameStringW(
            certificate,
            CERT_NAME_ATTR_TYPE,
            0,
            type_parameter,
            name.as_mut_ptr(),
            required,
        )
    };
    if written != required {
        return Err(OperationError::new(
            ErrorCode::SignatureInvalid,
            "Could not read the signer organization",
        ));
    }
    Ok(String::from_utf16_lossy(&name[..name.len() - 1]))
}

fn signature_error(path: &Path, detail: &str) -> OperationError {
    OperationError::new(
        ErrorCode::SignatureInvalid,
        format!("{} is not signed by Microsoft: {detail}", path.display()),
    )
}

fn check_cancelled(
    cancellation: &CancellationToken,
    media_may_be_incomplete: bool,
) -> Result<(), OperationError> {
    if cancellation.is_cancelled() {
        let message = if media_may_be_incomplete {
            "The operation was cancelled; the target media is incomplete"
        } else {
            "The operation was cancelled before writing began"
        };
        Err(OperationError::new(ErrorCode::Cancelled, message))
    } else {
        Ok(())
    }
}

fn emit(
    callback: &mut impl FnMut(StreamProgress),
    phase: OperationPhase,
    completed_bytes: u64,
    total_bytes: u64,
    started: Instant,
) {
    callback(StreamProgress {
        phase,
        completed_bytes,
        total_bytes,
        bytes_per_second: completed_bytes as f64 / started.elapsed().as_secs_f64().max(0.001),
    });
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.canonicalize().ok() == right.canonicalize().ok()
}

fn wide_null(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn disk_operation_error(error: rustfus_disk::DiskError) -> OperationError {
    OperationError::new(ErrorCode::Io, error.to_string())
}

fn io_operation_error(error: io::Error) -> OperationError {
    let mut result = OperationError::new(ErrorCode::Io, error.to_string());
    if let Some(code) = error.raw_os_error() {
        result = result.with_platform_code(code as u32);
    }
    result
}

struct WimApi {
    module: Handle,
    create_file: WimCreateFile,
    split_file: WimSplitFile,
    close_handle: WimCloseHandle,
    register_callback: WimRegisterMessageCallback,
    unregister_callback: WimUnregisterMessageCallback,
}

impl WimApi {
    fn load() -> Result<Self, OperationError> {
        let library = wide_null(Path::new("wimgapi.dll"));
        let module = unsafe { LoadLibraryW(library.as_ptr()) };
        if module.is_null() {
            return Err(io_operation_error(io::Error::last_os_error()));
        }
        let result = (|| {
            let create_file = unsafe {
                mem::transmute::<*mut c_void, WimCreateFile>(load_symbol(
                    module,
                    c"WIMCreateFile".as_ptr().cast(),
                )?)
            };
            let split_file = unsafe {
                mem::transmute::<*mut c_void, WimSplitFile>(load_symbol(
                    module,
                    c"WIMSplitFile".as_ptr().cast(),
                )?)
            };
            let close_handle = unsafe {
                mem::transmute::<*mut c_void, WimCloseHandle>(load_symbol(
                    module,
                    c"WIMCloseHandle".as_ptr().cast(),
                )?)
            };
            let register_callback = unsafe {
                mem::transmute::<*mut c_void, WimRegisterMessageCallback>(load_symbol(
                    module,
                    c"WIMRegisterMessageCallback".as_ptr().cast(),
                )?)
            };
            let unregister_callback = unsafe {
                mem::transmute::<*mut c_void, WimUnregisterMessageCallback>(load_symbol(
                    module,
                    c"WIMUnregisterMessageCallback".as_ptr().cast(),
                )?)
            };
            Ok(Self {
                module,
                create_file,
                split_file,
                close_handle,
                register_callback,
                unregister_callback,
            })
        })();
        if result.is_err() {
            unsafe {
                FreeLibrary(module);
            }
        }
        result
    }
}

impl Drop for WimApi {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.module);
        }
    }
}

fn load_symbol(module: Handle, name: *const u8) -> Result<*mut c_void, OperationError> {
    let symbol = unsafe { GetProcAddress(module, name) };
    if symbol.is_null() {
        Err(io_operation_error(io::Error::last_os_error()))
    } else {
        Ok(symbol)
    }
}

struct DynamicLibrary(Handle);

impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self.0);
        }
    }
}

struct WimHandle {
    handle: Handle,
    close: WimCloseHandle,
}

struct WimCallbackGuard {
    handle: Handle,
    callback: *mut c_void,
    unregister: WimUnregisterMessageCallback,
}

impl Drop for WimCallbackGuard {
    fn drop(&mut self) {
        unsafe {
            (self.unregister)(self.handle, self.callback);
        }
    }
}

impl Drop for WimHandle {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle);
        }
    }
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Result<Self, OperationError> {
        let path = std::env::temp_dir()
            .join("Rustfus")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&path).map_err(io_operation_error)?;
        fs::write(
            path.join(".rustfus-operation"),
            b"Rustfus WIM split workspace\n",
        )
        .map_err(io_operation_error)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!(
                "rustfus: could not clean WIM temporary directory {}: {error}",
                self.0.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfus_core::BusType;
    use sha2::Digest;

    #[test]
    fn wim_callback_reflects_cancellation_token() {
        let cancellation = CancellationToken::default();
        let user_data = ptr::from_ref(&cancellation).cast_mut().cast();
        let active = unsafe { wim_cancellation_callback(0, 0, 0, user_data) };
        assert_eq!(active, WIM_MSG_SUCCESS);
        cancellation.cancel();
        let cancelled = unsafe { wim_cancellation_callback(0, 0, 0, user_data) };
        assert_eq!(cancelled, WIM_MSG_ABORT_IMAGE);
    }

    #[test]
    fn microsoft_windows_binary_has_expected_publisher() {
        let Some(program_files) = std::env::var_os("ProgramFiles(x86)") else {
            return;
        };
        let edge = PathBuf::from(program_files).join("Microsoft/Edge/Application/msedge.exe");
        if edge.is_file() {
            verify_trusted_executable(&edge).unwrap();
        }
    }

    #[test]
    fn windows_media_is_recognized_by_its_boot_files() {
        let empty = tempfile::tempdir().unwrap();
        assert!(!looks_like_windows_media(empty.path()));

        // A Linux image has neither Windows boot file.
        let linux = tempfile::tempdir().unwrap();
        fs::create_dir_all(linux.path().join("isolinux")).unwrap();
        fs::write(linux.path().join("isolinux").join("isolinux.bin"), b"x").unwrap();
        assert!(!looks_like_windows_media(linux.path()));

        let by_bootmgr = tempfile::tempdir().unwrap();
        fs::write(by_bootmgr.path().join("bootmgr"), b"x").unwrap();
        assert!(looks_like_windows_media(by_bootmgr.path()));

        let by_boot_wim = tempfile::tempdir().unwrap();
        fs::create_dir_all(by_boot_wim.path().join("sources")).unwrap();
        fs::write(by_boot_wim.path().join("sources").join("boot.wim"), b"x").unwrap();
        assert!(looks_like_windows_media(by_boot_wim.path()));
    }

    #[test]
    fn mount_failure_is_added_to_the_reported_error_without_changing_its_code() {
        let error = explain_mount_failure(
            OperationError::new(ErrorCode::InvalidImage, "no MBR signature"),
            &OperationError::new(ErrorCode::InvalidImage, "Could not attach ISO"),
        );
        assert_eq!(error.code, ErrorCode::InvalidImage);
        assert!(error.message.starts_with("no MBR signature"));
        assert!(error.message.contains("Could not attach ISO"));
    }

    #[test]
    fn ubuntu_boot_arguments_are_patched_for_grub_and_isolinux() {
        let grub =
            patch_ubuntu_boot_config("boot/grub/grub.cfg", "linux /casper/vmlinuz quiet ---\n")
                .unwrap();
        assert!(grub.contains(r"autoinstall ds=nocloud\;s=/cdrom/nocloud/ ---"));

        let isolinux = patch_ubuntu_boot_config(
            "isolinux/txt.cfg",
            "append initrd=/casper/initrd quiet ---\n",
        )
        .unwrap();
        assert!(isolinux.contains("autoinstall ds=nocloud;s=/cdrom/nocloud/ ---"));
        assert!(!isolinux.contains(r"nocloud\;"));
    }

    #[test]
    fn ubuntu_optional_boot_config_without_terminator_is_preserved() {
        let original = "source /boot/grub/grub.cfg\n";
        assert_eq!(
            patch_ubuntu_boot_config("boot/grub/loopback.cfg", original).unwrap(),
            original
        );
        assert_eq!(
            patch_ubuntu_boot_config("boot/grub/grub.cfg", original)
                .unwrap_err()
                .code,
            ErrorCode::InvalidImage
        );
    }

    #[test]
    fn ubuntu_md5_manifest_tracks_patched_boot_files() {
        let bytes = b"patched grub\n".to_vec();
        let original = concat!(
            "00000000000000000000000000000000  ./boot/grub/grub.cfg\n",
            "11111111111111111111111111111111  ./casper/vmlinuz\n"
        );
        let updated = update_ubuntu_md5s(
            original,
            &[("boot/grub/grub.cfg".to_owned(), bytes.clone())],
        );
        let expected = hex::encode(md5::Md5::digest(bytes));
        assert!(updated.starts_with(&expected));
        assert!(updated.contains("11111111111111111111111111111111  ./casper/vmlinuz"));
    }

    #[test]
    fn unsigned_executable_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let unsigned = directory.path().join("unsigned.exe");
        fs::write(&unsigned, b"not a portable executable").unwrap();
        assert_eq!(
            verify_trusted_executable(&unsigned).unwrap_err().code,
            ErrorCode::SignatureInvalid
        );
    }

    #[test]
    fn stale_temporary_directories_are_removed_selectively() {
        let base = tempfile::tempdir().unwrap();
        let stale = base.path().join("stale-operation");
        let fresh = base.path().join("fresh-operation");
        fs::create_dir(&stale).unwrap();
        fs::create_dir(&fresh).unwrap();
        let stale_marker = stale.join(".rustfus-operation");
        let fresh_marker = fresh.join(".rustfus-operation");
        fs::write(&stale_marker, b"stale").unwrap();
        fs::write(&fresh_marker, b"fresh").unwrap();
        let now = SystemTime::now();
        fs::File::options()
            .write(true)
            .open(&stale_marker)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(now - Duration::from_secs(8 * 24 * 60 * 60)),
            )
            .unwrap();

        let removed =
            cleanup_stale_under(base.path(), now, Duration::from_secs(7 * 24 * 60 * 60)).unwrap();
        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn disposable_vhdx_guard_rejects_relative_paths() {
        let error = crate::platform::TestVirtualDisk::create(
            Path::new("unsafe-relative.vhdx"),
            8 * 1024 * 1024 * 1024,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsafeTarget);
    }

    #[test]
    #[ignore = "requires an elevated Windows runner and creates a disposable VHDX"]
    fn disposable_vhdx_formats_mounts_and_passes_chkdsk() {
        let directory = tempfile::tempdir().unwrap();
        let vhdx_path = directory.path().join("rustfus-disposable.vhdx");
        let vhdx =
            crate::platform::TestVirtualDisk::create(&vhdx_path, 8 * 1024 * 1024 * 1024).unwrap();
        let device = DeviceSnapshot {
            instance_id: format!("test-vhdx:{}", vhdx.disk_number),
            serial: Some("RUSTFUS-DISPOSABLE-VHDX".into()),
            model: "Rustfus disposable VHDX".into(),
            bus: BusType::Usb,
            disk_number: vhdx.disk_number,
            capacity_bytes: vhdx.capacity_bytes,
            logical_sector_size: 512,
            physical_sector_size: 512,
            volumes: Vec::new(),
            contains_system: false,
            contains_boot: false,
            contains_pagefile: false,
            contains_crashdump: false,
        };

        let root = prepare_fat32_target(&device, "RUSTFUS").unwrap();
        let probe = root.join("rustfus-verify.bin");
        let bytes = (0..8 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        fs::write(&probe, &bytes).unwrap();
        crate::platform::flush_volume(&root).unwrap();
        assert!(crate::platform::path_is_on_disk(&probe, vhdx.disk_number).unwrap());
        assert_eq!(
            rustfus_image::fingerprint_file(&probe).unwrap().0,
            sha2::Sha256::digest(&bytes).as_slice()
        );

        let status = Command::new("chkdsk.exe")
            .arg(&root)
            .args(["/f", "/x"])
            .status()
            .unwrap();
        assert!(status.success(), "chkdsk failed with {status}");
    }

    /// Exercises the raw-image writer (pipeline, sector padding, durable sync, read-back
    /// verification) against a real block device: a disposable VHDX, never a physical disk.
    #[test]
    #[ignore = "requires an elevated Windows runner and creates a disposable VHDX"]
    fn disposable_vhdx_raw_write_round_trips_and_verifies() {
        use std::io::{Cursor, Read};

        let directory = tempfile::tempdir().unwrap();
        let vhdx_path = directory.path().join("rustfus-raw.vhdx");
        let vhdx =
            crate::platform::TestVirtualDisk::create(&vhdx_path, 1024 * 1024 * 1024).unwrap();

        // Not a multiple of the block or sector size, so padding and the last block are covered.
        let bytes: Vec<u8> = (0..2 * crate::RAW_BLOCK_BYTES + 1000)
            .map(|index| u8::try_from((index.wrapping_mul(131) ^ (index >> 11)) & 0xFF).unwrap())
            .collect();
        let mut source = Cursor::new(bytes.clone());
        let mut target = crate::platform::open_physical_disk(vhdx.disk_number, true).unwrap();
        let token = crate::CancellationToken::default();
        let mut verified = 0_u64;
        let hash = crate::write_raw_image(
            &mut source,
            &mut target,
            crate::RawWriteOptions {
                source_length: bytes.len() as u64,
                sector_alignment: 512,
                verify: true,
                cancellation: &token,
            },
            |disk| {
                disk.sync_all()
                    .map_err(|error| OperationError::new(ErrorCode::Io, error.to_string()))
            },
            |update| {
                if update.phase == rustfus_core::OperationPhase::Verify {
                    verified = update.completed_bytes;
                }
            },
        )
        .unwrap();
        assert_eq!(hash, sha2::Sha256::digest(&bytes).as_slice());
        assert_eq!(verified, bytes.len() as u64);

        // Read it back through a completely separate handle.
        drop(target);
        let mut reader = crate::platform::open_physical_disk(vhdx.disk_number, false).unwrap();
        let mut read_back = vec![0_u8; bytes.len().div_ceil(512) * 512];
        reader.read_exact(&mut read_back).unwrap();
        assert_eq!(&read_back[..bytes.len()], bytes.as_slice());
        assert!(read_back[bytes.len()..].iter().all(|byte| *byte == 0));
    }
}
