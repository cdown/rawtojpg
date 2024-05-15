use anyhow::{ensure, Context, Result};
use clap::Parser;
use memmap2::{Mmap, MmapOptions};
use nix::fcntl::{posix_fadvise, PosixFadviseAdvice};
use nix::sys::mman::{madvise, MmapAdvise};
use nix::unistd::{sysconf, SysconfVar};
use once_cell::sync::OnceCell;
use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// Input directory containing RAW files
    input_dir: PathBuf,

    /// Output directory to store extracted JPEGs
    #[arg(default_value = ".")]
    output_dir: PathBuf,

    /// How many files to process at once
    #[arg(short, long, default_value_t = 8)]
    transfers: usize,

    /// Look for this extension in addition to the default list.
    ///
    /// Default list: arw, cr2, crw, dng, erf, kdc, mef, mrw, nef, nrw, orf, pef, raf, raw, rw2,
    /// rwl, sr2, srf, srw, x3f
    #[arg(short, long)]
    extension: Option<OsString>,
}

fn align_down(addr: *mut u8, alignment: usize) -> *mut u8 {
    let offset = addr.align_offset(alignment);
    if offset == 0 {
        addr
    } else {
        unsafe { addr.add(offset).sub(alignment) }
    }
}

fn align_up(addr: *mut u8, alignment: usize) -> *mut u8 {
    let offset = addr.align_offset(alignment);
    unsafe { addr.add(offset) }
}

unsafe fn madvise_aligned(addr: *mut u8, length: usize, advice: MmapAdvise) -> Result<()> {
    static PAGE_SIZE: OnceCell<usize> = OnceCell::new();

    let page_size = *PAGE_SIZE.get_or_try_init(|| {
        sysconf(SysconfVar::PAGE_SIZE)
            .context("Failed to get page size")?
            .context("PAGE_SIZE is not available")
            .map(|v| v as usize)
    })?;

    let start = align_down(addr, page_size);
    let end = align_up(addr.add(length), page_size);
    let length = end.offset_from(start);
    let start = NonNull::new(start).context("Aligned address was NULL")?;

    Ok(madvise(start.cast(), length.try_into()?, advice)?)
}

async fn mmap_raw(raw_fd: i32) -> Result<Mmap> {
    // We only access a small part of the file, don't read in more than necessary.
    posix_fadvise(raw_fd, 0, 0, PosixFadviseAdvice::POSIX_FADV_RANDOM)?;

    let raw_buf = unsafe { MmapOptions::new().map(raw_fd)? };

    unsafe {
        madvise_aligned(
            raw_buf.as_ptr() as *mut _,
            raw_buf.len(),
            MmapAdvise::MADV_RANDOM,
        )?;
    }

    Ok(raw_buf)
}

fn extract_jpeg(raw_fd: i32, raw_buf: &[u8]) -> Result<&[u8]> {
    let rule = quickexif::describe_rule!(tiff {
        0x0201 / jpeg_offset
        0x0202 / jpeg_sz
    });
    let exif = quickexif::parse(raw_buf, &rule)?;

    let jpeg_offset = exif.u32("jpeg_offset")? as usize;
    let jpeg_sz = exif.u32("jpeg_sz")? as usize;

    ensure!(
        (jpeg_offset + jpeg_sz) <= raw_buf.len(),
        "JPEG data exceeds file size"
    );

    posix_fadvise(
        raw_fd,
        jpeg_offset as i64,
        jpeg_sz as i64,
        PosixFadviseAdvice::POSIX_FADV_WILLNEED,
    )?;

    unsafe {
        let em_jpeg_ptr = raw_buf.as_ptr().add(jpeg_offset);
        madvise_aligned(em_jpeg_ptr as *mut _, jpeg_sz, MmapAdvise::MADV_WILLNEED)?;
    }

    Ok(&raw_buf[jpeg_offset..jpeg_offset + jpeg_sz])
}

async fn write_jpeg(output_file: &Path, jpeg_buf: &[u8]) -> Result<()> {
    let mut out_file = File::create(&output_file).await?;
    out_file.write_all(jpeg_buf).await?;
    Ok(())
}

async fn process_file(entry_path: &Path, out_dir: &Path, relative_path: &Path) -> Result<()> {
    println!("{}", relative_path.display());
    let in_file = File::open(entry_path).await?;
    let raw_fd = in_file.as_raw_fd();
    let raw_buf = mmap_raw(raw_fd).await?;
    let jpeg_buf = extract_jpeg(raw_fd, &raw_buf)?;
    let mut output_file = out_dir.join(relative_path);
    output_file.set_extension("jpg");
    write_jpeg(&output_file, jpeg_buf).await?;
    Ok(())
}

async fn process_directory(
    in_dir: &Path,
    out_dir: &'static Path,
    ext: Option<OsString>,
    transfers: usize,
) -> Result<()> {
    let valid_extensions = [
        "arw", "cr2", "crw", "dng", "erf", "kdc", "mef", "mrw", "nef", "nrw", "orf", "pef", "raf",
        "raw", "rw2", "rwl", "sr2", "srf", "srw", "x3f",
    ]
    .iter()
    .flat_map(|&ext| [OsString::from(ext), OsString::from(&ext.to_uppercase())])
    .chain(ext.into_iter())
    .collect::<HashSet<_>>();

    let mut entries = Vec::new();
    let mut dir_queue = vec![in_dir.to_path_buf()];

    while let Some(current_dir) = dir_queue.pop() {
        let mut read_dir = fs::read_dir(&current_dir).await?;
        let mut found_raw = false;

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if entry.file_type().await?.is_dir() {
                dir_queue.push(path);
            } else if path
                .extension()
                .map_or(false, |ext| valid_extensions.contains(ext))
            {
                found_raw = true;
                entries.push(path);
            }
        }

        if found_raw {
            let relative_dir = current_dir.strip_prefix(in_dir)?;
            let output_subdir = out_dir.join(relative_dir);
            fs::create_dir_all(&output_subdir).await?;
        }
    }

    let semaphore = Arc::new(Semaphore::new(transfers));
    let mut tasks: Vec<JoinHandle<Result<()>>> = Vec::new();

    for in_path in entries {
        let semaphore = semaphore.clone();
        let out_dir = out_dir.to_path_buf();
        let relative_path = in_path.strip_prefix(in_dir)?.to_path_buf();
        let task = tokio::spawn(async move {
            let permit = semaphore.acquire_owned().await?;
            let result = process_file(&in_path, &out_dir, &relative_path).await;
            drop(permit);
            if let Err(e) = &result {
                eprintln!("Error processing file {}: {:?}", in_path.display(), e);
            }
            result
        });
        tasks.push(task);
    }

    for task in tasks {
        task.await??;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let output_dir = Box::leak(Box::new(args.output_dir)); // It's gonna get used for each raw file and
                                                           // would need a copy for .filter_map(),
                                                           // better to just make it &'static

    fs::create_dir_all(&output_dir).await?;
    process_directory(&args.input_dir, output_dir, args.extension, args.transfers).await?;

    Ok(())
}
