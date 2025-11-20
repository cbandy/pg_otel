// SPDX-License-Identifier: ISC

mod wait;

pub use wait::WaitEventSet;

use std::{convert, io, sync};

const ENCODING: bincode::config::Configuration = bincode::config::standard();

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Decode(#[from] bincode::error::DecodeError),

    #[error(transparent)]
    Encode(#[from] bincode::error::EncodeError),

    #[error(transparent)]
    IO(#[from] std::io::Error),

    #[error(transparent)]
    Poisoned(#[from] std::sync::PoisonError<()>),
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(bincode::Decode, bincode::Encode, Debug)]
pub enum Message {
    None, // TODO: unneeded when using synchronous receiver
    Logs(Vec<u8>),
}

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

pub struct SyncReceiver {
    #[cfg(unix)]
    pipe: io::BufReader<io::PipeReader>,
}

impl convert::Into<SyncReceiver> for Reader {
    fn into(self) -> SyncReceiver {
        SyncReceiver {
            pipe: io::BufReader::with_capacity(4 * libc::PIPE_BUF, self.0),
        }
    }
}

impl SyncReceiver {
    pub fn is_buffered(&self) -> bool {
        // TODO: large messages
        let vacant = true;

        #[cfg(unix)]
        let vacant = vacant && self.pipe.buffer().is_empty();

        !vacant
    }

    pub fn recv(&mut self) -> Result<Message> {
        // TODO: large messages

        #[cfg(unix)]
        Ok(bincode::decode_from_reader(&mut self.pipe, ENCODING)?)
    }
}

pub trait SyncSender {
    fn send(&self, message: Message) -> Result<()>;
}

impl SyncSender for sync::Mutex<Writer> {
    /// Send a message to the background worker.
    fn send(&self, message: Message) -> Result<()> {
        let v = bincode::encode_to_vec(&message, ENCODING)?;
        let n = v.len();

        // POSIX.1 and the [Single UNIX Specification] mandate that small writes to a pipe(7) be atomic.
        // Conforming systems write 512 bytes or less atomically and indicate their largest atomic write
        // in PIPE_BUF. On Linux, writes of 4096 bytes or less are atomic.
        //
        // [Single UNIX Specification]: https://unix.org/what_is_unix/single_unix_specification.html
        // [1994, SUS System Interfaces and Headers]: https://pubs.opengroup.org/onlinepubs/009656499/toc.pdf
        // [POSIX.1-2004]: https://pubs.opengroup.org/onlinepubs/009695399/functions/write.html
        // [POSIX.1-2017]: https://pubs.opengroup.org/onlinepubs/9699919799/functions/write.html
        // [POSIX.1-2024]: https://pubs.opengroup.org/onlinepubs/9799919799/functions/write.html
        const MAX_PIPE_ATOMIC: usize = libc::PIPE_BUF;

        #[cfg(unix)]
        {
            debug_assert!(
                n <= MAX_PIPE_ATOMIC,
                "TODO: IPC message larger than {MAX_PIPE_ATOMIC:?} ({n:?})"
            );

            use io::Write;
            let mut w = &self.lock().map_err(|_| sync::PoisonError::new(()))?.0;

            // The entire message should be sent in a single syscall, without interrupts.
            // Panic when that is not the case.
            assert_eq!(n, w.write(&v)?);
            Ok(())
        }

        // TODO: large message
    }
}
