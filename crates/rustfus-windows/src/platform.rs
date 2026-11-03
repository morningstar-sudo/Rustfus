#![allow(
    clippy::borrow_as_ptr,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use crate::system_disks::{self, Inputs, SystemDisks};
use rustfus_core::{BusType, DeviceSnapshot, ErrorCode, OperationError};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

type Handle = *mut c_void;
type Bool = i32;

const INVALID_HANDLE_VALUE: Handle = -1_isize as Handle;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const STORAGE_ACCESS_ALIGNMENT_PROPERTY: u32 = 6;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const IOCTL_DISK_UPDATE_PROPERTIES: u32 = 0x0007_0140;
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;
const BUS_TYPE_USB: u32 = 7;
const VIRTUAL_DISK_ACCESS_ATTACH_RO: u32 = 0x0001_0000;
const VIRTUAL_DISK_ACCESS_GET_INFO: u32 = 0x0008_0000;
const FILE_DEVICE_CD_ROM: u32 = 2;
const ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY: u32 = 0x0000_0001;
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_TYPE_BYTE: u32 = 0;
const PIPE_READMODE_BYTE: u32 = 0;
const PIPE_WAIT: u32 = 0;
const PIPE_NOWAIT: u32 = 1;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_PIPE_LISTENING: u32 = 536;
const WORKER_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;
const SDDL_REVISION_1: u32 = 1;
const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
const SEE_MASK_NOASYNC: u32 = 0x0000_0100;
const SW_HIDE: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_INVALID_FUNCTION: i32 = 1;
const ERROR_NOT_SUPPORTED: i32 = 50;
const ERROR_NO_MORE_FILES: u32 = 18;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_NO_MORE_ITEMS: u32 = 259;
const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
const RRF_RT_ANY: u32 = 0x0000_FFFF;
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_MULTI_SZ: u32 = 7;
const HKEY_LOCAL_MACHINE: Handle = 0x8000_0002_usize as Handle;
const MEMORY_MANAGEMENT_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management";
const CRASH_CONTROL_KEY: &str = r"SYSTEM\CurrentControlSet\Control\CrashControl";
const SETUP_KEY: &str = r"SYSTEM\Setup";
const SYSTEM_SYSTEM_PARTITION_INFORMATION: u32 = 98;

#[derive(Clone, Copy)]
#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct VirtualStorageType {
    device_id: u32,
    vendor_id: Guid,
}

#[repr(C)]
struct OpenVirtualDiskParameters {
    version: u32,
    rw_depth: u32,
}

#[repr(C)]
struct AttachVirtualDiskParameters {
    version: u32,
    reserved: u32,
}

#[cfg(test)]
#[repr(C)]
struct CreateVirtualDiskParametersV1 {
    version: u32,
    union_alignment_padding: u32,
    unique_id: Guid,
    maximum_size: u64,
    block_size_in_bytes: u32,
    sector_size_in_bytes: u32,
    parent_path: *const u16,
    source_path: *const u16,
}

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    security_descriptor: *mut c_void,
    inherit_handle: Bool,
}

#[repr(C)]
struct ShellExecuteInfo {
    size: u32,
    mask: u32,
    window: Handle,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    instance: Handle,
    id_list: *mut c_void,
    class: *const u16,
    class_key: Handle,
    hot_key: u32,
    icon_or_monitor: Handle,
    process: Handle,
}

#[repr(C)]
struct DeviceInterfaceData {
    size: u32,
    interface_class_guid: Guid,
    flags: u32,
    reserved: usize,
}

#[repr(C)]
struct DeviceInfoData {
    size: u32,
    class_guid: Guid,
    device_instance: u32,
    reserved: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;
    fn DeviceIoControl(
        device: Handle,
        control_code: u32,
        input: *mut c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> Bool;
    fn FindFirstVolumeW(volume_name: *mut u16, buffer_length: u32) -> Handle;
    fn FindNextVolumeW(find_handle: Handle, volume_name: *mut u16, buffer_length: u32) -> Bool;
    fn FindVolumeClose(find_handle: Handle) -> Bool;
    fn GetVolumePathNameW(
        file_name: *const u16,
        volume_path_name: *mut u16,
        buffer_length: u32,
    ) -> Bool;
    fn GetVolumePathNamesForVolumeNameW(
        volume_name: *const u16,
        volume_path_names: *mut u16,
        buffer_length: u32,
        return_length: *mut u32,
    ) -> Bool;
    fn CloseHandle(handle: Handle) -> Bool;
    fn GetCurrentProcess() -> Handle;
    fn GetLastError() -> u32;
    fn LocalFree(memory: Handle) -> Handle;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        output_buffer_size: u32,
        input_buffer_size: u32,
        default_timeout: u32,
        security_attributes: *mut SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(pipe: Handle, overlapped: *mut c_void) -> Bool;
    fn GetNamedPipeClientProcessId(pipe: Handle, client_process_id: *mut u32) -> Bool;
    fn SetNamedPipeHandleState(
        pipe: Handle,
        mode: *mut u32,
        maximum_collection_count: *mut u32,
        collection_data_timeout: *mut u32,
    ) -> Bool;
    fn WaitNamedPipeW(name: *const u16, timeout: u32) -> Bool;
    fn GetProcessId(process: Handle) -> u32;
    fn IsNativeVhdBoot(is_native_vhd_boot: *mut Bool) -> Bool;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(
        information_class: u32,
        information: *mut c_void,
        information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> Bool;
    fn GetTokenInformation(
        token: Handle,
        information_class: u32,
        information: *mut c_void,
        information_length: u32,
        return_length: *mut u32,
    ) -> Bool;
    fn ConvertSidToStringSidW(sid: *mut c_void, string_sid: *mut *mut u16) -> Bool;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        string_sd_revision: u32,
        security_descriptor: *mut *mut c_void,
        security_descriptor_size: *mut u32,
    ) -> Bool;
    fn RegGetValueW(
        key: Handle,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        value_type: *mut u32,
        data: *mut c_void,
        data_size: *mut u32,
    ) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfo) -> Bool;
}

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(
        class_guid: *const Guid,
        enumerator: *const u16,
        parent_window: Handle,
        flags: u32,
    ) -> Handle;
    fn SetupDiEnumDeviceInterfaces(
        device_info_set: Handle,
        device_info_data: *mut DeviceInfoData,
        interface_class_guid: *const Guid,
        member_index: u32,
        device_interface_data: *mut DeviceInterfaceData,
    ) -> Bool;
    fn SetupDiGetDeviceInterfaceDetailW(
        device_info_set: Handle,
        device_interface_data: *mut DeviceInterfaceData,
        detail_data: *mut c_void,
        detail_data_size: u32,
        required_size: *mut u32,
        device_info_data: *mut DeviceInfoData,
    ) -> Bool;
    fn SetupDiGetDeviceInstanceIdW(
        device_info_set: Handle,
        device_info_data: *mut DeviceInfoData,
        instance_id: *mut u16,
        instance_id_size: u32,
        required_size: *mut u32,
    ) -> Bool;
    fn SetupDiDestroyDeviceInfoList(device_info_set: Handle) -> Bool;
}

