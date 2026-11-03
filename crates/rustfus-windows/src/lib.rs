//! Windows storage integration and the platform-neutral raw streaming engine.

use rustfus_core::{ErrorCode, OperationError, OperationPhase};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

pub const RAW_BLOCK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamProgress {
    pub phase: OperationPhase,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_second: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct RawWriteOptions<'a> {
    pub source_length: u64,
    pub sector_alignment: u32,
    pub verify: bool,
    pub cancellation: &'a CancellationToken,
}

/// Blocks in flight between the I/O thread and its helper thread. Each pool holds one more buffer
/// than this so both sides always have something to work on.
const PIPELINE_DEPTH: usize = 3;

/// One block of the source image on its way to the target.
struct Block {
    buffer: Vec<u8>,
    /// Bytes of source data at the start of `buffer`.
    wanted: usize,
    /// `wanted` rounded up to the sector size; the tail of `buffer[..padded]` is zero padding.
    padded: usize,
}

fn internal(message: &str) -> OperationError {
    OperationError::new(ErrorCode::Internal, message)
}

fn buffer_pool(block_size: usize) -> (mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
    let (free_tx, free_rx) = mpsc::channel();
    for _ in 0..=PIPELINE_DEPTH {
        let _ = free_tx.send(vec![0_u8; block_size]);
    }
    (free_tx, free_rx)
}

/// Writes `source` to `target` in sector-aligned blocks, then (optionally) reads the target back
/// and compares its hash with the hash of the source. Returns the SHA-256 of the source bytes.
///
/// Reading and hashing the source run on a helper thread so they overlap with the (usually much
/// slower) writes; during read-back the hashing overlaps with the reads. Blocks are still written,
/// and errors still surface, strictly in image order.
///
/// # Errors
///
/// `Cancelled` when cancelled, `Io` for read or write failures, `VerificationFailed` when the
/// read-back differs from the source.
pub fn write_raw_image<S, T, D, F>(
    source: &mut S,
    target: &mut T,
    options: RawWriteOptions<'_>,
    mut durable_sync: D,
    mut progress: F,
) -> Result<[u8; 32], OperationError>
where
    S: Read + Seek + Send,
    T: Read + Write + Seek,
    D: FnMut(&mut T) -> Result<(), OperationError>,
    F: FnMut(StreamProgress),
{
    let RawWriteOptions {
        source_length,
        sector_alignment,
        verify,
        cancellation,
    } = options;
    if sector_alignment == 0 || !sector_alignment.is_power_of_two() {
        return Err(OperationError::new(
            ErrorCode::Internal,
            "The target sector alignment is invalid",
        ));
    }
    let sector_size = sector_alignment as usize;
    let block_size = RAW_BLOCK_BYTES.div_ceil(sector_size) * sector_size;

    source.seek(SeekFrom::Start(0)).map_err(io_error)?;
    target.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let expected = copy_source(
        source,
        target,
        source_length,
        block_size,
        sector_size,
        cancellation,
        &mut progress,
    )?;
    target.flush().map_err(io_error)?;
    durable_sync(target)?;

    if verify {
        target.seek(SeekFrom::Start(0)).map_err(io_error)?;
        verify_target(
            target,
            source_length,
            block_size,
            sector_size,
            &expected,
            cancellation,
            &mut progress,
        )?;
    }
    Ok(expected)
}

fn copy_source<S, T, F>(
    source: &mut S,
    target: &mut T,
    source_length: u64,
    block_size: usize,
    sector_size: usize,
    cancellation: &CancellationToken,
    progress: &mut F,
) -> Result<[u8; 32], OperationError>
where
    S: Read + Send,
    T: Write,
    F: FnMut(StreamProgress),
{
    let (free_tx, free_rx) = buffer_pool(block_size);
    let (full_tx, full_rx) = mpsc::sync_channel::<Result<Block, OperationError>>(PIPELINE_DEPTH);
    let started = Instant::now();
    std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            read_and_hash(
                source,
                source_length,
                block_size,
                sector_size,
                &free_rx,
                &full_tx,
            )
        });
        let written = write_blocks(
            target,
            source_length,
            cancellation,
            progress,
            started,
            &full_rx,
            &free_tx,
        );
        // Dropping both ends unblocks the reader if the writer stopped early.
        drop(full_rx);
        drop(free_tx);
        let hash = reader
            .join()
            .map_err(|_| internal("The image reader thread panicked"));
        written?;
        hash
    })
}

