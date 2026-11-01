//! Independent MBR and FAT32 layout/formatting primitives.
//!
//! No boot code is embedded. Microsoft `bootsect.exe` from the user-supplied,
//! signature-validated Windows ISO installs boot code after formatting.

use std::io::{self, Seek, SeekFrom, Write};

const MBR_SIZE: usize = 512;
const PARTITION_ENTRY_OFFSET: usize = 446;
const FAT32_MIN_CLUSTERS: u64 = 65_525;
const FAT32_MAX_CLUSTERS: u64 = 0x0FFF_FFF5;
const RESERVED_SECTORS: u16 = 32;
const FAT_COUNT: u8 = 2;
const ROOT_CLUSTER: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MbrPartition {
    pub start_lba: u32,
    pub sector_count: u32,
    pub active: bool,
    pub partition_type: u8,
}

impl MbrPartition {
    #[must_use]
    pub fn fat32_lba(start_lba: u32, sector_count: u32) -> Self {
        Self {
            start_lba,
            sector_count,
            active: true,
            partition_type: 0x0C,
        }
    }
}

/// Writes an MBR partition table without embedding third-party boot code.
///
/// # Errors
///
/// Returns an error when the partition is empty/starts at LBA zero, or when
/// seeking, writing, or flushing the target fails.
pub fn write_mbr<W: Write + Seek>(
    target: &mut W,
    disk_signature: u32,
    partition: MbrPartition,
) -> Result<(), DiskError> {
    if partition.start_lba == 0 || partition.sector_count == 0 {
        return Err(DiskError::InvalidLayout(
            "the partition must start after the MBR and contain at least one sector",
        ));
    }

    let mut sector = [0_u8; MBR_SIZE];
    sector[440..444].copy_from_slice(&disk_signature.to_le_bytes());
    let entry = &mut sector[PARTITION_ENTRY_OFFSET..PARTITION_ENTRY_OFFSET + 16];
    entry[0] = u8::from(partition.active) * 0x80;
    entry[1..4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    entry[4] = partition.partition_type;
    entry[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    entry[8..12].copy_from_slice(&partition.start_lba.to_le_bytes());
    entry[12..16].copy_from_slice(&partition.sector_count.to_le_bytes());
    sector[510..512].copy_from_slice(&[0x55, 0xAA]);

    target.seek(SeekFrom::Start(0))?;
    target.write_all(&sector)?;
    target.flush()?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fat32Layout {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub sectors_per_fat: u32,
    pub total_sectors: u32,
    pub cluster_count: u32,
    pub root_cluster: u32,
    pub hidden_sectors: u32,
}

impl Fat32Layout {
    /// Calculates a FAT32 layout compatible with the supplied logical sector size.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported sector sizes, unaligned or oversized
    /// partitions, and media that cannot hold a valid FAT32 cluster count.
    pub fn calculate(
        partition_bytes: u64,
        bytes_per_sector: u16,
        hidden_sectors: u32,
    ) -> Result<Self, DiskError> {
        if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
            return Err(DiskError::InvalidSectorSize(bytes_per_sector));
        }
        if partition_bytes % u64::from(bytes_per_sector) != 0 {
            return Err(DiskError::InvalidLayout(
                "partition size is not a multiple of its logical sector size",
            ));
        }
        let total_sectors_u64 = partition_bytes / u64::from(bytes_per_sector);
        let total_sectors = u32::try_from(total_sectors_u64).map_err(|_| {
            DiskError::InvalidLayout("FAT32/MBR media may not exceed u32::MAX sectors")
        })?;

        let target_cluster_bytes = match partition_bytes {
            0..=8_589_934_592 => 4 * 1024,
            8_589_934_593..=17_179_869_184 => 8 * 1024,
            17_179_869_185..=34_359_738_368 => 16 * 1024,
            _ => 32 * 1024,
        };
        let sectors_per_cluster_u32 = target_cluster_bytes / u32::from(bytes_per_sector);
        if sectors_per_cluster_u32 == 0
            || sectors_per_cluster_u32 > 128
            || !sectors_per_cluster_u32.is_power_of_two()
        {
            return Err(DiskError::InvalidLayout(
                "could not select a compatible FAT32 cluster size",
            ));
        }
        let sectors_per_cluster = u8::try_from(sectors_per_cluster_u32)
            .map_err(|_| DiskError::InvalidLayout("cluster size exceeds FAT32 limits"))?;

        let mut sectors_per_fat = 1_u64;
        let (cluster_count, stable_fat_sectors) = loop {
            let metadata_sectors =
                u64::from(RESERVED_SECTORS) + u64::from(FAT_COUNT) * sectors_per_fat;
            if metadata_sectors >= total_sectors_u64 {
                return Err(DiskError::InvalidLayout(
                    "partition is too small for FAT32 metadata",
                ));
            }
            let data_sectors = total_sectors_u64 - metadata_sectors;
            let clusters = data_sectors / u64::from(sectors_per_cluster);
            let fat_bytes = (clusters + 2) * 4;
            let required_fat_sectors = fat_bytes.div_ceil(u64::from(bytes_per_sector));
            if required_fat_sectors == sectors_per_fat {
                break (clusters, sectors_per_fat);
            }
            sectors_per_fat = required_fat_sectors;
        };

        if !(FAT32_MIN_CLUSTERS..=FAT32_MAX_CLUSTERS).contains(&cluster_count) {
            return Err(DiskError::InvalidClusterCount(cluster_count));
        }

        Ok(Self {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors: RESERVED_SECTORS,
            fat_count: FAT_COUNT,
            sectors_per_fat: u32::try_from(stable_fat_sectors)
                .map_err(|_| DiskError::InvalidLayout("FAT is too large"))?,
            total_sectors,
            cluster_count: u32::try_from(cluster_count)
                .map_err(|_| DiskError::InvalidLayout("cluster count is too large"))?,
            root_cluster: ROOT_CLUSTER,
            hidden_sectors,
        })
    }

    #[must_use]
    pub fn cluster_size(&self) -> u32 {
        u32::from(self.bytes_per_sector) * u32::from(self.sectors_per_cluster)
    }

    #[must_use]
    pub fn first_data_sector(&self) -> u64 {
        u64::from(self.reserved_sectors)
            + u64::from(self.fat_count) * u64::from(self.sectors_per_fat)
    }
}

/// Quick-formats an empty FAT32 filesystem at the layout's partition offset.
///
/// # Errors
///
/// Returns an error for an invalid label or when target I/O fails.
pub fn format_empty_fat32<W: Write + Seek>(
    target: &mut W,
    layout: &Fat32Layout,
    volume_id: u32,
    volume_label: &str,
) -> Result<(), DiskError> {
    let label = encode_label(volume_label)?;
    let sector_size = usize::from(layout.bytes_per_sector);
    let partition_offset = u64::from(layout.hidden_sectors) * u64::from(layout.bytes_per_sector);

    zero_region(
        target,
        partition_offset,
        u64::from(layout.reserved_sectors) * u64::from(layout.bytes_per_sector),
    )?;

    let boot_sector = make_boot_sector(layout, volume_id, &label);
    write_sector(target, partition_offset, &boot_sector)?;
    let fs_info = make_fs_info(layout);
    write_sector(
        target,
        partition_offset + u64::from(layout.bytes_per_sector),
        &fs_info,
    )?;
    write_sector(
        target,
        partition_offset + 6 * u64::from(layout.bytes_per_sector),
        &boot_sector,
    )?;
    write_sector(
        target,
        partition_offset + 7 * u64::from(layout.bytes_per_sector),
        &fs_info,
    )?;

    let fat_bytes = u64::from(layout.sectors_per_fat) * u64::from(layout.bytes_per_sector);
    let first_fat_offset =
        partition_offset + u64::from(layout.reserved_sectors) * u64::from(layout.bytes_per_sector);
    for fat_index in 0..layout.fat_count {
        let offset = first_fat_offset + u64::from(fat_index) * fat_bytes;
        zero_region(target, offset, fat_bytes)?;
        let mut first_sector = vec![0_u8; sector_size];
        first_sector[0..4].copy_from_slice(&0x0FFF_FFF8_u32.to_le_bytes());
        first_sector[4..8].copy_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
        first_sector[8..12].copy_from_slice(&0x0FFF_FFFF_u32.to_le_bytes());
        write_sector(target, offset, &first_sector)?;
    }

    let root_offset = partition_offset
        + (layout.first_data_sector()
            + u64::from(layout.root_cluster - 2) * u64::from(layout.sectors_per_cluster))
            * u64::from(layout.bytes_per_sector);
    let root_size = u64::from(layout.cluster_size());
    zero_region(target, root_offset, root_size)?;
    let mut label_entry = [0_u8; 32];
    label_entry[..11].copy_from_slice(&label);
    label_entry[11] = 0x08;
    // A raw disk only accepts whole-sector transfers, so write the whole first sector of the
    // root directory (the region was just zeroed, so the rest of the sector stays empty).
    let mut root_sector = vec![0_u8; sector_size];
    root_sector[..label_entry.len()].copy_from_slice(&label_entry);
    write_sector(target, root_offset, &root_sector)?;
    target.flush()?;
    Ok(())
}

fn make_boot_sector(layout: &Fat32Layout, volume_id: u32, label: &[u8; 11]) -> Vec<u8> {
    let mut sector = vec![0_u8; usize::from(layout.bytes_per_sector)];
    sector[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    sector[3..11].copy_from_slice(b"RUSTFUS ");
    put_u16(&mut sector, 11, layout.bytes_per_sector);
    sector[13] = layout.sectors_per_cluster;
    put_u16(&mut sector, 14, layout.reserved_sectors);
    sector[16] = layout.fat_count;
    put_u16(&mut sector, 17, 0);
    put_u16(&mut sector, 19, 0);
    sector[21] = 0xF8;
    put_u16(&mut sector, 22, 0);
    put_u16(&mut sector, 24, 63);
    put_u16(&mut sector, 26, 255);
    put_u32(&mut sector, 28, layout.hidden_sectors);
    put_u32(&mut sector, 32, layout.total_sectors);
    put_u32(&mut sector, 36, layout.sectors_per_fat);
    put_u16(&mut sector, 40, 0);
    put_u16(&mut sector, 42, 0);
    put_u32(&mut sector, 44, layout.root_cluster);
    put_u16(&mut sector, 48, 1);
    put_u16(&mut sector, 50, 6);
    sector[64] = 0x80;
    sector[66] = 0x29;
    put_u32(&mut sector, 67, volume_id);
    sector[71..82].copy_from_slice(label);
    sector[82..90].copy_from_slice(b"FAT32   ");
    let end = sector.len();
    sector[end - 2..].copy_from_slice(&[0x55, 0xAA]);
    sector
}

fn make_fs_info(layout: &Fat32Layout) -> Vec<u8> {
    let mut sector = vec![0_u8; usize::from(layout.bytes_per_sector)];
    put_u32(&mut sector, 0, 0x4161_5252);
    put_u32(&mut sector, 484, 0x6141_7272);
    put_u32(&mut sector, 488, layout.cluster_count.saturating_sub(1));
    put_u32(&mut sector, 492, 3);
    let end = sector.len();
    sector[end - 4..end].copy_from_slice(&0xAA55_0000_u32.to_le_bytes());
    sector
}

fn encode_label(label: &str) -> Result<[u8; 11], DiskError> {
    if label.is_empty()
        || label.len() > 11
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ')
    {
        return Err(DiskError::InvalidVolumeLabel);
    }
    let mut encoded = [b' '; 11];
    encoded[..label.len()].copy_from_slice(label.as_bytes());
    Ok(encoded)
}

fn write_sector<W: Write + Seek>(
    target: &mut W,
    offset: u64,
    bytes: &[u8],
) -> Result<(), io::Error> {
    target.seek(SeekFrom::Start(offset))?;
    target.write_all(bytes)
}

fn zero_region<W: Write + Seek>(target: &mut W, offset: u64, length: u64) -> Result<(), io::Error> {
    target.seek(SeekFrom::Start(offset))?;
    let zeros = vec![0_u8; 1024 * 1024];
    let mut remaining = length;
    while remaining > 0 {
        let chunk = usize::try_from(remaining.min(zeros.len() as u64)).unwrap_or(zeros.len());
        target.write_all(&zeros[..chunk])?;
        remaining -= chunk as u64;
    }
    Ok(())
}

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[derive(Debug, thiserror::Error)]
pub enum DiskError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("unsupported logical sector size: {0}")]
    InvalidSectorSize(u16),
    #[error("invalid FAT32 cluster count: {0}")]
    InvalidClusterCount(u64),
    #[error("invalid FAT32 volume label")]
    InvalidVolumeLabel,
    #[error("invalid disk layout: {0}")]
    InvalidLayout(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn mbr_contains_active_fat32_partition_without_embedded_boot_code() {
        let mut image = std::io::Cursor::new(vec![0_u8; 4096]);
        write_mbr(
            &mut image,
            0x1234_5678,
            MbrPartition::fat32_lba(2048, 1_000_000),
        )
        .unwrap();
        let bytes = image.into_inner();
        assert_eq!(&bytes[440..444], &0x1234_5678_u32.to_le_bytes());
        assert_eq!(bytes[446], 0x80);
        assert_eq!(bytes[450], 0x0C);
        assert_eq!(&bytes[510..512], &[0x55, 0xAA]);
        assert!(bytes[..440].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn layout_uses_compatible_cluster_size() {
        let layout = Fat32Layout::calculate(32 * 1024 * 1024 * 1024, 512, 2048).unwrap();
        assert_eq!(layout.cluster_size(), 16 * 1024);
        assert!(u64::from(layout.cluster_count) >= FAT32_MIN_CLUSTERS);
        assert!(u64::from(layout.cluster_count) <= FAT32_MAX_CLUSTERS);
    }

    /// Behaves like a raw disk handle: every transfer must start on a sector boundary and cover
    /// whole sectors, otherwise Windows answers `ERROR_INVALID_PARAMETER` (87).
    struct RawDisk {
        inner: std::io::Cursor<Vec<u8>>,
        sector: u64,
    }

    impl RawDisk {
        fn new(bytes: usize, sector: u64) -> Self {
            Self {
                inner: std::io::Cursor::new(vec![0_u8; bytes]),
                sector,
            }
        }

        fn check(&self, length: usize) -> io::Result<()> {
            if self.inner.position() % self.sector != 0 || length as u64 % self.sector != 0 {
                return Err(io::Error::from_raw_os_error(87));
            }
            Ok(())
        }
    }

    impl Read for RawDisk {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.check(buffer.len())?;
            self.inner.read(buffer)
        }
    }

    impl Write for RawDisk {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.check(buffer.len())?;
            self.inner.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for RawDisk {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn formatter_only_issues_whole_sector_transfers_like_a_raw_disk_requires() {
        for sector in [512_u16, 4096] {
            let mut disk = RawDisk::new(300 * 1024 * 1024, u64::from(sector));
            let start_lba = (1024 * 1024) / u32::from(sector);
            let layout = Fat32Layout::calculate(299 * 1024 * 1024, sector, start_lba).unwrap();
            format_empty_fat32(&mut disk, &layout, 0x1234_ABCD, "RUSTFUS").unwrap();

            // The volume-label entry sits at the start of the root directory.
            let root = (u64::from(layout.hidden_sectors)
                + layout.first_data_sector()
                + u64::from(layout.root_cluster - 2) * u64::from(layout.sectors_per_cluster))
                * u64::from(sector);
            let bytes = disk.inner.into_inner();
            let root = usize::try_from(root).unwrap();
            assert_eq!(&bytes[root..root + 11], b"RUSTFUS    ", "sector {sector}");
            assert_eq!(bytes[root + 11], 0x08, "sector {sector}");
        }
    }

    #[test]
    fn mbr_is_written_as_a_whole_sector_on_a_raw_disk() {
        let mut disk = RawDisk::new(4096, 512);
        write_mbr(
            &mut disk,
            0x1234_5678,
            MbrPartition::fat32_lba(2048, 1_000_000),
        )
        .unwrap();
        assert_eq!(&disk.inner.into_inner()[510..512], &[0x55, 0xAA]);
    }

    #[test]
    fn formatter_writes_primary_and_backup_metadata() {
        let mut media = tempfile::tempfile().unwrap();
        media.set_len(512 * 1024 * 1024).unwrap();
        let layout = Fat32Layout::calculate(511 * 1024 * 1024, 512, 2048).unwrap();
        format_empty_fat32(&mut media, &layout, 0xABCD_1234, "RUSTFUS").unwrap();

        let partition_offset = 2048 * 512;
        media.seek(SeekFrom::Start(partition_offset)).unwrap();
        let mut primary = [0_u8; 512];
        media.read_exact(&mut primary).unwrap();
        media
            .seek(SeekFrom::Start(partition_offset + 6 * 512))
            .unwrap();
        let mut backup = [0_u8; 512];
        media.read_exact(&mut backup).unwrap();
        assert_eq!(primary, backup);
        assert_eq!(&primary[82..90], b"FAT32   ");
        assert_eq!(&primary[510..512], &[0x55, 0xAA]);
    }
}