#[link(name = "virtdisk")]
unsafe extern "system" {
    fn OpenVirtualDisk(
        storage_type: *const VirtualStorageType,
        path: *const u16,
        access_mask: u32,
        flags: u32,
        parameters: *mut OpenVirtualDiskParameters,
        handle: *mut Handle,
    ) -> u32;
    fn AttachVirtualDisk(
        handle: Handle,
        security_descriptor: *mut c_void,
        flags: u32,
        provider_specific_flags: u32,
        parameters: *mut AttachVirtualDiskParameters,
        overlapped: *mut c_void,
    ) -> u32;
    fn DetachVirtualDisk(handle: Handle, flags: u32, provider_specific_flags: u32) -> u32;
    fn GetVirtualDiskPhysicalPath(
        handle: Handle,
        disk_path_size_in_bytes: *mut u32,
        disk_path: *mut u16,
    ) -> u32;
    #[cfg(test)]
    fn CreateVirtualDisk(
        storage_type: *const VirtualStorageType,
        path: *const u16,
        access_mask: u32,
        security_descriptor: *mut c_void,
        flags: u32,
        provider_specific_flags: u32,
        parameters: *mut CreateVirtualDiskParametersV1,
        overlapped: *mut c_void,
        handle: *mut Handle,
    ) -> u32;
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestVirtualDisk {
    handle: Handle,
    pub disk_number: u32,
    pub capacity_bytes: u64,
}

#[cfg(test)]
impl TestVirtualDisk {
    pub fn create(path: &Path, capacity_bytes: u64) -> Result<Self, OperationError> {
        const VIRTUAL_DISK_ACCESS_ALL: u32 = 0x003F_0000;
        const VIRTUAL_STORAGE_TYPE_DEVICE_VHDX: u32 = 3;

        if !path.is_absolute()
            || path
                .extension()
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("vhdx"))
            || path.exists()
            || capacity_bytes < 64 * 1024 * 1024
            || !capacity_bytes.is_multiple_of(512)
        {
            return Err(OperationError::new(
                ErrorCode::UnsafeTarget,
                "The disposable test VHDX path or capacity is invalid",
            ));
        }
        let storage_type = VirtualStorageType {
            device_id: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
            vendor_id: Guid {
                data1: 0xEC98_4AEC,
                data2: 0xA0F9,
                data3: 0x47E9,
                data4: [0x90, 0x1F, 0x71, 0x41, 0x5A, 0x66, 0x34, 0x5B],
            },
        };
        let mut parameters = CreateVirtualDiskParametersV1 {
            version: 1,
            union_alignment_padding: 0,
            unique_id: Guid {
                data1: 0,
                data2: 0,
                data3: 0,
                data4: [0; 8],
            },
            maximum_size: capacity_bytes,
            block_size_in_bytes: 0,
            sector_size_in_bytes: 512,
            parent_path: ptr::null(),
            source_path: ptr::null(),
        };
        let path = wide_null(path.to_str().ok_or_else(|| {
            OperationError::new(
                ErrorCode::Internal,
                "The VHDX test path is not valid Unicode",
            )
        })?);
        let mut handle = ptr::null_mut();
        let status = unsafe {
            CreateVirtualDisk(
                &storage_type,
                path.as_ptr(),
                VIRTUAL_DISK_ACCESS_ALL,
                ptr::null_mut(),
                0,
                0,
                &mut parameters,
                ptr::null_mut(),
                &mut handle,
            )
        };
        if status != 0 {
            return Err(win32_status(
                ErrorCode::Io,
                "Could not create disposable test VHDX",
                status,
            ));
        }
        let mut disk = Self {
            handle,
            disk_number: u32::MAX,
            capacity_bytes,
        };
        let mut attach_parameters = AttachVirtualDiskParameters {
            version: 1,
            reserved: 0,
        };
        let status = unsafe {
            AttachVirtualDisk(
                disk.handle,
                ptr::null_mut(),
                0,
                0,
                &mut attach_parameters,
                ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(win32_status(
                ErrorCode::Io,
                "Could not attach disposable test VHDX",
                status,
            ));
        }
        disk.disk_number = virtual_disk_number(disk.handle)?;
        Ok(disk)
    }
}

#[cfg(test)]
impl Drop for TestVirtualDisk {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                DetachVirtualDisk(self.handle, 0, 0);
                CloseHandle(self.handle);
            }
        }
    }
}

pub struct MountedIso {
    handle: Handle,
    pub root: PathBuf,
}

impl MountedIso {
    pub fn attach(path: &Path) -> Result<Self, OperationError> {
        let path_str = path.to_str().ok_or_else(|| {
            OperationError::new(ErrorCode::InvalidImage, "The ISO path is not valid Unicode")
        })?;
        let storage_type = VirtualStorageType {
            device_id: 1,
            vendor_id: Guid {
                data1: 0xEC98_4AEC,
                data2: 0xA0F9,
                data3: 0x47E9,
                data4: [0x90, 0x1F, 0x71, 0x41, 0x5A, 0x66, 0x34, 0x5B],
            },
        };
        let mut open_parameters = OpenVirtualDiskParameters {
            version: 1,
            rw_depth: 1,
        };
        let mut handle = ptr::null_mut();
        let path_wide = wide_null(path_str);
        let status = unsafe {
            OpenVirtualDisk(
                &storage_type,
                path_wide.as_ptr(),
                VIRTUAL_DISK_ACCESS_ATTACH_RO | VIRTUAL_DISK_ACCESS_GET_INFO,
                0,
                &mut open_parameters,
                &mut handle,
            )
        };
        if status != 0 {
            return Err(win32_status(
                ErrorCode::InvalidImage,
                "Could not open ISO",
                status,
            ));
        }
        let mut mounted = Self {
            handle,
            root: PathBuf::new(),
        };
        let mut attach_parameters = AttachVirtualDiskParameters {
            version: 1,
            reserved: 0,
        };
        let status = unsafe {
            AttachVirtualDisk(
                mounted.handle,
                ptr::null_mut(),
                ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY,
                0,
                &mut attach_parameters,
                ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(win32_status(
                ErrorCode::InvalidImage,
                "Could not attach ISO",
                status,
            ));
        }
        let device = virtual_disk_device(mounted.handle)?;
        mounted.root = wait_for_iso_mount_path(device, Duration::from_secs(15))?;
        Ok(mounted)
    }
}

impl Drop for MountedIso {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                DetachVirtualDisk(self.handle, 0, 0);
                CloseHandle(self.handle);
            }
        }
    }
}

