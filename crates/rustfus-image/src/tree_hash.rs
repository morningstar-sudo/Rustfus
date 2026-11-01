//! Parallel image fingerprint.
//!
//! The fingerprint is a SHA-256 *tree hash*: the image is cut into fixed-size chunks, every chunk
//! is hashed independently (so all CPU cores can work at once), and the fingerprint is
//! `SHA-256(DOMAIN || length_le64 || chunk_digest_0 || chunk_digest_1 || ...)`. It identifies an
//! image between the UI's analysis and the worker's re-analysis; it is deliberately **not** the
//! plain SHA-256 of the file (that one is still used for read-back verification of copies).
//!
//! The same pass can look for the `x86_64` / ARM64 EFI boot-loader names that a hybrid image must
//! expose, so a hybrid image is read once.

use super::io_error;
use rustfus_core::{ErrorCode, Fingerprint, OperationError};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Chunk size of the tree. Changing it changes every fingerprint.
pub const CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MAX_THREADS: usize = 8;
const DOMAIN: &[u8] = b"rustfus-image-tree-sha256-v1\0";
const X64_MARKER: &[u8] = b"BOOTX64.EFI";
const ARM64_MARKER: &[u8] = b"BOOTAA64.EFI";
/// Bytes read past the end of a chunk so a marker that starts inside it is found even when it
/// ends in the next chunk.
const OVERLAP: usize = ARM64_MARKER.len() - 1;

/// Result of fingerprinting an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageScan {
    pub fingerprint: Fingerprint,
    /// `BOOTX64.EFI` was seen (only meaningful when markers were requested).
    pub has_x64_boot: bool,
    /// `BOOTAA64.EFI` was seen. Not searched for once an x64 loader has been found.
    pub has_arm64_boot: bool,
}

/// Positional reads that do not move a shared cursor.
pub trait ReadAt {
    /// Reads up to `buf.len()` bytes at `offset`; `Ok(0)` means end of data.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl ReadAt for &[u8] {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        let Some(rest) = self.get(start..) else {
            return Ok(0);
        };
        let count = rest.len().min(buf.len());
        buf[..count].copy_from_slice(&rest[..count]);
        Ok(count)
    }
}

impl ReadAt for File {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }

    #[cfg(not(any(unix, windows)))]
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn read_full_at(source: &impl ReadAt, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match source.read_at(&mut buf[filled..], offset + filled as u64) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// Fingerprints `path` on all available cores (at most eight), optionally also looking for the
/// EFI boot-loader names.
///
/// # Errors
///
/// `Io` when the file cannot be read or changes size while it is read.
pub fn fingerprint_image_file(path: &Path, markers: bool) -> Result<ImageScan, OperationError> {
    let length = fs::metadata(path).map_err(io_error)?.len();
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    scan(
        || File::open(path),
        length,
        CHUNK_BYTES,
        markers,
        threads.min(MAX_THREADS),
    )
}

struct WorkerOutput {
    digests: Vec<(usize, [u8; 32])>,
    has_x64: bool,
    has_arm64: bool,
}

struct Shared<'a> {
    length: u64,
    chunk: usize,
    chunk_count: usize,
    markers: bool,
    next: &'a AtomicUsize,
    found_x64: &'a AtomicBool,
    failed: &'a AtomicBool,
}

fn changed() -> OperationError {
    OperationError::new(
        ErrorCode::Io,
        "The image changed while it was being fingerprinted",
    )
}

fn worker<R: ReadAt>(
    open: &(impl Fn() -> io::Result<R> + Sync),
    shared: &Shared<'_>,
) -> Result<WorkerOutput, OperationError> {
    let source = open().map_err(io_error)?;
    let mut buffer = vec![0_u8; shared.chunk + OVERLAP];
    let mut output = WorkerOutput {
        digests: Vec::new(),
        has_x64: false,
        has_arm64: false,
    };
    loop {
        if shared.failed.load(Ordering::Relaxed) {
            break;
        }
        let index = shared.next.fetch_add(1, Ordering::Relaxed);
        if index >= shared.chunk_count {
            break;
        }
        let start = index as u64 * shared.chunk as u64;
        let wanted = usize::try_from((shared.length - start).min(shared.chunk as u64))
            .unwrap_or(shared.chunk);
        let read = read_full_at(&source, &mut buffer[..wanted], start).map_err(io_error)?;
        if read != wanted {
            return Err(changed());
        }
        output
            .digests
            .push((index, Sha256::digest(&buffer[..wanted]).into()));

        if shared.markers && !shared.found_x64.load(Ordering::Relaxed) {
            let tail = usize::try_from((shared.length - start - wanted as u64).min(OVERLAP as u64))
                .unwrap_or(0);
            let extra = read_full_at(
                &source,
                &mut buffer[wanted..wanted + tail],
                start + wanted as u64,
            )
            .map_err(io_error)?;
            let window = &buffer[..wanted + extra];
            if contains_ascii_case_insensitive(window, X64_MARKER) {
                output.has_x64 = true;
                shared.found_x64.store(true, Ordering::Relaxed);
            }
            output.has_arm64 |= contains_ascii_case_insensitive(window, ARM64_MARKER);
        }
    }
    Ok(output)
}

