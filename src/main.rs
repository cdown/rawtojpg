use anyhow::{ensure, Result};
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use clap::Parser;
use memmap2::{Advice, Mmap};
use nix::fcntl::{posix_fadvise, PosixFadviseAdvice};
use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

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

async fn mmap_raw(raw_fd: i32) -> Result<Mmap> {
    // We only access a small part of the file, don't read in more than necessary.
    posix_fadvise(raw_fd, 0, 0, PosixFadviseAdvice::POSIX_FADV_RANDOM)?;

    let raw_buf = unsafe { Mmap::map(raw_fd)? };
    raw_buf.advise(Advice::Random)?;

    Ok(raw_buf)
}

#[derive(Default, Eq, PartialEq)]
struct EmbeddedJpegInfo {
    offset: usize,
    length: usize,
}

/// We do this by hand because EXIF libraries don't fit requirements:
///
/// - kamadak-exif: Reads into a big Vec<u8>, which is huge for our big RAW.
/// - quickexif: Cannot iterate over IFDs.
fn find_largest_embedded_jpeg(raw_buf: &[u8]) -> Result<EmbeddedJpegInfo> {
    const IFD_ENTRY_SIZE: usize = 12;
    const TIFF_MAGIC_LE: &[u8] = b"II*\0";
    const TIFF_MAGIC_BE: &[u8] = b"MM\0*";
    const JPEG_TAG: u16 = 0x201;
    const JPEG_LENGTH_TAG: u16 = 0x202;

    ensure!(
        &raw_buf[0..4] == TIFF_MAGIC_LE || &raw_buf[0..4] == TIFF_MAGIC_BE,
        "Not a valid TIFF file"
    );

    let is_le = &raw_buf[0..4] == TIFF_MAGIC_LE;

    let read_u16 = if is_le {
        LittleEndian::read_u16
    } else {
        BigEndian::read_u16
    };

    let read_u32 = if is_le {
        LittleEndian::read_u32
    } else {
        BigEndian::read_u32
    };

    let mut next_ifd_offset = read_u32(&raw_buf[4..8]).try_into()?;
    let mut largest_jpeg = EmbeddedJpegInfo::default();

    while next_ifd_offset != 0 {
        let cursor = &raw_buf[next_ifd_offset..];
        let num_entries = read_u16(&cursor[..2]).into();
        let entries_cursor = &cursor[2..];

        let mut cur_offset = None;
        let mut cur_length = None;

        for entry in entries_cursor
            .chunks_exact(IFD_ENTRY_SIZE)
            .take(num_entries)
        {
            let tag = read_u16(&entry[..2]);

            match tag {
                JPEG_TAG => cur_offset = Some(read_u32(&entry[8..12]).try_into()?),
                JPEG_LENGTH_TAG => cur_length = Some(read_u32(&entry[8..12]).try_into()?),
                _ => {}
            }

            if let (Some(offset), Some(length)) = (cur_offset, cur_length) {
                if length > largest_jpeg.length {
                    largest_jpeg = EmbeddedJpegInfo { offset, length };
                }
                break;
            }
        }

        next_ifd_offset = read_u32(&cursor[2 + num_entries * IFD_ENTRY_SIZE..][..4]).try_into()?;
    }

    ensure!(
        largest_jpeg != EmbeddedJpegInfo::default(),
        "No JPEG data found"
    );
    ensure!(
        largest_jpeg.offset + largest_jpeg.length <= raw_buf.len(),
        "JPEG data exceeds file size"
    );

    Ok(largest_jpeg)
}

fn extract_jpeg(raw_fd: i32, raw_buf: &Mmap) -> Result<&[u8]> {
    let jpeg = find_largest_embedded_jpeg(raw_buf)?;

    posix_fadvise(
        raw_fd,
        jpeg.offset as i64,
        jpeg.length as i64,
        PosixFadviseAdvice::POSIX_FADV_WILLNEED,
    )?;

    raw_buf.advise_range(Advice::WillNeed, jpeg.offset, jpeg.length)?;
    raw_buf.advise_range(Advice::PopulateRead, jpeg.offset, jpeg.length)?;

    Ok(&raw_buf[jpeg.offset..jpeg.offset + jpeg.length])
}

async fn write_jpeg(output_file: &Path, jpeg_buf: &[u8]) -> Result<()> {
    let mut out_file = File::create(output_file).await?;
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
    .flat_map(|&ext| [OsString::from(ext), OsString::from(ext.to_uppercase())])
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
    let mut tasks = Vec::new();

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
