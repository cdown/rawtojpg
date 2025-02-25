use anyhow::{ensure, Result};
use memmap2::Mmap;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use tokio::fs::{File, OpenOptions};
use windows::Win32::Storage::FileSystem::FILE_FLAG_RANDOM_ACCESS;
use windows::Win32::System::Memory::{PrefetchVirtualMemory, WIN32_MEMORY_RANGE_ENTRY};
use windows::Win32::System::Threading::GetCurrentProcess;

pub fn mmap_raw(file: File) -> Result<Mmap> {
    // SAFETY: see comment in unix.rs
    let raw_buf = unsafe { Mmap::map(file.as_raw_handle())? };
    Ok(raw_buf)
}

pub async fn open_raw(path: &Path) -> Result<File> {
    // There's no MADV_RANDOM equivalent, we have to do it at open time. See unix.rs for why we do
    // this in general.
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_RANDOM_ACCESS.0)
        .open(path)
        .await?)
}

pub fn prefetch_jpeg(raw_buf: &Mmap, jpeg: &crate::EmbeddedJpegInfo) -> Result<()> {
    ensure!(
        jpeg.offset + jpeg.length <= raw_buf.len(),
        "JPEG data is out of bounds"
    );

    // SAFETY: The `ensure!` above guarantees that the range [jpeg.offset, jpeg.offset +
    // jpeg.length) is within the bounds of `raw_buf`, so `raw_buf.as_ptr().add(jpeg.offset)`
    // produces a valid pointer, and it's fine to give jpeg.length as NumberOfBytes. The rest is
    // just simple FFI.
    unsafe {
        let process = GetCurrentProcess();
        let entry = [WIN32_MEMORY_RANGE_ENTRY {
            VirtualAddress: raw_buf.as_ptr().add(jpeg.offset) as *mut _,
            NumberOfBytes: jpeg.length,
        }];
        PrefetchVirtualMemory(process, &entry, 0)?;
    }
    Ok(())
}
