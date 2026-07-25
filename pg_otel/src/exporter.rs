// SPDX-License-Identifier: MIT

mod config;

use self::config::{GucInt32, GucString};
use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};
use pgrx::pg_sys;
use std::{ffi, sync, time};
use url::Url as URL;

static GUC_BATCH_MAX_DELAY_MS: GucInt32 = GucInt32::new(1000);
static GUC_BATCH_MAX_ITEMS: GucInt32 = GucInt32::new(512);
static GUC_COMPRESSION: GucString = GucString::new(Some(c"none"));
static GUC_ENDPOINT: GucString = GucString::new(Some(c"http://localhost:4318"));
static GUC_HEADERS: GucString = GucString::new(None);
static GUC_PROTOCOL: GucString = GucString::new(Some(c"http/protobuf"));
static GUC_TIMEOUT_MS: GucInt32 = GucInt32::new(10000);

// IPC queue in shared memory.
static QUEUE: sync::OnceLock<crate::shmem::Queue> = sync::OnceLock::new();

pub fn define_guc_variables() {
    use pgrx::guc::{GucCheckError, GucContext, GucFlags, GucRegistry};

    // SAFETY: GUC hooks must have `#[pgrx::pg_guard]` or `#[pgrx::pg_guc_hook]`.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"pg_otel.endpoint",
            c"Base OTLP collector endpoint URL",
            c"Base URL of an OpenTelemetry collector.",
            &GUC_ENDPOINT,
            GucContext::Sighup, // server reload
            GucFlags::empty(),  // none
            Some(check_endpoint),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_endpoint(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            eyre::OptionExt::ok_or_eyre(value.as_deref(), "required")
                .and_then(config::Endpoint::validate)
                .map_err(|e| GucCheckError::new(e.to_string()))
        }
    }

    // SAFETY: GUC hooks must have `#[pgrx::pg_guard]` or `#[pgrx::pg_guc_hook]`.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"pg_otel.headers",
            c"OTLP headers for all signals (superuser only)",
            c"Headers sent with OTLP export requests. May contain sensitive credentials. Restricted to superusers.",
            &GUC_HEADERS,
            GucContext::Sighup, // server reload
            GucFlags::SUPERUSER_ONLY,
            Some(check_headers),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_headers(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            if let Some(value) = value {
                config::Headers::validate(&value).map_err(|e| GucCheckError::new(e.to_string()))?;
            }
            Ok(())
        }
    }

    GucRegistry::define_string_guc(
        c"pg_otel.protocol",
        c"OTLP transport protocol",
        c"Exporter protocol (http/protobuf or http/json).",
        &GUC_PROTOCOL,
        GucContext::Sighup, // server reload
        GucFlags::empty(),  // none
    );

    GucRegistry::define_int_guc(
        c"pg_otel.batch_max_items",
        c"Max items per HTTP export batch",
        c"Maximum number of log records exported in a single HTTP batch.",
        &GUC_BATCH_MAX_ITEMS,
        1,
        i32::MAX,
        GucContext::Sighup, // server reload
        GucFlags::empty(),  // none
    );
    GucRegistry::define_int_guc(
        c"pg_otel.batch_max_delay",
        c"Max wait time before exporting a batch",
        c"Maximum duration (in milliseconds) the exporter waits before flushing.",
        &GUC_BATCH_MAX_DELAY_MS,
        0,
        i32::MAX,
        GucContext::Sighup, // server reload
        GucFlags::UNIT_MS,
    );
    GucRegistry::define_int_guc(
        c"pg_otel.timeout",
        c"Send timeout",
        c"Request timeout duration in milliseconds.",
        &GUC_TIMEOUT_MS,
        1,
        i32::MAX,
        GucContext::Sighup, // server reload
        GucFlags::empty(),  // none
    );
    GucRegistry::define_string_guc(
        c"pg_otel.compression",
        c"HTTP payload compression",
        c"Payload compression algorithm (none, gzip, or zstd).",
        &GUC_COMPRESSION,
        GucContext::Sighup, // server reload
        GucFlags::empty(),  // none
    );
}

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
    pgrx::pg_shmem_init!(QUEUE_HOOK);
    static QUEUE_HOOK: crate::shmem::QueueBuilder = crate::shmem::QueueBuilder {
        name: c"pg_otel_exporter_queue",
        queue: &QUEUE,
        size: || 1024 * 1024, // TODO: make configurable, PGC_POSTMASTER
    };
}