pub struct NamedPipeServer {
    file: Option<File>,
}

impl NamedPipeServer {
    pub fn create(name: &str) -> Result<Self, OperationError> {
        if !name.starts_with(r"\\.\pipe\rustfus-") {
            return Err(OperationError::new(
                ErrorCode::Internal,
                "Rustfus pipe names must use the private rustfus namespace",
            ));
        }
        let security_descriptor = current_user_pipe_security_descriptor()?;
        let mut security_attributes = SecurityAttributes {
            length: mem::size_of::<SecurityAttributes>() as u32,
            security_descriptor: security_descriptor.0,
            inherit_handle: 0,
        };
        let name = wide_null(name);
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                1024 * 1024,
                1024 * 1024,
                30_000,
                &mut security_attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io_error(io::Error::last_os_error()));
        }
        Ok(Self {
            file: Some(unsafe { File::from_raw_handle(handle as RawHandle) }),
        })
    }

    pub fn accept(mut self, expected_process_id: u32) -> Result<File, OperationError> {
        let file = self
            .file
            .take()
            .ok_or_else(|| OperationError::new(ErrorCode::Internal, "Pipe was already accepted"))?;
        let started = Instant::now();
        loop {
            let connected = unsafe {
                ConnectNamedPipe(file.as_raw_handle().cast(), ptr::null_mut())
            };
            if connected != 0 {
                break;
            }
            let error = unsafe { GetLastError() };
            if error == ERROR_PIPE_CONNECTED {
                break;
            }
            if error != ERROR_PIPE_LISTENING {
                return Err(io_error(io::Error::from_raw_os_error(error as i32)));
            }
            if started.elapsed() >= WORKER_CONNECT_TIMEOUT {
                return Err(OperationError::new(
                    ErrorCode::Io,
                    "The elevated worker did not connect within 30 seconds",
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
        let mut blocking_mode = PIPE_WAIT;
        let blocking = unsafe {
            SetNamedPipeHandleState(
                file.as_raw_handle().cast(),
                &mut blocking_mode,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if blocking == 0 {
            return Err(io_error(io::Error::last_os_error()));
        }
        let mut client_process_id = 0_u32;
        let success = unsafe {
            GetNamedPipeClientProcessId(file.as_raw_handle().cast(), &mut client_process_id)
        };
        if success == 0 {
            return Err(io_error(io::Error::last_os_error()));
        }
        if client_process_id != expected_process_id {
            return Err(OperationError::new(
                ErrorCode::AccessDenied,
                "The named-pipe client is not the elevated worker process",
            ));
        }
        Ok(file)
    }
}

pub fn connect_named_pipe(name: &str) -> Result<File, OperationError> {
    if !name.starts_with(r"\\.\pipe\rustfus-") {
        return Err(OperationError::new(
            ErrorCode::AccessDenied,
            "Worker received an invalid named-pipe path",
        ));
    }
    let name_wide = wide_null(name);
    let waited = unsafe { WaitNamedPipeW(name_wide.as_ptr(), 30_000) };
    if waited == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    open_handle_with_share(name, GENERIC_READ | GENERIC_WRITE, 0).map_err(io_error)
}

pub fn launch_worker(
    worker_path: &Path,
    pipe_name: &str,
    token: &str,
) -> Result<u32, OperationError> {
    let worker = wide_null(worker_path.to_str().ok_or_else(|| {
        OperationError::new(ErrorCode::Internal, "Worker path is not valid Unicode")
    })?);
    // Rustfus.exe requests requireAdministrator in its manifest. Use the normal executable verb
    // so the worker inherits that token without presenting a second elevation prompt.
    let verb = wide_null("open");
    let parameters = wide_null(&format!(
        "--pipe {} --token {}",
        quote_argument(pipe_name),
        quote_argument(token)
    ));
    let mut info = ShellExecuteInfo {
        size: mem::size_of::<ShellExecuteInfo>() as u32,
        mask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        window: ptr::null_mut(),
        verb: verb.as_ptr(),
        file: worker.as_ptr(),
        parameters: parameters.as_ptr(),
        directory: ptr::null(),
        show: SW_HIDE,
        instance: ptr::null_mut(),
        id_list: ptr::null_mut(),
        class: ptr::null(),
        class_key: ptr::null_mut(),
        hot_key: 0,
        icon_or_monitor: ptr::null_mut(),
        process: ptr::null_mut(),
    };
    let success = unsafe { ShellExecuteExW(&mut info) };
    if success == 0 || info.process.is_null() {
        return Err(io_error(io::Error::last_os_error()));
    }
    let process_id = unsafe { GetProcessId(info.process) };
    unsafe {
        CloseHandle(info.process);
    }
    if process_id == 0 {
        Err(io_error(io::Error::last_os_error()))
    } else {
        Ok(process_id)
    }
}

pub fn enumerate_usb_devices() -> Result<Vec<DeviceSnapshot>, OperationError> {
    let system_disks = query_system_disks()?;
    let pagefile_disks = configured_pagefile_disks()?;
    let crashdump_disks = configured_crashdump_disks()?;
    let instance_ids = disk_instance_ids()?;
    let mut devices = Vec::new();
    let mut disk_numbers = instance_ids.keys().copied().collect::<Vec<_>>();
    disk_numbers.sort_unstable();
    for disk_number in disk_numbers {
        let path = format!(r"\\.\PhysicalDrive{disk_number}");
        // Discovery runs in the non-elevated UI. A zero-access DASD handle is
        // sufficient for property/geometry IOCTLs and does not grant raw reads.
        let Ok(file) = open_handle(&path, 0) else {
            continue;
        };
        let Ok(descriptor) = query_storage_descriptor(&file) else {
            continue;
        };
        if descriptor.bus_type != BUS_TYPE_USB {
            continue;
        }
        let Ok((capacity_bytes, logical_sector_size)) = query_geometry(&file) else {
            continue;
        };
        let physical_sector_size = query_access_alignment(&file)
            .ok()
            .map(|(_, physical)| physical)
            .filter(|physical| physical.is_power_of_two() && *physical >= logical_sector_size)
            .unwrap_or(logical_sector_size);
        let model = [descriptor.vendor.as_deref(), descriptor.product.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let instance_id = instance_ids.get(&disk_number).cloned().ok_or_else(|| {
            OperationError::new(
                ErrorCode::UnsafeTarget,
                format!("Could not resolve the PnP instance ID for USB disk {disk_number}"),
            )
        })?;
        let snapshot = DeviceSnapshot {
            instance_id,
            serial: descriptor.serial,
            model: if model.is_empty() {
                "USB storage device".into()
            } else {
                model
            },
            bus: BusType::Usb,
            disk_number,
            capacity_bytes,
            logical_sector_size,
            physical_sector_size,
            volumes: volume_paths_for_disk(disk_number).unwrap_or_default(),
            contains_system: system_disks.system.contains(&disk_number),
            contains_boot: system_disks.boot.contains(&disk_number),
            contains_pagefile: pagefile_disks.contains(&disk_number),
            contains_crashdump: crashdump_disks.contains(&disk_number),
        };
        devices.push(snapshot);
    }
    Ok(devices)
}

/// Disks holding the running Windows installation, from native queries only (no WMI). Every
/// query that fails is reported to `system_disks::combine`, which stops discovery when a fact
/// cannot be established.
fn query_system_disks() -> Result<SystemDisks, OperationError> {
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let boot = disk_set(&format!(r"\\.\{}", system_drive.trim_end_matches('\\')));
    let registry_partition = registry_strings(SETUP_KEY, "SystemPartition")
        .map_err(|error| error.to_string())
        .and_then(|values| {
            values
                .into_iter()
                .next()
                .ok_or_else(|| "the value is missing".to_owned())
        })
        .and_then(|nt_path| nt_volume_disks(&nt_path));
    let kernel_partition = kernel_system_partition().and_then(|nt_path| nt_volume_disks(&nt_path));
    system_disks::combine(Inputs {
        boot,
        registry_partition,
        kernel_partition,
        native_vhd_boot: native_vhd_boot(),
    })
}

/// Disks that a volume (or volume device path) has extents on. Opened with zero access, which is
/// enough for the extent IOCTL and grants no data access.
fn disk_set(path: &str) -> Result<HashSet<u32>, String> {
    let file = open_handle(path, 0).map_err(|error| format!("{path}: {error}"))?;
    disk_numbers_for_volume(&file)
        .map(|numbers| numbers.into_iter().collect())
        .map_err(|error| format!("{path}: {error}"))
}

fn nt_volume_disks(nt_path: &str) -> Result<HashSet<u32>, String> {
    if !nt_path.starts_with(r"\Device\") {
        return Err(format!("unexpected device path {nt_path:?}"));
    }
    disk_set(&format!(r"\\.\GLOBALROOT{nt_path}"))
}

fn native_vhd_boot() -> Result<bool, String> {
    let mut flag: Bool = 0;
    if unsafe { IsNativeVhdBoot(&mut flag) } == 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(flag != 0)
}

/// The kernel's own answer for the system partition, e.g. `\Device\HarddiskVolume1`.
fn kernel_system_partition() -> Result<String, String> {
    let mut buffer = vec![0_u8; 1024];
    let mut returned = 0_u32;
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_SYSTEM_PARTITION_INFORMATION,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut returned,
        )
    };
    if status < 0 {
        return Err(format!("NTSTATUS {status:#010x}"));
    }
    // UNICODE_STRING { u16 length_in_bytes; u16 capacity; <4 bytes padding>; *mut u16 buffer },
    // with the string itself stored inside our output buffer by the kernel.
    let length = usize::from(u16::from_le_bytes([buffer[0], buffer[1]]));
    let pointer = usize::from_le_bytes(
        buffer[8..16]
            .try_into()
            .map_err(|_| "short output buffer".to_owned())?,
    );
    let base = buffer.as_ptr() as usize;
    if length % 2 != 0 || pointer < base || pointer - base + length > buffer.len() {
        return Err("the partition string is outside the output buffer".into());
    }
    let start = pointer - base;
    let units: Vec<u16> = buffer[start..start + length]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

pub fn open_physical_disk(disk_number: u32, write: bool) -> Result<File, OperationError> {
    let access = if write {
        GENERIC_READ | GENERIC_WRITE
    } else {
        GENERIC_READ
    };
    open_handle(&format!(r"\\.\PhysicalDrive{disk_number}"), access).map_err(io_error)
}

pub fn open_source_image(path: &std::path::Path) -> Result<File, OperationError> {
    let path = path.to_str().ok_or_else(|| {
        OperationError::new(
            ErrorCode::InvalidImage,
            "The image path is not valid Unicode",
        )
    })?;
    open_handle_with_share(path, GENERIC_READ, FILE_SHARE_READ).map_err(io_error)
}

pub fn path_is_on_disk(path: &std::path::Path, disk_number: u32) -> Result<bool, OperationError> {
    let path = path.to_str().ok_or_else(|| {
        OperationError::new(
            ErrorCode::InvalidImage,
            "The image path is not valid Unicode",
        )
    })?;
    let path = wide_null(path);
    let mut root = vec![0_u16; 1024];
    let success =
        unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if success == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let root = from_wide_buffer(&root);
    let device_path = format!(r"\\.\{}", root.trim_end_matches('\\'));
    let volume = open_handle(&device_path, GENERIC_READ).map_err(io_error)?;
    let numbers = disk_numbers_for_volume(&volume).map_err(io_error)?;
    Ok(numbers.contains(&disk_number))
}

pub fn update_disk_properties(file: &File) -> Result<(), OperationError> {
    device_io_no_buffer(file, IOCTL_DISK_UPDATE_PROPERTIES).map_err(io_error)
}

pub fn flush_volume(root: &Path) -> Result<(), OperationError> {
    let root = root.to_str().ok_or_else(|| {
        OperationError::new(ErrorCode::Io, "The target volume path is not valid Unicode")
    })?;
    let bytes = root.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return Err(OperationError::new(
            ErrorCode::Io,
            "Windows did not provide a drive-letter target for volume flush",
        ));
    }
    let volume_path = format!(r"\\.\{}:", bytes[0] as char);
    let volume = open_handle(&volume_path, GENERIC_READ | GENERIC_WRITE).map_err(io_error)?;
    volume.sync_all().map_err(io_error)
}

pub fn wait_for_volume_mount_path(
    disk_number: u32,
    timeout: Duration,
) -> Result<PathBuf, OperationError> {
    let started = Instant::now();
    loop {
        if let Ok(volumes) = volume_paths_for_disk(disk_number) {
            for volume in volumes {
                if let Ok(paths) = mount_paths_for_volume(&volume) {
                    if let Some(path) = paths.into_iter().find(|path| path.is_dir()) {
                        return Ok(path);
                    }
                }
            }
        }
        if started.elapsed() >= timeout {
            return Err(OperationError::new(
                ErrorCode::Io,
                "Windows did not mount the volume before the timeout",
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub struct VolumeLocks {
    _handles: Vec<File>,
}

pub fn lock_target_volumes(disk_number: u32) -> Result<VolumeLocks, OperationError> {
    let paths = volume_paths_for_disk(disk_number).map_err(io_error)?;
    let mut handles = Vec::with_capacity(paths.len());
    for path in &paths {
        let open_path = path.trim_end_matches('\\');
        let file = open_handle(open_path, GENERIC_READ | GENERIC_WRITE).map_err(|error| {
            OperationError::new(
                ErrorCode::LockFailed,
                format!("Could not open target volume {path}: {error}"),
            )
        })?;
        device_io_no_buffer(&file, FSCTL_LOCK_VOLUME).map_err(|error| {
            OperationError::new(
                ErrorCode::LockFailed,
                format!("Could not lock target volume {path}: {error}"),
            )
        })?;
        handles.push(file);
    }

    // Do not dismount anything unless every volume on the target is locked.
    // Dropping this collection releases all locks when any lock attempt fails.
    for (path, file) in paths.iter().zip(&handles) {
        device_io_no_buffer(file, FSCTL_DISMOUNT_VOLUME).map_err(|error| {
            OperationError::new(
                ErrorCode::LockFailed,
                format!("Could not dismount target volume {path}: {error}"),
            )
        })?;
    }
    Ok(VolumeLocks { _handles: handles })
}

struct StorageDescriptor {
    vendor: Option<String>,
    product: Option<String>,
    serial: Option<String>,
    bus_type: u32,
}

fn query_storage_descriptor(file: &File) -> Result<StorageDescriptor, io::Error> {
    let mut query = [0_u8; 12];
    let mut output = vec![0_u8; 4096];
    let bytes_returned = device_io(file, IOCTL_STORAGE_QUERY_PROPERTY, &mut query, &mut output)?;
    if bytes_returned < 36 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short storage descriptor",
        ));
    }
    Ok(StorageDescriptor {
        vendor: descriptor_string(&output, read_u32(&output, 12)),
        product: descriptor_string(&output, read_u32(&output, 16)),
        serial: descriptor_string(&output, read_u32(&output, 24)),
        bus_type: read_u32(&output, 28),
    })
}

fn query_geometry(file: &File) -> Result<(u64, u32), io::Error> {
    let mut output = vec![0_u8; 256];
    let bytes_returned = device_io(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &mut [], &mut output)?;
    if bytes_returned < 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short disk geometry",
        ));
    }
    let logical_sector_size = read_u32(&output, 20);
    let capacity = u64::from_le_bytes(output[24..32].try_into().unwrap());
    Ok((capacity, logical_sector_size))
}

fn query_access_alignment(file: &File) -> Result<(u32, u32), io::Error> {
    let mut query = [0_u8; 12];
    query[..4].copy_from_slice(&STORAGE_ACCESS_ALIGNMENT_PROPERTY.to_le_bytes());
    let mut output = vec![0_u8; 28];
    let bytes_returned = device_io(file, IOCTL_STORAGE_QUERY_PROPERTY, &mut query, &mut output)?;
    if bytes_returned < 28 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short storage access alignment descriptor",
        ));
    }
    let logical = read_u32(&output, 16);
    let physical = read_u32(&output, 20);
    if logical == 0 || physical == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "storage access alignment descriptor has a zero sector size",
        ));
    }
    Ok((logical, physical))
}

