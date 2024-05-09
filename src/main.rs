use anyhow::{ensure, Context, Result};
use futures::stream::StreamExt;
use memmap2::{Mmap, MmapOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
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

async fn mmap_arw(arw_fd: i32) -> Result<Mmap> {
    let arw_buf = unsafe { MmapOptions::new().map(arw_fd).unwrap() };
    Ok(arw_buf)
}

fn extract_jpeg(arw_buf: &[u8]) -> Result<&[u8]> {
    let exif = rexif::parse_buffer(arw_buf).context("Failed to parse EXIF data")?;
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

    ensure!(
        (jpeg_offset + jpeg_sz) <= arw_buf.len(),
        "JPEG data exceeds file size"
    );
    ensure!(
        is_jpeg_soi(&arw_buf[jpeg_offset..]),
        "Missing JPEG SOI marker"
    );

    Ok(&arw_buf[jpeg_offset..jpeg_offset + jpeg_sz])
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
        .context("Failed to open ARW file")?;
    let arw_fd = in_file.as_raw_fd();
    let arw_buf = mmap_arw(arw_fd).await?;
    let jpeg_buf = extract_jpeg(&arw_buf)?;
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
                if e.path().extension().map_or(false, |ext| ext == "ARW")
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
    let output_dir = Box::leak(Box::new(output_dir)); // It's gonna get used for each ARW file and
                                                      // would need a copy for .filter_map(),
                                                      // better to just make it &'static

    fs::create_dir_all(&output_dir)
        .await
        .context("Failed to create output directory")?;
    process_directory(&in_dir, output_dir).await?;

    Ok(())
}
