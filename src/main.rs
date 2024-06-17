use anyhow::{ensure, Result};
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use memmap2::{Advice, Mmap};
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

/// Map a RAW file into memory using `mmap()`. The file must be static.
fn mmap_raw(file: File) -> Result<Mmap> {
    // SAFETY: mmap in general is unsafe because the lifecycle of the backing bytes are mutable
    // from outside the program.
    //
    // This means that, among other things, I/O errors can abort the program (e.g. by SIGBUS). This
    // is not a big problem, since we are just a command line program and have control over the
    // entire execution lifecycle.
    //
    // Also, any guarantees around validation (like taking a string slice from the &[u8]) are also
    // only enforced at creation time, so it's possible for the underlying file to cause corruption
    // (and thus UB). However, in our case, that's not a problem: we don't rely on such
    // enforcement.
    let raw_buf = unsafe { Mmap::map(file.as_raw_fd())? };

    // Avoid overread into the rest of the RAW, which degrades performance substantially. We will
    // later update the advice for the JPEG section with Advice::WillNeed. Until then, our accesses
    // are essentially random: we walk the IFDs, but these are likely in non-sequential pages.
    raw_buf.advise(Advice::Random)?;
    Ok(raw_buf)
}

/// An embedded JPEG in a RAW file.
#[derive(Default, Eq, PartialEq)]
struct EmbeddedJpegInfo {
    offset: usize,
    length: usize,
}

/// Find the largest embedded JPEG in a memory-mapped RAW buffer.
///
/// This function parses the IFDs in the TIFF structure of the RAW file to find the largest JPEG
/// thumbnail embedded in the file.
///
/// We hand roll the IFD parsing because libraries do not fit requirements. For example:
///
/// - kamadak-exif: Reads into a big `Vec<u8>`, which is huge for our big RAW.
/// - quickexif: Cannot iterate over IFDs.
fn find_largest_embedded_jpeg(raw_buf: &[u8]) -> Result<EmbeddedJpegInfo> {
    const IFD_ENTRY_SIZE: usize = 12;
    const TIFF_MAGIC_LE: &[u8] = b"II*\0";
    const TIFF_MAGIC_BE: &[u8] = b"MM\0*";
    const JPEG_TAG: u16 = 0x201;
    const JPEG_LENGTH_TAG: u16 = 0x202;

    let is_le = &raw_buf[0..4] == TIFF_MAGIC_LE;
    ensure!(
        is_le || &raw_buf[0..4] == TIFF_MAGIC_BE,
        "Not a valid TIFF file"
    );

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

fn extract_jpeg(raw_buf: &Mmap) -> Result<&[u8]> {
    let jpeg = find_largest_embedded_jpeg(raw_buf)?;
    raw_buf.advise_range(Advice::WillNeed, jpeg.offset, jpeg.length)?;
    Ok(&raw_buf[jpeg.offset..jpeg.offset + jpeg.length])
}

async fn write_file(output_file: &Path, buf: &[u8]) -> Result<()> {
    let mut out_file = File::create(output_file).await?;
    out_file.write_all(buf).await?;
    Ok(())
}

/// Process a single RAW file to extract the embedded JPEG, and then write the extracted JPEG to
/// the output directory.
async fn process_file(entry_path: &Path, out_dir: &Path, relative_path: &Path) -> Result<()> {
    let in_file = File::open(entry_path).await?;
    let raw_buf = mmap_raw(in_file)?;
    let jpeg_buf = extract_jpeg(&raw_buf)?;
    let mut output_file = out_dir.join(relative_path);
    output_file.set_extension("jpg");
    write_file(&output_file, jpeg_buf).await?;
    Ok(())
}

/// Recursively process a directory of RAW files, extracting embedded JPEGs and writing them to the
/// output directory.
///
/// This function recursively searches the input directory for RAW files with valid extensions,
/// processes each file to extract the embedded JPEG, and writes the JPEGs to the corresponding
/// location in the output directory. The directory structure relative to the input directory is
/// maintained.
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

    let progress_bar = ProgressBar::new(entries.len().try_into()?);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template("{pos}/{len} [{bar}] (ETA: {eta})")?
            .progress_chars("##-"),
    );

    let semaphore = Arc::new(Semaphore::new(transfers));
    let mut tasks = Vec::new();

    for in_path in entries {
        let semaphore = semaphore.clone();
        let relative_path = in_path.strip_prefix(in_dir)?.to_path_buf();
        let progress_bar = progress_bar.clone();
        let task = tokio::spawn(async move {
            let permit = semaphore.acquire_owned().await?;
            let result = process_file(&in_path, out_dir, &relative_path).await;
            drop(permit);
            progress_bar.inc(1);
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

    progress_bar.finish();

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // We would need a copy for each task otherwise, so better just to make it &'static
    let output_dir = Box::leak(Box::new(args.output_dir));

    fs::create_dir_all(&output_dir).await?;
    process_directory(&args.input_dir, output_dir, args.extension, args.transfers).await?;

    Ok(())
}