/// Helper thread of the write phase: reads the source, hashes exactly the source bytes, pads the
/// last block to the sector size and hands the blocks over in order.
fn read_and_hash<S: Read>(
    source: &mut S,
    source_length: u64,
    block_size: usize,
    sector_size: usize,
    free: &mpsc::Receiver<Vec<u8>>,
    full: &mpsc::SyncSender<Result<Block, OperationError>>,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    let mut done = 0_u64;
    while done < source_length {
        let Ok(mut buffer) = free.recv() else {
            break;
        };
        let wanted =
            usize::try_from((source_length - done).min(block_size as u64)).unwrap_or(block_size);
        if let Err(error) = source.read_exact(&mut buffer[..wanted]) {
            let _ = full.send(Err(io_error(error)));
            break;
        }
        hash.update(&buffer[..wanted]);
        let padded = wanted.div_ceil(sector_size) * sector_size;
        buffer[wanted..padded].fill(0);
        done += wanted as u64;
        if full
            .send(Ok(Block {
                buffer,
                wanted,
                padded,
            }))
            .is_err()
        {
            break;
        }
    }
    hash.finalize().into()
}

fn write_blocks<T: Write, F: FnMut(StreamProgress)>(
    target: &mut T,
    source_length: u64,
    cancellation: &CancellationToken,
    progress: &mut F,
    started: Instant,
    full: &mpsc::Receiver<Result<Block, OperationError>>,
    free: &mpsc::Sender<Vec<u8>>,
) -> Result<(), OperationError> {
    let mut completed = 0_u64;
    while completed < source_length {
        check_cancelled(cancellation, completed > 0)?;
        let block = full
            .recv()
            .map_err(|_| internal("The image reader stopped unexpectedly"))??;
        target
            .write_all(&block.buffer[..block.padded])
            .map_err(io_error)?;
        completed += block.wanted as u64;
        emit_progress(
            progress,
            OperationPhase::Write,
            completed,
            source_length,
            started,
        );
        let _ = free.send(block.buffer);
    }
    Ok(())
}

fn verify_target<T: Read, F: FnMut(StreamProgress)>(
    target: &mut T,
    source_length: u64,
    block_size: usize,
    sector_size: usize,
    expected: &[u8; 32],
    cancellation: &CancellationToken,
    progress: &mut F,
) -> Result<(), OperationError> {
    let (free_tx, free_rx) = buffer_pool(block_size);
    let (full_tx, full_rx) = mpsc::sync_channel::<(Vec<u8>, usize)>(PIPELINE_DEPTH);
    let started = Instant::now();
    let actual = std::thread::scope(|scope| {
        // The hasher is the only sender of recycled buffers, so if it dies the reader's wait for
        // a free buffer ends instead of blocking forever.
        let hasher = scope.spawn(move || {
            let mut hash = Sha256::new();
            while let Ok((buffer, count)) = full_rx.recv() {
                hash.update(&buffer[..count]);
                let _ = free_tx.send(buffer);
            }
            <[u8; 32]>::from(hash.finalize())
        });
        let read = read_back_blocks(
            target,
            &ReadBack {
                source_length,
                sector_size,
                cancellation,
                started,
            },
            progress,
            &free_rx,
            &full_tx,
        );
        drop(full_tx);
        let hash = hasher
            .join()
            .map_err(|_| internal("The verification hasher thread panicked"));
        read?;
        hash
    })?;
    if actual != *expected {
        return Err(OperationError::new(
            ErrorCode::VerificationFailed,
            format!(
                "Read-back verification failed (expected {}, got {})",
                hex::encode(expected),
                hex::encode(actual)
            ),
        ));
    }
    Ok(())
}

/// What the read-back pass needs besides the target and the channels.
struct ReadBack<'a> {
    source_length: u64,
    sector_size: usize,
    cancellation: &'a CancellationToken,
    started: Instant,
}

