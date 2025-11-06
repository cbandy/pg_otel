// SPDX-License-Identifier: ISC

use pgrx::{AllocatedByRust, PgBox, pg_sys};
use std::time::Duration;

/// WaitEventSet is a wrapper around Postgres' internal mechanism for efficiently
/// waiting for signals from other processes and detecting Postmaster crash or exit.
///
/// - https://doxygen.postgresql.org/waiteventset_8h.html
///
/// NOTE: Like all functions in pg_sys, these methods can only be called from the main thread.
pub struct WaitEventSet {
    capacity: i32,
    inner: PgBox<pg_sys::WaitEventSet>,
}

impl Drop for WaitEventSet {
    fn drop(&mut self) {
        unsafe { pg_sys::FreeWaitEventSet(self.inner.as_ptr()) }
    }
}

impl WaitEventSet {
    /// WaitEventSet can only wait for a fixed number of event types.
    /// Choose a capacity that is greater than or equal to the number of expected events.
    pub fn new(capacity: i32) -> WaitEventSet {
        #[cfg(any(feature = "pg13", feature = "pg14", feature = "pg15", feature = "pg16"))]
        let inner = unsafe { pg_sys::CreateWaitEventSet(pg_sys::TopMemoryContext, capacity) };

        #[cfg(not(any(feature = "pg13", feature = "pg14", feature = "pg15", feature = "pg16")))]
        let inner = unsafe { pg_sys::CreateWaitEventSet(pg_sys::CurrentResourceOwner, capacity) };

        WaitEventSet {
            capacity,
            inner: unsafe { PgBox::from_pg(inner) },
        }
    }

    /// Wait for a latch that is set by another process or a signal handler in this process.
    pub fn expect_latch_set(&mut self, latch: *mut pg_sys::Latch) {
        debug_assert!(self.capacity > 0);
        unsafe {
            pg_sys::AddWaitEventToSet(
                self.inner.as_ptr(),
                pg_sys::WL_LATCH_SET,
                pg_sys::PGINVALID_SOCKET,
                latch,
                std::ptr::null_mut(),
            );
        }
        self.capacity -= 1;
    }

    /// Wait for an indication that Postmaster has crashed or exited.
    /// When this happens, the caller should finish its work and exit as well.
    pub fn expect_postmaster_death(&mut self) {
        debug_assert!(self.capacity > 0);
        unsafe {
            pg_sys::AddWaitEventToSet(
                self.inner.as_ptr(),
                pg_sys::WL_POSTMASTER_DEATH,
                pg_sys::PGINVALID_SOCKET,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }
        self.capacity -= 1;
    }

    pub fn expect_readable(&mut self, reader: &crate::ipc::Reader) {
        use std::os::fd::AsRawFd;

        debug_assert!(self.capacity > 0);
        unsafe {
            pg_sys::AddWaitEventToSet(
                self.inner.as_ptr(),
                pg_sys::WL_SOCKET_READABLE,
                reader.0.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }
        self.capacity -= 1;
    }

    /// Wait up to Some(timeout) for one of the expected events.
    /// Some(0) checks without blocking, and None blocks forever.
    fn wait(
        &mut self,
        timeout: Option<Duration>,
    ) -> Option<PgBox<pg_sys::WaitEvent, AllocatedByRust>> {
        // Construct an event into which WaitEventSetWait can write.
        let event = unsafe { PgBox::<pg_sys::WaitEvent>::alloc0() };
        let milliseconds: libc::c_long = match timeout {
            Some(d) => d.as_millis().try_into().unwrap(),
            None => -1,
        };

        let occurred = unsafe {
            pg_sys::WaitEventSetWait(
                self.inner.as_ptr(),
                milliseconds,
                event.as_ptr(),
                1,
                pg_sys::PG_WAIT_EXTENSION,
            )
        };

        (occurred > 0).then_some(event)
    }

    /// Wait forever for one of the expected events.
    pub fn wait_forever(&mut self) -> PgBox<pg_sys::WaitEvent, AllocatedByRust> {
        let result = self.wait(None);
        debug_assert!(result.is_some());
        result.unwrap()
    }
}

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use super::WaitEventSet;
    use pgrx::pg_sys;

    #[pgrx::pg_test]
    fn memory_management() {
        let ctx: pgrx::PgMemoryContexts;
        let counters = unsafe { pgrx::PgBox::<pg_sys::MemoryContextCounters>::alloc() };

        // Create a WaitEventSet and note the metrics of its MemoryContext.
        let during: pg_sys::MemoryContextCounters;
        {
            let ws = WaitEventSet::new(8);
            ctx = unsafe { pgrx::PgMemoryContexts::of(ws.inner.as_ptr() as pgrx::void_mut_ptr) }
                .unwrap();

            unsafe { pg_sys::MemoryContextMemConsumed(ctx.value(), counters.as_ptr()) };
            during = *counters;

            // The WaitEventSet is dropped and freed here.
        }

        // Read the metrics of the MemoryContext again.
        let after: pg_sys::MemoryContextCounters;
        unsafe { pg_sys::MemoryContextMemConsumed(ctx.value(), counters.as_ptr()) };
        after = *counters;

        assert_eq!(after.nblocks, during.nblocks);
        assert!(after.freechunks > during.freechunks);
    }

    #[pgrx::pg_test]
    fn event_readable() {
        let (r, mut w) = super::super::new().unwrap();

        // Register for readable events.
        let mut ws = WaitEventSet::new(8);
        ws.expect_readable(&r);

        // Trigger an IO event.
        use std::io::Write;
        w.0.write(&[0]).unwrap();

        // Wait for some event; timeout returns None.
        let event = ws
            .wait(Some(std::time::Duration::from_secs(1)))
            .expect("no timeout");

        assert_eq!(event.events & pg_sys::WL_SOCKET_READABLE, pg_sys::WL_SOCKET_READABLE);
    }
}