fn disk_instance_ids() -> Result<HashMap<u32, String>, OperationError> {
    const DISK_INTERFACE: Guid = Guid {
        data1: 0x53F5_6307,
        data2: 0xB6BF,
        data3: 0x11D0,
        data4: [0x94, 0xF2, 0x00, 0xA0, 0xC9, 0x1E, 0xFB, 0x8B],
    };
    let set = unsafe {
        SetupDiGetClassDevsW(
            &DISK_INTERFACE,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE {
        return Err(io_error(io::Error::last_os_error()));
    }
    let _guard = DeviceInfoSetGuard(set);
    let mut result = HashMap::new();
    let mut index = 0_u32;
    loop {
        let mut interface = DeviceInterfaceData {
            size: mem::size_of::<DeviceInterfaceData>() as u32,
            interface_class_guid: DISK_INTERFACE,
            flags: 0,
            reserved: 0,
        };
        let success = unsafe {
            SetupDiEnumDeviceInterfaces(
                set,
                ptr::null_mut(),
                &DISK_INTERFACE,
                index,
                &mut interface,
            )
        };
        if success == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(io_error(io::Error::from_raw_os_error(error as i32)));
        }
        index += 1;

        let mut required = 0_u32;
        let mut device_info = DeviceInfoData {
            size: mem::size_of::<DeviceInfoData>() as u32,
            class_guid: Guid {
                data1: 0,
                data2: 0,
                data3: 0,
                data4: [0; 8],
            },
            device_instance: 0,
            reserved: 0,
        };
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut interface,
                ptr::null_mut(),
                0,
                &mut required,
                ptr::null_mut(),
            );
        }
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || required < 8 {
            return Err(io_error(io::Error::last_os_error()));
        }
        let mut detail = vec![0_u16; (required as usize).div_ceil(mem::size_of::<u16>())];
        let detail_header_size = if cfg!(target_pointer_width = "64") {
            8
        } else {
            6
        };
        detail[0] = detail_header_size;
        let success = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut interface,
                detail.as_mut_ptr().cast(),
                required,
                &mut required,
                &mut device_info,
            )
        };
        if success == 0 {
            return Err(io_error(io::Error::last_os_error()));
        }
        let device_path = unsafe {
            let path = detail.as_ptr().add(2);
            let maximum = detail.len() - 2;
            let slice = std::slice::from_raw_parts(path, maximum);
            let length = slice
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(maximum);
            String::from_utf16_lossy(&slice[..length])
        };
        let Ok(device) = open_handle(&device_path, 0) else {
            continue;
        };
        let mut number = [0_u8; 12];
        let returned = device_io(
            &device,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            &mut [],
            &mut number,
        )
        .map_err(io_error)?;
        if returned < number.len() as u32 {
            return Err(OperationError::new(
                ErrorCode::Io,
                "A disk interface returned a short device-number descriptor",
            ));
        }
        let disk_number = read_u32(&number, 4);
        let instance_id = device_instance_id(set, &mut device_info)?;
        result.entry(disk_number).or_insert(instance_id);
    }
    Ok(result)
}

