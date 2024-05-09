use anyhow::{ensure, Context, Result};
use futures::stream::StreamExt;
use memmap2::{Mmap, MmapOptions};
use nix::fcntl::posix_fadvise;
use nix::fcntl::PosixFadviseAdvice;
use nix::sys::mman::{madvise, MmapAdvise};
use nix::unistd::{sysconf, SysconfVar};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use tokio::fs;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReadDirStream;

const fn is_jpeg_soi(buf: &[u8]) -> bool {
    buf[0] == 0xff && buf[1] == 0xd8
}

unsafe fn madvise_aligned(addr: *mut u8, length: usize, advice: MmapAdvise) -> Result<()> {
    let page_size: usize = sysconf(SysconfVar::PAGE_SIZE).unwrap().unwrap() as usize;

    let page_aligned_start = (addr as usize) & !(page_size - 1);

    let original_end = addr as usize + length;
    let page_aligned_end = (original_end + page_size - 1) & !(page_size - 1);

    let aligned_length = page_aligned_end - page_aligned_start;
    let aligned_addr = page_aligned_start as *mut _;
    let aligned_nonnull = NonNull::new(aligned_addr)
        .ok_or_else(|| anyhow::anyhow!("Failed to convert aligned address to NonNull"))?;

    madvise(aligned_nonnull, aligned_length, advice).context("Failed to madvise()")
}

async fn mmap_raw(raw_fd: i32) -> Result<Mmap> {
    // We only access a small part of the file, don't read in more than necessary.
    posix_fadvise(raw_fd, 0, 0, PosixFadviseAdvice::POSIX_FADV_RANDOM).unwrap();

    let raw_buf = unsafe { MmapOptions::new().map(raw_fd).unwrap() };

    let base_length = raw_buf.len();
    unsafe {
        madvise_aligned(
            raw_buf.as_ptr() as *mut _,
            base_length,
            MmapAdvise::MADV_RANDOM,
        )
        .unwrap();
    }

    Ok(raw_buf)
}

fn extract_jpeg(raw_fd: i32, raw_buf: &[u8]) -> Result<&[u8]> {
    let exif = rexif::parse_buffer(raw_buf).context("Failed to parse EXIF data")?;
    let jpeg_offset_tag = 0x0201; // JPEGInterchangeFormat
    let jpeg_length_tag = 0x0202; // JPEGInterchangeFormatLength
    let mut jpeg_offset = None;
    let mut jpeg_sz = None;

    for entry in &exif.entries {
        if entry.ifd.tag == jpeg_offset_tag {
            jpeg_offset = Some(entry.value.to_i64(0).unwrap() as usize);
        } else if entry.ifd.tag == jpeg_length_tag {
            jpeg_sz = Some(entry.value.to_i64(0).unwrap() as usize);
        }
    }

    let jpeg_offset = jpeg_offset.context("Cannot find embedded JPEG")?;
    let jpeg_sz = jpeg_sz.context("Cannot find embedded JPEG")?;

    posix_fadvise(
        raw_fd,
        jpeg_offset as i64,
        jpeg_sz as i64,
        PosixFadviseAdvice::POSIX_FADV_WILLNEED,
    )
    .unwrap();
    unsafe {
        let em_jpeg_ptr = raw_buf.as_ptr().add(jpeg_offset);
        madvise_aligned(em_jpeg_ptr as *mut _, jpeg_sz, MmapAdvise::MADV_WILLNEED).unwrap();
    }

    ensure!(
        (jpeg_offset + jpeg_sz) <= raw_buf.len(),
        "JPEG data exceeds file size"
    );
    ensure!(
        is_jpeg_soi(&raw_buf[jpeg_offset..]),
        "Missing JPEG SOI marker"
    );

    Ok(&raw_buf[jpeg_offset..jpeg_offset + jpeg_sz])
}

async fn write_jpeg(out_dir: &Path, filename: &str, jpeg_buf: &[u8]) -> Result<()> {
    let mut output_file = out_dir.join(filename);
    output_file.set_extension("jpg");
    println!("{filename}");

    let mut out_file = File::create(&output_file)
        .await
        .context("Failed to open output file")?;
    out_file
        .write_all(jpeg_buf)
        .await
        .context("Failed to write to output file")?;
    Ok(())
}

// Determined by anecdotal profiling. When reading from a CFexpress card and writing to NVMe, 8
// is about the right number of files that we don't end up with a lot of contention while still
// making optimal forward progress.
const MAX_OPEN_FILES: usize = 8;

async fn process_file(entry_path: PathBuf, out_dir: &Path) -> Result<()> {
    let filename = entry_path.file_name().unwrap().to_string_lossy();
    let in_file = File::open(&entry_path)
        .await
        .context("Failed to open raw file")?;
    let raw_fd = in_file.as_raw_fd();
    let raw_buf = mmap_raw(raw_fd).await?;
    let jpeg_buf = extract_jpeg(raw_fd, &raw_buf)?;
    write_jpeg(out_dir, &filename, jpeg_buf).await?;
    Ok(())
}

async fn process_directory(in_dir: &Path, out_dir: &'static Path) -> Result<()> {
    let ent = fs::read_dir(in_dir)
        .await
        .context("Failed to open input directory")?;
    let mut ent_stream = ReadDirStream::new(ent);
    let semaphore = Arc::new(Semaphore::new(MAX_OPEN_FILES));

    let mut tasks: Vec<JoinHandle<Result<()>>> = Vec::new();

    while let Some(entry) = ent_stream.next().await {
        match entry {
            Ok(e)
                if e.path().extension().map_or(false, |ext| ext == "raw")
                    && e.metadata().await.unwrap().is_file() =>
            {
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let task = tokio::spawn(async move {
                    let result = process_file(e.path(), out_dir).await;
                    drop(permit);
                    result
                });
                tasks.push(task);
            }
            _ => continue,
        }
    }

    for task in tasks {
        task.await??;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} input_dir [output_dir]", args[0]);
        std::process::exit(1);
    }

    let in_dir = PathBuf::from(&args[1]);
    let output_dir = if args.len() > 2 {
        PathBuf::from(&args[2])
    } else {
        PathBuf::from(".")
    };
    let output_dir = Box::leak(Box::new(output_dir)); // It's gonna get used for each raw file and
                                                      // would need a copy for .filter_map(),
                                                      // better to just make it &'static

    fs::create_dir_all(&output_dir)
        .await
        .context("Failed to create output directory")?;
    process_directory(&in_dir, output_dir).await?;

    Ok(())
}
