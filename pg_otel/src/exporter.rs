// SPDX-License-Identifier: MIT

use crate::shmem::{Queue, QueueSharedMemory};
use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};
use pgrx::pg_sys;
use std::{sync, time};

// IPC queue in shared memory.
static QUEUE: QueueSharedMemory = QueueSharedMemory::new(c"pg_otel_exporter_queue", || {
    1024 * 1024 // TODO: make configurable, PGC_POSTMASTER
});

pub fn install_hooks() {
    debug_assert!(crate::assert_postmaster_startup());

    // Set the "extra" value so hooks know when they are running inside this worker.
    BackgroundWorkerBuilder::new("OpenTelemetry exporter")
        .set_start_time(BgWorkerStartTime::PostmasterStart)
        .set_restart_time(Some(time::Duration::from_secs(1)))
        .set_function("exporter_main")
        .set_library(crate::PG_OTEL_LIBRARY)
        .set_extra("E")
        .load();

    use pgrx::pg_guard;
    pgrx::pg_shmem_init!(QUEUE);
}

pub fn send(data: &crate::BytesMut, notify: bool) {
    let result = QUEUE.push(data);
    if notify && result.is_ok() {
        QUEUE.notify();
    }
}

pub fn send_one(data: &crate::BytesMut) {
    send(data, true);
}

#[unsafe(no_mangle)]
#[pgrx::pg_guard]
pub extern "C-unwind" fn exporter_main(_arg: pg_sys::Datum) {
    // Immediately register handlers and unblock signals.
    // These handlers set MyLatch, ConfigReloadPending, and ShutdownRequestPending.
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    pgrx::log!("{} is starting", BackgroundWorker::get_name(),);

    QUEUE.set_latch(unsafe { pg_sys::MyLatch });

    // wake up every 10s or if we received a SIGTERM or latch signal
    while BackgroundWorker::wait_latch(Some(time::Duration::from_secs(10))) {
        if BackgroundWorker::sighup_received() {
            // on SIGHUP, reload configuration if needed
        }

        let mut count = 0;
        while let Some(_data) = QUEUE.pop() {
            count += 1;
        }

        if count > 0 {
            pgrx::log!("exporter worker popped {} log records", count);
        }
    }

    pgrx::log!("{} stopped", BackgroundWorker::get_name());
}