fn device_instance_id(
    set: Handle,
    device_info: &mut DeviceInfoData,
) -> Result<String, OperationError> {
    let mut required = 0_u32;
    unsafe {
        SetupDiGetDeviceInstanceIdW(set, device_info, ptr::null_mut(), 0, &mut required);
    }
    if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || required < 2 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let mut instance_id = vec![0_u16; required as usize];
    let success = unsafe {
        SetupDiGetDeviceInstanceIdW(
            set,
            device_info,
            instance_id.as_mut_ptr(),
            required,
            &mut required,
        )
    };
    if success == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let length = instance_id
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(instance_id.len());
    Ok(String::from_utf16_lossy(&instance_id[..length]))
}

fn configured_pagefile_disks() -> Result<HashSet<u32>, OperationError> {
    let mut paths = registry_strings(MEMORY_MANAGEMENT_KEY, "ExistingPageFiles")?;
    paths.extend(registry_strings(MEMORY_MANAGEMENT_KEY, "PagingFiles")?);
    configured_path_disks(paths.into_iter().filter_map(|value| {
        let path = configuration_path(&value)?;
        (!path.starts_with("?:")).then_some(path)
    }))
}

fn configured_crashdump_disks() -> Result<HashSet<u32>, OperationError> {
    let paths = ["DumpFile", "MinidumpDir", "DedicatedDumpFile"]
        .into_iter()
        .map(|name| registry_strings(CRASH_CONTROL_KEY, name))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .filter_map(|value| configuration_path(&value));
    configured_path_disks(paths)
}

