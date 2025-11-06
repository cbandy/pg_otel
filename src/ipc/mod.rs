// SPDX-License-Identifier: ISC

mod wait;

pub use wait::WaitEventSet;

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub struct Reader(#[cfg(unix)] io::PipeReader);

#[derive(Debug)]
pub struct Writer(#[cfg(unix)] io::PipeWriter);

pub fn new() -> Result<(Reader, Writer)> {
    use std::os::fd::AsRawFd;

    // When available, use pipe(2) to send small messages lock-free;
    #[cfg(unix)]
    let (reader, writer) = io::pipe()?;

    // Reset FD_CLOEXEC to share these descriptors between the backends and exporter.
    #[cfg(unix)]
    for fd in &[reader.as_raw_fd(), writer.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
        let flags = flags & !libc::FD_CLOEXEC;
        if unsafe { libc::fcntl(*fd, libc::F_SETFD, flags) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
    }

    Ok((Reader(reader), Writer(writer)))
}