fn scan<R, F>(
    open: F,
    length: u64,
    chunk: usize,
    markers: bool,
    threads: usize,
) -> Result<ImageScan, OperationError>
where
    R: ReadAt,
    F: Fn() -> io::Result<R> + Sync,
{
    let chunk_count = usize::try_from(length.div_ceil(chunk as u64)).map_err(|_| {
        OperationError::new(
            ErrorCode::InvalidImage,
            "The image is too large to fingerprint",
        )
    })?;
    let next = AtomicUsize::new(0);
    let found_x64 = AtomicBool::new(false);
    let failed = AtomicBool::new(false);
    let shared = Shared {
        length,
        chunk,
        chunk_count,
        markers,
        next: &next,
        found_x64: &found_x64,
        failed: &failed,
    };
    let threads = threads.clamp(1, chunk_count.max(1));

    let outputs: Vec<Result<WorkerOutput, OperationError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let result = worker(&open, &shared);
                    if result.is_err() {
                        shared.failed.store(true, Ordering::Relaxed);
                    }
                    result
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    Err(OperationError::new(
                        ErrorCode::Internal,
                        "A fingerprint worker panicked",
                    ))
                })
            })
            .collect()
    });

    let mut digests = vec![[0_u8; 32]; chunk_count];
    let (mut has_x64, mut has_arm64) = (false, false);
    for output in outputs {
        let output = output?;
        for (index, digest) in output.digests {
            digests[index] = digest;
        }
        has_x64 |= output.has_x64;
        has_arm64 |= output.has_arm64;
    }

    let mut root = Sha256::new();
    root.update(DOMAIN);
    root.update(length.to_le_bytes());
    for digest in &digests {
        root.update(digest);
    }
    Ok(ImageScan {
        fingerprint: Fingerprint(root.finalize().into()),
        has_x64_boot: has_x64,
        has_arm64_boot: has_arm64,
    })
}