fn configured_path_disks(
    paths: impl IntoIterator<Item = String>,
) -> Result<HashSet<u32>, OperationError> {
    let mut disks = HashSet::new();
    for path in paths {
        if let Some(numbers) = disk_numbers_for_configured_path(&path).map_err(io_error)? {
            disks.extend(numbers);
        }
    }
    Ok(disks)
}

fn disk_numbers_for_configured_path(path: &str) -> Result<Option<Vec<u32>>, io::Error> {
    let normalized = path
        .strip_prefix(r"\??\")
        .or_else(|| path.strip_prefix(r"\\?\"))
        .unwrap_or(path);
    let bytes = normalized.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return Ok(None);
    }
    let volume_path = format!(r"\\.\{}:", bytes[0] as char);
    let volume = open_handle(&volume_path, 0)?;
    disk_numbers_for_volume(&volume).map(Some)
}

fn configuration_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(quoted) = value.strip_prefix('"') {
        let end = quoted.find('"')?;
        Some(quoted[..end].to_owned())
    } else {
        value.split_whitespace().next().map(str::to_owned)
    }
}

fn registry_strings(subkey: &str, name: &str) -> Result<Vec<String>, OperationError> {
    let subkey = wide_null(subkey);
    let name = wide_null(name);
    let mut value_type = 0_u32;
    let mut size = 0_u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_ANY,
            &mut value_type,
            ptr::null_mut(),
            &mut size,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(Vec::new());
    }
    if status != 0 {
        return Err(registry_error(subkey.as_slice(), name.as_slice(), status));
    }
    if !matches!(value_type, REG_SZ | REG_EXPAND_SZ | REG_MULTI_SZ) || size == 0 {
        return Ok(Vec::new());
    }
    let mut data = vec![0_u16; (size as usize).div_ceil(mem::size_of::<u16>())];
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_ANY,
            &mut value_type,
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return Err(registry_error(subkey.as_slice(), name.as_slice(), status));
    }
    let used = (size as usize / mem::size_of::<u16>()).min(data.len());
    let data = &data[..used];
    let values = if value_type == REG_MULTI_SZ {
        data.split(|character| *character == 0)
            .filter(|value| !value.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    } else {
        let end = data
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(data.len());
        vec![String::from_utf16_lossy(&data[..end])]
    };
    Ok(values)
}

fn registry_error(subkey: &[u16], name: &[u16], status: i32) -> OperationError {
    let subkey = String::from_utf16_lossy(subkey)
        .trim_end_matches('\0')
        .to_owned();
    let name = String::from_utf16_lossy(name)
        .trim_end_matches('\0')
        .to_owned();
    OperationError::new(
        ErrorCode::AccessDenied,
        format!("Could not read HKLM\\{subkey}\\{name}"),
    )
    .with_platform_code(status as u32)
}

/// Volumes that are not backed by a physical disk (optical drives, network or RAM volumes)
/// answer the extent IOCTL with one of these errors. Any other failure is not ignorable: it could
/// belong to a volume of the target disk, so the scan must stop rather than skip it.
fn volume_has_no_physical_disk(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)
    )
}

