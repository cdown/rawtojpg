use anyhow::Result;
use memmap2::{Advice, Mmap};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use tokio::fs::File;

pub fn mmap_raw(file: File) -> Result<Mmap> {
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

pub async fn open_raw(path: &Path) -> Result<File> {
    Ok(File::open(path).await?)
}

pub fn prefetch_jpeg(raw_buf: &Mmap, jpeg: &crate::EmbeddedJpegInfo) -> Result<()> {
    raw_buf.advise_range(Advice::WillNeed, jpeg.offset, jpeg.length)?;
    Ok(())
}