fn read_back_blocks<T: Read, F: FnMut(StreamProgress)>(
    target: &mut T,
    pass: &ReadBack<'_>,
    progress: &mut F,
    free: &mpsc::Receiver<Vec<u8>>,
    full: &mpsc::SyncSender<(Vec<u8>, usize)>,
) -> Result<(), OperationError> {
    let mut verified = 0_u64;
    while verified < pass.source_length {
        check_cancelled(pass.cancellation, true)?;
        let mut buffer = free
            .recv()
            .map_err(|_| internal("The verification hasher stopped unexpectedly"))?;
        let wanted = usize::try_from((pass.source_length - verified).min(buffer.len() as u64))
            .unwrap_or(buffer.len());
        // A raw disk only serves whole sectors, so read the zero padding written after the last
        // image byte too; only the image bytes are hashed.
        let padded = wanted.div_ceil(pass.sector_size) * pass.sector_size;
        target.read_exact(&mut buffer[..padded]).map_err(io_error)?;
        verified += wanted as u64;
        emit_progress(
            progress,
            OperationPhase::Verify,
            verified,
            pass.source_length,
            pass.started,
        );
        full.send((buffer, wanted))
            .map_err(|_| internal("The verification hasher stopped unexpectedly"))?;
    }
    Ok(())
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

fn emit_progress<F: FnMut(StreamProgress)>(
    callback: &mut F,
    phase: OperationPhase,
    completed_bytes: u64,
    total_bytes: u64,
    started: Instant,
) {
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    callback(StreamProgress {
        phase,
        completed_bytes,
        total_bytes,
        bytes_per_second: completed_bytes as f64 / elapsed,
    });
}

fn io_error(error: std::io::Error) -> OperationError {
    let mut result = OperationError::new(ErrorCode::Io, error.to_string());
    if let Some(code) = error.raw_os_error() {
        result = result.with_platform_code(code as u32);
    }
    result
}

#[cfg(windows)]
mod media;
#[cfg(windows)]
mod platform;
#[cfg(any(windows, test))]
mod system_disks;

#[cfg(windows)]
pub use media::{
    analyze_image, cleanup_stale_temporary_directories, create_ubuntu_installer,
    create_windows_installer,
};
#[cfg(windows)]
pub use platform::{
    NamedPipeServer, connect_named_pipe, enumerate_usb_devices, flush_volume, launch_worker,
    lock_target_volumes, open_physical_disk, open_source_image, path_is_on_disk,
    update_disk_properties,
};

#[cfg(not(windows))]
pub fn analyze_image(
    path: &std::path::Path,
) -> Result<rustfus_core::ImageAnalysis, OperationError> {
    rustfus_image::analyze_hybrid_iso(path)
}

#[cfg(not(windows))]
pub fn enumerate_usb_devices() -> Result<Vec<rustfus_core::DeviceSnapshot>, OperationError> {
    Err(OperationError::new(
        ErrorCode::UnsupportedPlatform,
        "USB device enumeration is available only on Windows",
    ))
}

#[cfg(not(windows))]
pub fn open_source_image(_path: &std::path::Path) -> Result<std::fs::File, OperationError> {
    Err(OperationError::new(
        ErrorCode::UnsupportedPlatform,
        "Exclusive source-image access is available only on Windows",
    ))
}

#[cfg(not(windows))]
pub fn path_is_on_disk(_path: &std::path::Path, _disk_number: u32) -> Result<bool, OperationError> {
    Err(OperationError::new(
        ErrorCode::UnsupportedPlatform,
        "Volume extent inspection is available only on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    struct ShortWriteTarget {
        inner: Cursor<Vec<u8>>,
        remaining: usize,
    }

    impl Read for ShortWriteTarget {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buffer)
        }
    }

    impl Write for ShortWriteTarget {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            let count = buffer.len().min(self.remaining);
            let written = self.inner.write(&buffer[..count])?;
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for ShortWriteTarget {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    struct CorruptingTarget {
        inner: Cursor<Vec<u8>>,
        wrote_data: bool,
        corrupted: bool,
    }

    impl Read for CorruptingTarget {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buffer)
        }
    }

    impl Write for CorruptingTarget {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let written = self.inner.write(buffer)?;
            self.wrote_data |= written > 0;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for CorruptingTarget {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if position == SeekFrom::Start(0) && self.wrote_data && !self.corrupted {
                self.inner.get_mut()[0] ^= 0xFF;
                self.corrupted = true;
            }
            self.inner.seek(position)
        }
    }

    #[test]
    fn raw_writer_pads_sector_but_hashes_only_source() {
        let bytes = vec![0xA5_u8; 1000];
        let mut source = Cursor::new(bytes.clone());
        let mut target = Cursor::new(vec![0_u8; 4096]);
        let mut phases = Vec::new();
        let cancellation = CancellationToken::default();
        let hash = write_raw_image(
            &mut source,
            &mut target,
            RawWriteOptions {
                source_length: bytes.len() as u64,
                sector_alignment: 512,
                verify: true,
                cancellation: &cancellation,
            },
            |_| Ok(()),
            |update| phases.push(update.phase),
        )
        .unwrap();
        assert_eq!(hash, Sha256::digest(&bytes).as_slice());
        assert_eq!(&target.get_ref()[..1000], bytes.as_slice());
        assert!(target.get_ref()[1000..1024].iter().all(|byte| *byte == 0));
        assert!(phases.contains(&OperationPhase::Write));
        assert!(phases.contains(&OperationPhase::Verify));
    }

    #[test]
    fn durable_sync_must_succeed_before_read_back() {
        let bytes = vec![0xA5_u8; 4096];
        let mut source = Cursor::new(bytes);
        let mut target = Cursor::new(vec![0_u8; 4096]);
        let mut phases = Vec::new();
        let error = write_raw_image(
            &mut source,
            &mut target,
            RawWriteOptions {
                source_length: 4096,
                sector_alignment: 512,
                verify: true,
                cancellation: &CancellationToken::default(),
            },
            |_| {
                Err(OperationError::new(
                    ErrorCode::Io,
                    "injected durable-sync failure",
                ))
            },
            |progress| phases.push(progress.phase),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(!phases.contains(&OperationPhase::Verify));
    }

    #[test]
    fn cancellation_is_fail_fast() {
        let token = CancellationToken::default();
        token.cancel();
        let mut source = Cursor::new(vec![1_u8; 512]);
        let mut target = Cursor::new(vec![0_u8; 512]);
        assert_eq!(
            write_raw_image(
                &mut source,
                &mut target,
                RawWriteOptions {
                    source_length: 512,
                    sector_alignment: 512,
                    verify: true,
                    cancellation: &token,
                },
                |_| Ok(()),
                |_| {},
            )
            .unwrap_err()
            .code,
            ErrorCode::Cancelled
        );
    }

    #[test]
    fn cancellation_after_first_chunk_marks_media_incomplete() {
        let token = CancellationToken::default();
        let callback_token = token.clone();
        let bytes = vec![0x4D_u8; RAW_BLOCK_BYTES + 512];
        let mut source = Cursor::new(bytes.clone());
        let mut target = Cursor::new(vec![0_u8; bytes.len()]);
        let error = write_raw_image(
            &mut source,
            &mut target,
            RawWriteOptions {
                source_length: bytes.len() as u64,
                sector_alignment: 512,
                verify: true,
                cancellation: &token,
            },
            |_| Ok(()),
            move |progress| {
                if progress.phase == OperationPhase::Write && progress.completed_bytes > 0 {
                    callback_token.cancel();
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Cancelled);
        assert!(error.message.contains("incomplete"));
    }

    #[test]
    fn short_write_is_reported_as_io_failure() {
        let bytes = vec![0xA7_u8; 4096];
        let mut source = Cursor::new(bytes.clone());
        let mut target = ShortWriteTarget {
            inner: Cursor::new(vec![0_u8; bytes.len()]),
            remaining: 1024,
        };
        let cancellation = CancellationToken::default();
        let error = write_raw_image(
            &mut source,
            &mut target,
            RawWriteOptions {
                source_length: bytes.len() as u64,
                sector_alignment: 512,
                verify: true,
                cancellation: &cancellation,
            },
            |_| Ok(()),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
    }

    struct FailingSource {
        inner: Cursor<Vec<u8>>,
        fail_after: u64,
    }

    impl Read for FailingSource {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let position = self.inner.position();
            if position >= self.fail_after {
                return Err(io::Error::other("injected source failure"));
            }
            let room = usize::try_from(self.fail_after - position).unwrap_or(usize::MAX);
            let limit = buffer.len().min(room);
            self.inner.read(&mut buffer[..limit])
        }
    }

    impl Seek for FailingSource {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    /// Reads back correctly except for one flipped byte at `corrupt_at`.
    struct LateCorruptingTarget {
        inner: Cursor<Vec<u8>>,
        wrote: bool,
        reading_back: bool,
        corrupt_at: u64,
    }

    impl Read for LateCorruptingTarget {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let start = self.inner.position();
            let count = self.inner.read(buffer)?;
            if self.reading_back && (start..start + count as u64).contains(&self.corrupt_at) {
                buffer[usize::try_from(self.corrupt_at - start).unwrap()] ^= 0xFF;
            }
            Ok(count)
        }
    }

    impl Write for LateCorruptingTarget {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.wrote = true;
            self.inner.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for LateCorruptingTarget {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if position == SeekFrom::Start(0) && self.wrote {
                self.reading_back = true;
            }
            self.inner.seek(position)
        }
    }

    /// Like a raw disk handle: every transfer must start on a sector boundary and cover whole
    /// sectors, otherwise Windows answers `ERROR_INVALID_PARAMETER` (87).
    struct RawDiskTarget {
        inner: Cursor<Vec<u8>>,
        sector: u64,
    }

    impl RawDiskTarget {
        fn check(&self, length: usize) -> io::Result<()> {
            if self.inner.position() % self.sector != 0 || length as u64 % self.sector != 0 {
                return Err(io::Error::from_raw_os_error(87));
            }
            Ok(())
        }
    }

    impl Read for RawDiskTarget {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.check(buffer.len())?;
            self.inner.read(buffer)
        }
    }

    impl Write for RawDiskTarget {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.check(buffer.len())?;
            self.inner.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for RawDiskTarget {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn read_back_uses_whole_sectors_so_it_works_on_a_raw_disk() {
        // ISO sizes are multiples of 2048, not necessarily of the device's sector size.
        for (length, sector) in [
            (3000_usize, 512_u32),
            (6144, 4096),
            (2 * RAW_BLOCK_BYTES + 2048, 4096),
        ] {
            let bytes = pattern(length);
            let padded = length.div_ceil(sector as usize) * sector as usize;
            let mut source = Cursor::new(bytes.clone());
            let mut target = RawDiskTarget {
                inner: Cursor::new(vec![0_u8; padded + 8192]),
                sector: u64::from(sector),
            };
            let token = CancellationToken::default();
            let hash = write_raw_image(
                &mut source,
                &mut target,
                RawWriteOptions {
                    source_length: length as u64,
                    sector_alignment: sector,
                    verify: true,
                    cancellation: &token,
                },
                |_| Ok(()),
                |_| {},
            )
            .unwrap_or_else(|error| panic!("length {length} sector {sector}: {error}"));
            assert_eq!(hash, Sha256::digest(&bytes).as_slice());
            assert_eq!(&target.inner.get_ref()[..length], bytes.as_slice());
        }
    }

    fn pattern(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| u8::try_from((index.wrapping_mul(131) ^ (index >> 11)) & 0xFF).unwrap())
            .collect()
    }

    /// Runs `job` on its own thread and fails the test instead of hanging if it deadlocks.
    fn run_with_timeout<R: Send + 'static>(job: impl FnOnce() -> R + Send + 'static) -> R {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(job());
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the write pipeline deadlocked")
    }

    fn options(length: usize, token: &CancellationToken) -> RawWriteOptions<'_> {
        RawWriteOptions {
            source_length: length as u64,
            sector_alignment: 512,
            verify: true,
            cancellation: token,
        }
    }

    #[test]
    fn multi_block_image_round_trips_through_the_pipeline() {
        let bytes = pattern(3 * RAW_BLOCK_BYTES + 777);
        let padded = bytes.len().div_ceil(512) * 512;
        let mut source = Cursor::new(bytes.clone());
        let mut target = Cursor::new(vec![0xEE_u8; padded + 4096]);
        let mut seen = Vec::new();
        let token = CancellationToken::default();
        let hash = write_raw_image(
            &mut source,
            &mut target,
            options(bytes.len(), &token),
            |_| Ok(()),
            |update| seen.push((update.phase, update.completed_bytes)),
        )
        .unwrap();

        assert_eq!(hash, Sha256::digest(&bytes).as_slice());
        let written = target.get_ref();
        assert_eq!(&written[..bytes.len()], bytes.as_slice());
        assert!(written[bytes.len()..padded].iter().all(|byte| *byte == 0));
        assert!(written[padded..].iter().all(|byte| *byte == 0xEE));
        for phase in [OperationPhase::Write, OperationPhase::Verify] {
            let steps: Vec<u64> = seen
                .iter()
                .filter(|(seen_phase, _)| *seen_phase == phase)
                .map(|(_, completed)| *completed)
                .collect();
            assert_eq!(steps.len(), 4, "{phase:?}");
            assert!(steps.windows(2).all(|pair| pair[0] < pair[1]), "{phase:?}");
            assert_eq!(steps.last(), Some(&(bytes.len() as u64)), "{phase:?}");
        }
    }

    #[test]
    fn source_failure_is_reported_after_the_earlier_blocks_were_written() {
        let bytes = pattern(3 * RAW_BLOCK_BYTES);
        let (result, written) = run_with_timeout({
            let bytes = bytes.clone();
            move || {
                let mut source = FailingSource {
                    inner: Cursor::new(bytes.clone()),
                    fail_after: RAW_BLOCK_BYTES as u64 + 10,
                };
                let mut target = Cursor::new(vec![0_u8; bytes.len()]);
                let token = CancellationToken::default();
                let result = write_raw_image(
                    &mut source,
                    &mut target,
                    options(bytes.len(), &token),
                    |_| Ok(()),
                    |_| {},
                );
                (result, target.into_inner())
            }
        });
        assert_eq!(result.unwrap_err().code, ErrorCode::Io);
        assert_eq!(&written[..RAW_BLOCK_BYTES], &bytes[..RAW_BLOCK_BYTES]);
        assert!(written[RAW_BLOCK_BYTES..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn writer_failure_stops_the_reader_without_hanging() {
        let error = run_with_timeout(|| {
            let bytes = pattern(8 * RAW_BLOCK_BYTES);
            let mut source = Cursor::new(bytes.clone());
            let mut target = ShortWriteTarget {
                inner: Cursor::new(vec![0_u8; bytes.len()]),
                remaining: RAW_BLOCK_BYTES + 100,
            };
            let token = CancellationToken::default();
            write_raw_image(
                &mut source,
                &mut target,
                options(bytes.len(), &token),
                |_| Ok(()),
                |_| {},
            )
            .unwrap_err()
        });
        assert_eq!(error.code, ErrorCode::Io);
    }

    #[test]
    fn corruption_in_a_late_block_is_detected_on_read_back() {
        let error = run_with_timeout(|| {
            let bytes = pattern(3 * RAW_BLOCK_BYTES);
            let mut source = Cursor::new(bytes.clone());
            let mut target = LateCorruptingTarget {
                inner: Cursor::new(vec![0_u8; bytes.len()]),
                wrote: false,
                reading_back: false,
                corrupt_at: 2 * RAW_BLOCK_BYTES as u64 + 5,
            };
            let token = CancellationToken::default();
            write_raw_image(
                &mut source,
                &mut target,
                options(bytes.len(), &token),
                |_| Ok(()),
                |_| {},
            )
            .unwrap_err()
        });
        assert_eq!(error.code, ErrorCode::VerificationFailed);
    }

    #[test]
    fn cancellation_during_read_back_marks_media_incomplete() {
        let error = run_with_timeout(|| {
            let bytes = pattern(3 * RAW_BLOCK_BYTES);
            let mut source = Cursor::new(bytes.clone());
            let mut target = Cursor::new(vec![0_u8; bytes.len()]);
            let token = CancellationToken::default();
            let callback_token = token.clone();
            write_raw_image(
                &mut source,
                &mut target,
                options(bytes.len(), &token),
                |_| Ok(()),
                move |update| {
                    if update.phase == OperationPhase::Verify && update.completed_bytes > 0 {
                        callback_token.cancel();
                    }
                },
            )
            .unwrap_err()
        });
        assert_eq!(error.code, ErrorCode::Cancelled);
        assert!(error.message.contains("incomplete"));
    }

    #[test]
    fn corrupt_read_back_is_rejected() {
        let bytes = vec![0x3C_u8; 4096];
        let mut source = Cursor::new(bytes.clone());
        let mut target = CorruptingTarget {
            inner: Cursor::new(vec![0_u8; bytes.len()]),
            wrote_data: false,
            corrupted: false,
        };
        let cancellation = CancellationToken::default();
        let error = write_raw_image(
            &mut source,
            &mut target,
            RawWriteOptions {
                source_length: bytes.len() as u64,
                sector_alignment: 512,
                verify: true,
                cancellation: &cancellation,
            },
            |_| Ok(()),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::VerificationFailed);
    }
}