pub fn send(data: &crate::BytesMut, notify: bool) {
    let outgoing = QUEUE.get().unwrap();
    let result = outgoing.push(&data);
    if notify && result.is_ok() {
        outgoing.notify();
    }
}

pub fn send_one(data: &crate::BytesMut) {
    send(&data, true);
}

/// Export an accumulated batch of OTLP log records.
fn export_batch(batch: &[Vec<u8>]) {
    use crate::otlp::*;
    use prost::Message;

    let records: Vec<LogRecord> = batch
        .iter()
        .filter_map(|bytes| LogRecord::decode(bytes.as_slice()).ok())
        .collect();

    if records.is_empty() {
        return;
    }

    let scope = InstrumentationScope::build()
        .name(crate::PG_OTEL_LIBRARY)
        .version(crate::PG_OTEL_VERSION)
        .finish();

    let scope_logs = ScopeLogs::new(scope, records);
    let resource_logs = ResourceLogs::new(None, vec![scope_logs]);
    let service_request = ExportLogsServiceRequest::new(vec![resource_logs]);

    let mut body = Vec::new();
    if let Err(error) = service_request.encode(&mut body) {
        return pgrx::warning!("pg_otel: failed to encode OTLP request: {error}");
    }

    let endpoint = config::Endpoint::from(&GUC_ENDPOINT).unwrap();
    let timeout = time::Duration::from_millis(GUC_TIMEOUT_MS.get().max(1) as u64);

    let mut request = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(client) => client.post(endpoint.join("v1/logs").unwrap().as_str()),
        Err(error) => {
            return pgrx::warning!("pg_otel: failed to build HTTP client: {error}");
        }
    };

    for (k, v) in config::Headers::from(&GUC_HEADERS) {
        request = request.header(k, v);
    }

    let result = request
        .header("Content-Type", "application/x-protobuf")
        .body(body)
        .send();

    if let Err(error) = result {
        return pgrx::warning!("pg_otel: failed to send OTLP log batch: {error}");
    }
    if let Ok(response) = result
        && let status = response.status()
        && !status.is_success()
    {
        return pgrx::warning!("pg_otel: export HTTP request failed with status: {status}");
    }
}

#[unsafe(no_mangle)]
#[pgrx::pg_guard]
pub extern "C-unwind" fn exporter_main(_arg: pg_sys::Datum) {
    // Immediately register handlers and unblock signals.
    // These handlers set MyLatch, ConfigReloadPending, and ShutdownRequestPending.
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    pgrx::log!("{} is starting", BackgroundWorker::get_name());

    let mut batch = Vec::new();
    let mut limit = GUC_BATCH_MAX_ITEMS.get().max(1) as usize;
    let incoming = QUEUE.get().unwrap();
    unsafe { incoming.set_latch(pg_sys::MyLatch as usize) };
    unsafe { pg_sys::SetLatch(pg_sys::MyLatch) };

    // Wake every time (1) GUC_BATCH_MAX_DELAY_MS passes, (2) the latch is set by a QUEUE producer,
    // or (3) the latch is set by a SIGHUP or SIGTERM handler. Zero here waits forever, so clamp it
    // to at least one.
    while BackgroundWorker::wait_latch(Some({
        time::Duration::from_millis(GUC_BATCH_MAX_DELAY_MS.get().max(1) as u64)
    })) {
        if BackgroundWorker::sighup_received() {
            // SAFETY: This is safe to call from the main thread.
            unsafe { pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP) };
        }

        incoming.set_waiting(false);
        limit = GUC_BATCH_MAX_ITEMS.get().max(1) as usize;

        while let Some(data) = incoming.pop() {
            batch.push(data);

            if batch.len() >= limit {
                export_batch(&batch);
                batch.clear();
            }
        }
        if !batch.is_empty() {
            export_batch(&batch);
            batch.clear();
        }

        incoming.set_waiting(true);
    }
    incoming.set_waiting(false);

    // Received a SIGTERM; gracefully shutdown by draining the queue.
    while let Some(data) = incoming.pop() {
        batch.push(data);

        if batch.len() >= limit {
            export_batch(&batch);
            batch.clear();
        }
    }
    if !batch.is_empty() {
        export_batch(&batch);
        batch.clear();
    }

    pgrx::log!("{} stopped", BackgroundWorker::get_name());
}