/// Calls `visit` with the `\\?\Volume{...}\` name of every volume on the machine.
fn for_each_volume(mut visit: impl FnMut(&str) -> Result<(), io::Error>) -> Result<(), io::Error> {
    let mut buffer = vec![0_u16; 1024];
    let find_handle = unsafe { FindFirstVolumeW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if find_handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let guard = FindVolumeGuard(find_handle);
    loop {
        let path = from_wide_buffer(&buffer);
        if !path.is_empty() {
            visit(&path)?;
        }
        buffer.fill(0);
        let has_next =
            unsafe { FindNextVolumeW(guard.0, buffer.as_mut_ptr(), buffer.len() as u32) };
        if has_next == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                return Ok(());
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }
}

fn volume_paths_for_disk(disk_number: u32) -> Result<Vec<String>, io::Error> {
    let mut paths = Vec::new();
    for_each_volume(|path| {
        let file = open_handle(path.trim_end_matches('\\'), 0)?;
        match disk_numbers_for_volume(&file) {
            Ok(disks) => {
                if disks.contains(&disk_number) {
                    paths.push(path.to_owned());
                }
            }
            // Not on a physical disk, so it cannot belong to the target.
            Err(error) if volume_has_no_physical_disk(&error) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    })?;
    Ok(paths)
}

/// Volumes of the CD-ROM device `\\.\CDROM<device_number>`, which is how Windows exposes a mounted
/// ISO. Best effort: a volume that cannot be queried cannot be the ISO's volume, so it is skipped
/// (unlike `volume_paths_for_disk`, nothing safety-relevant depends on this answer).
fn volume_paths_for_cdrom(device_number: u32) -> Vec<String> {
    let mut paths = Vec::new();
    let _ = for_each_volume(|path| {
        if let Ok(file) = open_handle(path.trim_end_matches('\\'), 0) {
            let mut number = [0_u8; 12];
            let described = device_io(&file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &mut [], &mut number)
                .is_ok_and(|returned| returned as usize >= number.len());
            if described
                && read_u32(&number, 0) == FILE_DEVICE_CD_ROM
                && read_u32(&number, 4) == device_number
            {
                paths.push(path.to_owned());
            }
        }
        Ok(())
    });
    paths
}

fn mount_paths_for_volume(volume_name: &str) -> Result<Vec<PathBuf>, io::Error> {
    let volume_name = wide_null(volume_name);
    let mut paths = vec![0_u16; 32_768];
    let mut required = 0_u32;
    let success = unsafe {
        GetVolumePathNamesForVolumeNameW(
            volume_name.as_ptr(),
            paths.as_mut_ptr(),
            paths.len() as u32,
            &mut required,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    let used = usize::try_from(required)
        .unwrap_or(paths.len())
        .min(paths.len());
    let mut result = Vec::new();
    let mut start = 0;
    while start < used {
        let Some(length) = paths[start..used].iter().position(|value| *value == 0) else {
            break;
        };
        if length == 0 {
            break;
        }
        result.push(PathBuf::from(String::from_utf16_lossy(
            &paths[start..start + length],
        )));
        start += length + 1;
    }
    Ok(result)
}

/// How Windows exposed an attached virtual disk: a mounted VHD(X) is a physical drive, a mounted
/// ISO is a CD-ROM device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachedDevice {
    PhysicalDrive(u32),
    CdRom(u32),
}

/// Parses the path reported by `GetVirtualDiskPhysicalPath`, e.g. `\\.\PhysicalDrive3` or
/// `\\.\CDROM0`.
fn parse_attached_device(path: &str) -> Option<AttachedDevice> {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('\\').next()?;
    if let Some(number) = name.strip_prefix("physicaldrive") {
        number.parse().ok().map(AttachedDevice::PhysicalDrive)
    } else if let Some(number) = name.strip_prefix("cdrom") {
        number.parse().ok().map(AttachedDevice::CdRom)
    } else {
        None
    }
}

fn virtual_disk_device(handle: Handle) -> Result<AttachedDevice, OperationError> {
    let mut path = vec![0_u16; 1024];
    let mut size_bytes = (path.len() * mem::size_of::<u16>()) as u32;
    let status = unsafe { GetVirtualDiskPhysicalPath(handle, &mut size_bytes, path.as_mut_ptr()) };
    if status != 0 {
        return Err(win32_status(
            ErrorCode::InvalidImage,
            "Could not resolve attached ISO disk",
            status,
        ));
    }
    let path = from_wide_buffer(&path);
    parse_attached_device(&path).ok_or_else(|| {
        OperationError::new(
            ErrorCode::InvalidImage,
            format!("Unexpected virtual disk path: {path}"),
        )
    })
}

/// The physical drive number of an attached VHD(X), which Windows exposes as `PhysicalDriveN`.
#[cfg(test)]
fn virtual_disk_number(handle: Handle) -> Result<u32, OperationError> {
    match virtual_disk_device(handle)? {
        AttachedDevice::PhysicalDrive(number) => Ok(number),
        AttachedDevice::CdRom(number) => Err(OperationError::new(
            ErrorCode::InvalidImage,
            format!("Expected a physical drive but Windows attached CD-ROM {number}"),
        )),
    }
}

/// Waits for Windows to assign a mount path to the attached image.
fn wait_for_iso_mount_path(
    device: AttachedDevice,
    timeout: Duration,
) -> Result<PathBuf, OperationError> {
    match device {
        AttachedDevice::PhysicalDrive(number) => wait_for_volume_mount_path(number, timeout),
        AttachedDevice::CdRom(number) => {
            let started = Instant::now();
            loop {
                for volume in volume_paths_for_cdrom(number) {
                    if let Ok(paths) = mount_paths_for_volume(&volume) {
                        if let Some(path) = paths.into_iter().find(|path| path.is_dir()) {
                            return Ok(path);
                        }
                    }
                }
                if started.elapsed() >= timeout {
                    return Err(OperationError::new(
                        ErrorCode::Io,
                        "Windows did not mount the ISO volume before the timeout",
                    ));
                }
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn disk_numbers_for_volume(file: &File) -> Result<Vec<u32>, io::Error> {
    let mut output = vec![0_u8; 64 * 1024];
    device_io(
        file,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        &mut [],
        &mut output,
    )?;
    let count = read_u32(&output, 0) as usize;
    let mut numbers = Vec::with_capacity(count);
    for index in 0..count {
        let offset = 8 + index * 24;
        if offset + 4 > output.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short volume extent list",
            ));
        }
        numbers.push(read_u32(&output, offset));
    }
    Ok(numbers)
}

fn device_io_no_buffer(file: &File, control_code: u32) -> Result<(), io::Error> {
    device_io(file, control_code, &mut [], &mut []).map(|_| ())
}

fn device_io(
    file: &File,
    control_code: u32,
    input: &mut [u8],
    output: &mut [u8],
) -> Result<u32, io::Error> {
    use std::os::windows::io::AsRawHandle;
    let mut bytes_returned = 0_u32;
    let success = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            control_code,
            input.as_mut_ptr().cast(),
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut bytes_returned,
            ptr::null_mut(),
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(bytes_returned)
    }
}

fn open_handle(path: &str, access: u32) -> Result<File, io::Error> {
    open_handle_with_share(path, access, FILE_SHARE_READ | FILE_SHARE_WRITE)
}

fn open_handle_with_share(path: &str, access: u32, share_mode: u32) -> Result<File, io::Error> {
    let path = wide_null(path);
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            share_mode,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
    }
}

fn descriptor_string(buffer: &[u8], offset: u32) -> Option<String> {
    let offset = offset as usize;
    if offset == 0 || offset >= buffer.len() {
        return None;
    }
    let end = buffer[offset..]
        .iter()
        .position(|byte| *byte == 0)
        .map_or(buffer.len(), |relative| offset + relative);
    let value = String::from_utf8_lossy(&buffer[offset..end])
        .trim()
        .to_owned();
    (!value.is_empty()).then_some(value)
}

fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        buffer[offset..offset + mem::size_of::<u32>()]
            .try_into()
            .unwrap(),
    )
}