/// Case-insensitive ASCII substring search that only compares the rest of the needle at
/// positions where the first byte matches.
fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if haystack.len() < needle.len() {
        return false;
    }
    let first = first.to_ascii_lowercase();
    haystack[..=haystack.len() - needle.len()]
        .iter()
        .enumerate()
        .any(|(position, byte)| {
            byte.to_ascii_lowercase() == first
                && haystack[position + 1..position + needle.len()].eq_ignore_ascii_case(rest)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| u8::try_from((index.wrapping_mul(31) ^ (index >> 7)) & 0xFF).unwrap())
            .collect()
    }

    fn scan_bytes(bytes: &[u8], chunk: usize, markers: bool, threads: usize) -> ImageScan {
        scan(|| Ok(bytes), bytes.len() as u64, chunk, markers, threads).unwrap()
    }

    #[test]
    fn fingerprint_follows_the_documented_formula() {
        let bytes = data(1000);
        let chunk = 64;
        let mut root = Sha256::new();
        root.update(DOMAIN);
        root.update(1000_u64.to_le_bytes());
        for piece in bytes.chunks(chunk) {
            root.update(Sha256::digest(piece));
        }
        let expected: [u8; 32] = root.finalize().into();
        assert_eq!(scan_bytes(&bytes, chunk, false, 3).fingerprint.0, expected);
    }

    #[test]
    fn result_does_not_depend_on_the_thread_count() {
        let bytes = data(10_000);
        let reference = scan_bytes(&bytes, 128, true, 1);
        for threads in [2, 3, 5, 16, 1000] {
            assert_eq!(
                scan_bytes(&bytes, 128, true, threads),
                reference,
                "{threads}"
            );
        }
    }

    #[test]
    fn content_and_length_both_change_the_fingerprint() {
        let bytes = data(777);
        let base = scan_bytes(&bytes, 64, false, 2).fingerprint;

        let mut flipped = bytes.clone();
        flipped[500] ^= 1;
        assert_ne!(scan_bytes(&flipped, 64, false, 2).fingerprint, base);

        let mut longer = bytes.clone();
        longer.push(0);
        assert_ne!(scan_bytes(&longer, 64, false, 2).fingerprint, base);

        // Same bytes, different chunking, is a different tree.
        assert_ne!(scan_bytes(&bytes, 128, false, 2).fingerprint, base);
    }

    #[test]
    fn empty_input_has_a_stable_fingerprint() {
        let mut root = Sha256::new();
        root.update(DOMAIN);
        root.update(0_u64.to_le_bytes());
        let expected: [u8; 32] = root.finalize().into();
        let result = scan_bytes(&[], 64, true, 4);
        assert_eq!(result.fingerprint.0, expected);
        assert!(!result.has_x64_boot && !result.has_arm64_boot);
    }

    #[test]
    fn markers_are_found_at_every_offset_including_chunk_boundaries() {
        for marker in [&b"BOOTX64.EFI"[..], b"bootx64.efi", b"BootX64.Efi"] {
            for offset in 0..=(64 - marker.len()) {
                let mut bytes = vec![b'x'; 64];
                bytes[offset..offset + marker.len()].copy_from_slice(marker);
                // 16-byte chunks put many boundaries through the marker.
                for threads in [1, 4] {
                    let found = scan_bytes(&bytes, 16, true, threads);
                    assert!(found.has_x64_boot, "offset {offset} threads {threads}");
                }
            }
        }
    }

    #[test]
    fn arm64_is_reported_only_when_there_is_no_x64_loader() {
        let mut arm = vec![b'x'; 100];
        arm[37..37 + 12].copy_from_slice(b"bootaa64.efi");
        let found = scan_bytes(&arm, 16, true, 3);
        assert!(!found.has_x64_boot && found.has_arm64_boot);

        let mut both = arm.clone();
        both[5..5 + 11].copy_from_slice(b"BOOTX64.EFI");
        assert!(scan_bytes(&both, 16, true, 3).has_x64_boot);

        let neither = vec![b'x'; 100];
        let found = scan_bytes(&neither, 16, true, 3);
        assert!(!found.has_x64_boot && !found.has_arm64_boot);
    }

    #[test]
    fn markers_are_ignored_unless_requested() {
        let mut bytes = vec![b'x'; 64];
        bytes[10..21].copy_from_slice(b"BOOTX64.EFI");
        let found = scan_bytes(&bytes, 16, false, 2);
        assert!(!found.has_x64_boot);
    }

    #[test]
    fn a_marker_can_cross_the_real_chunk_boundary() {
        let mut bytes = vec![b'x'; CHUNK_BYTES + 100];
        bytes[CHUNK_BYTES - 4..CHUNK_BYTES - 4 + 11].copy_from_slice(b"bootx64.efi");
        assert!(scan_bytes(&bytes, CHUNK_BYTES, true, 2).has_x64_boot);
    }

    #[test]
    fn data_shorter_than_the_announced_length_is_an_error() {
        let bytes = data(100);
        let error = scan(|| Ok(&bytes[..]), 200, 64, false, 2).unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
    }

    #[test]
    fn a_failing_open_is_reported() {
        let result = scan(
            || -> io::Result<&[u8]> { Err(io::Error::other("no")) },
            10,
            4,
            false,
            2,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::Io);
    }

    #[test]
    fn files_and_slices_agree() {
        let bytes = data(3 * CHUNK_BYTES + 12_345);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.bin");
        fs::write(&path, &bytes).unwrap();

        let from_file = fingerprint_image_file(&path, true).unwrap();
        let from_slice = scan_bytes(&bytes, CHUNK_BYTES, true, 3);
        assert_eq!(from_file, from_slice);
    }

    #[test]
    fn search_helper_matches_the_naive_definition() {
        let samples: [&[u8]; 6] = [
            b"",
            b"a",
            b"xxBootX64.efixx",
            b"bootx64.ef",
            b"BOOTX64.EFI",
            b"zzz",
        ];
        for haystack in samples {
            for needle in [&b"BOOTX64.EFI"[..], b"x", b""] {
                let naive = needle.is_empty()
                    || haystack
                        .windows(needle.len())
                        .any(|window| window.eq_ignore_ascii_case(needle));
                assert_eq!(
                    contains_ascii_case_insensitive(haystack, needle),
                    naive,
                    "{haystack:?} {needle:?}"
                );
            }
        }
    }
}