fn wide_null(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn from_wide_buffer(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

fn current_user_pipe_security_descriptor() -> Result<LocalMemory, OperationError> {
    let mut token = ptr::null_mut();
    let success = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if success == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let token = KernelHandle(token);
    let mut required = 0_u32;
    unsafe {
        GetTokenInformation(token.0, TOKEN_USER, ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let word_count = usize::try_from(required)
        .unwrap_or(0)
        .div_ceil(mem::size_of::<usize>());
    let mut information = vec![0_usize; word_count];
    let success = unsafe {
        GetTokenInformation(
            token.0,
            TOKEN_USER,
            information.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    };
    if success == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let sid = unsafe { *information.as_ptr().cast::<*mut c_void>() };
    let mut sid_string = ptr::null_mut();
    let success = unsafe { ConvertSidToStringSidW(sid, &mut sid_string) };
    if success == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    let sid_string = LocalWideString(sid_string);
    let sid = unsafe { wide_ptr_to_string(sid_string.0) };
    let sddl = wide_null(&format!("D:P(A;;GA;;;{sid})"));
    let mut descriptor = ptr::null_mut();
    let success = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if success == 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    Ok(LocalMemory(descriptor))
}

fn quote_argument(argument: &str) -> String {
    format!("\"{}\"", argument.replace('"', "\\\""))
}

unsafe fn wide_ptr_to_string(pointer: *const u16) -> String {
    let mut length = 0_usize;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
}

fn io_error(error: io::Error) -> OperationError {
    let code = if error.kind() == io::ErrorKind::PermissionDenied {
        ErrorCode::AccessDenied
    } else {
        ErrorCode::Io
    };
    let mut result = OperationError::new(code, error.to_string());
    if let Some(platform_code) = error.raw_os_error() {
        result = result.with_platform_code(platform_code as u32);
    }
    result
}

fn win32_status(code: ErrorCode, context: &str, status: u32) -> OperationError {
    OperationError::new(
        code,
        format!("{context}: {}", io::Error::from_raw_os_error(status as i32)),
    )
    .with_platform_code(status)
}

struct DeviceInfoSetGuard(Handle);

impl Drop for DeviceInfoSetGuard {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

struct FindVolumeGuard(Handle);

impl Drop for FindVolumeGuard {
    fn drop(&mut self) {
        unsafe {
            FindVolumeClose(self.0);
        }
    }
}

struct KernelHandle(Handle);

impl Drop for KernelHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

struct LocalWideString(*mut u16);

impl Drop for LocalWideString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0.cast());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_queries_identify_the_running_windows_disks() {
        let disks = query_system_disks().unwrap();
        assert!(
            !disks.boot.is_empty() && !disks.system.is_empty(),
            "Windows did not identify the boot and system disks: {disks:?}"
        );
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let boot = disk_set(&format!(r"\\.\{}", system_drive.trim_end_matches('\\'))).unwrap();
        assert!(boot.iter().all(|disk| disks.boot.contains(disk)));
    }

    #[test]
    fn volumes_without_a_physical_disk_do_not_hide_the_real_ones() {
        // Optical drives, network and RAM volumes cannot report disk extents. They must be
        // skipped, not abort the scan: every disk that holds Windows has at least one volume.
        let disks = query_system_disks().unwrap();
        for disk in disks.boot {
            let volumes = volume_paths_for_disk(disk).unwrap();
            assert!(!volumes.is_empty(), "no volumes found for disk {disk}");
        }
    }

    #[test]
    fn attached_device_paths_are_classified() {
        for (path, expected) in [
            (
                r"\\.\PhysicalDrive3",
                Some(AttachedDevice::PhysicalDrive(3)),
            ),
            (
                r"\\.\physicaldrive12",
                Some(AttachedDevice::PhysicalDrive(12)),
            ),
            (r"\\.\CDROM0", Some(AttachedDevice::CdRom(0))),
            (r"\\.\CdRom7", Some(AttachedDevice::CdRom(7))),
            (r"\\.\Harddisk1Partition1", None),
            (r"\\.\CDROM", None),
            (r"\\.\PhysicalDriveX", None),
            ("", None),
        ] {
            assert_eq!(parse_attached_device(path), expected, "{path}");
        }
    }

    #[test]
    fn the_optical_drive_volume_is_found_by_device_number() {
        // Environment-dependent: only meaningful where an optical volume exists, so it checks the
        // lookup against the volume's own device number instead of assuming any particular drive.
        let mut found = 0;
        for_each_volume(|path| {
            if let Ok(file) = open_handle(path.trim_end_matches('\\'), 0) {
                let mut number = [0_u8; 12];
                let ok = device_io(&file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &mut [], &mut number);
                if ok.is_ok() && read_u32(&number, 0) == FILE_DEVICE_CD_ROM {
                    let device = read_u32(&number, 4);
                    assert!(volume_paths_for_cdrom(device).iter().any(|v| v == path));
                    found += 1;
                }
            }
            Ok(())
        })
        .unwrap();
        // A nonexistent CD-ROM device number matches nothing.
        assert_eq!(volume_paths_for_cdrom(u32::MAX), Vec::<String>::new());
        eprintln!("optical volumes checked: {found}");
    }

    #[test]
    fn only_extent_less_volume_errors_are_ignorable() {
        for code in [ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED] {
            assert!(volume_has_no_physical_disk(&io::Error::from_raw_os_error(
                code
            )));
        }
        // Access denied, I/O errors, missing devices and the like must still stop the scan.
        for code in [2, 5, 21, 23, 31, 87, 1117] {
            assert!(
                !volume_has_no_physical_disk(&io::Error::from_raw_os_error(code)),
                "error {code} must not be ignored"
            );
        }
    }

    #[test]
    fn kernel_and_registry_agree_on_the_system_partition() {
        let kernel = kernel_system_partition().unwrap();
        assert!(kernel.starts_with(r"\Device\"), "{kernel}");
        let registry = registry_strings(SETUP_KEY, "SystemPartition").unwrap();
        assert_eq!(registry.first(), Some(&kernel));
    }
}
