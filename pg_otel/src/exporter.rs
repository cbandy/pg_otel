// SPDX-License-Identifier: MIT

mod config;

use crate::shmem::{Queue, QueueSharedMemory};
use crate::{GucInt32, GucString};
use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};
use pgrx::pg_sys;
use std::{ffi, time};
use url::Url as URL;

static GUC_BATCH_MAX_DELAY_MS: GucInt32 = GucInt32::new(1000);
static GUC_BATCH_MAX_ITEMS: GucInt32 = GucInt32::new(512);
static GUC_COMPRESSION: GucString = GucString::new(Some(c"none"));
static GUC_ENDPOINT: GucString = GucString::new(Some(c"http://localhost:4318"));
static GUC_HEADERS: GucString = GucString::new(None);
static GUC_PROTOCOL: GucString = GucString::new(Some(c"http/protobuf"));
static GUC_TIMEOUT_MS: GucInt32 = GucInt32::new(10000);

// IPC queue in shared memory.
static QUEUE: QueueSharedMemory = QueueSharedMemory::new(c"pg_otel_exporter_queue", || {
    1024 * 1024 // TODO: make configurable, PGC_POSTMASTER
});

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
        i32::MAX / 4,
        GucContext::Sighup, // server reload
        GucFlags::empty(),  // none
    );
    GucRegistry::define_int_guc(
        c"pg_otel.batch_max_delay",
        c"Max wait time before exporting a batch",
        c"Maximum duration (in milliseconds) the exporter waits before flushing.",
        &GUC_BATCH_MAX_DELAY_MS,
        0,
        time::Duration::from_hours(1).as_millis() as i32,
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
        GucFlags::UNIT_MS,
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

    // https://github.com/pgcentralfoundation/pgrx/issues/2370
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

/// Export an accumulated batch of OTLP data.
fn export_batch(batch: &[Vec<u8>]) {
    use crate::otlp::*;

    let scope = InstrumentationScope::build()
        .name(crate::PG_OTEL_LIBRARY)
        .version(crate::PG_OTEL_VERSION)
        .finish();

    let records: Vec<LogRecord> = batch
        .iter()
        .filter_map(|bytes| prost::Message::decode(bytes.as_slice()).ok())
        .collect();

    if !records.is_empty() {
        let path = "v1/logs";

        let scope_logs = ScopeLogs::new(scope.clone(), records);
        let resource_logs = ResourceLogs::new(None, vec![scope_logs]);
        let service_request = ExportLogsServiceRequest::new(vec![resource_logs]);

        let mut body = Vec::new();
        if let Err(error) = prost::Message::encode(&service_request, &mut body) {
            pgrx::warning!("pg_otel: failed to encode OTLP {path} request: {error}");
        } else {
            export_otlp(path, "application/x-protobuf", body);
        }
    }

    let spans: Vec<Span> = batch
        .iter()
        .filter_map(|bytes| prost::Message::decode(bytes.as_slice()).ok())
        .collect();

    if !spans.is_empty() {
        let path = "v1/traces";

        let scope_spans = ScopeSpans::new(scope.clone(), spans);
        let resource_spans = ResourceSpans::create(None, vec![scope_spans]);
        let service_request = ExportTraceServiceRequest::new(vec![resource_spans]);

        let mut body = Vec::new();
        if let Err(error) = prost::Message::encode(&service_request, &mut body) {
            pgrx::warning!("pg_otel: failed to encode OTLP {path} request: {error}");
        } else {
            export_otlp(path, "application/x-protobuf", body);
        }
    }
}

fn export_otlp(path: &str, content: &str, body: Vec<u8>) {
    let endpoint = config::Endpoint::from(&GUC_ENDPOINT).unwrap();
    let timeout = time::Duration::from_millis(GUC_TIMEOUT_MS.get().max(1) as u64);

    let mut request = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(client) => client.post(endpoint.join(path).unwrap().as_str()),
        Err(error) => {
            return pgrx::warning!("pg_otel: failed to build HTTP client: {error}");
        }
    };

    for (k, v) in config::Headers::from(&GUC_HEADERS) {
        request = request.header(k, v);
    }

    let response = match request.header("Content-Type", content).body(body).send() {
        Ok(response) => response,
        Err(error) => {
            return pgrx::warning!("pg_otel: failed to send OTLP {path} batch: {error}");
        }
    };

    if let status = response.status()
        && !status.is_success()
    {
        return pgrx::warning!("pg_otel: export HTTP {path} failed with status: {status}");
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
    QUEUE.set_latch(unsafe { pg_sys::MyLatch });

    let mut shutdown_deadline: Option<time::Instant> = None;
    let max_shutdown_delay = time::Duration::from_secs(5);

    loop {
        // Check for configuration changes before doing any work.
        // SAFETY: This is set atomically by the SIGHUP handler and is safe to read here.
        if unsafe { pg_sys::ConfigReloadPending } != 0 {
            // Reset the signal flag then load the config file.
            // SAFETY: These are safe to call from the main thread.
            unsafe { pg_sys::ConfigReloadPending = 0 };
            unsafe { pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP) };
        }

        let limit = GUC_BATCH_MAX_ITEMS.get().max(1) as usize;
        while let Some(item) = QUEUE.pop() {
            batch.push(item);

            if batch.len() >= limit {
                export_batch(&batch);
                batch.clear();
            }
        }
        if !batch.is_empty() {
            export_batch(&batch);
            batch.clear();
        }

        // When this process as been signaled to terminate, check for work more frequently and exit
        // only after other backends have stopped.
        //
        // SAFETY: This is set atomically by the SIGTERM handler and is safe to read here.
        let next_wait = if unsafe { pg_sys::ShutdownRequestPending } != 0 {
            let now = time::Instant::now();
            let deadline = *shutdown_deadline.get_or_insert_with(|| now + max_shutdown_delay);

            // Continue exporting telemetry until all client backends and workers have terminated.
            // Auxiliary processes (checkpointer, bgwriter, walwriter) are ignored/excluded here
            // because they are not signalled until AFTER all background workers terminate.
            //
            // SAFETY: `CountDBBackends` acquires `ProcArrayLock` internally.
            if now >= deadline || unsafe { pg_sys::CountDBBackends(pg_sys::InvalidOid) } <= 0 {
                break;
            }

            time::Duration::from_millis(GUC_BATCH_MAX_DELAY_MS.get() as u64)
                .min(deadline.saturating_duration_since(now))
                .min(time::Duration::from_millis(100))
        } else {
            time::Duration::from_millis(GUC_BATCH_MAX_DELAY_MS.get() as u64)
        };

        // Sleep until (1) `next_wait` elapses, (2) the latch is set by a `QUEUE` producer, or (3)
        // the latch is set by a SIGHUP or SIGTERM handler. Zero here waits forever, so clamp the
        // value to at least one.
        BackgroundWorker::wait_latch(Some(next_wait.max(time::Duration::from_millis(1))));
    }

    pgrx::log!("{} stopped", BackgroundWorker::get_name());
}

#[cfg(any(test, feature = "pg_test"))]
pub fn endpoint() -> String {
    GUC_ENDPOINT
        .get()
        .as_deref()
        .map(ffi::CStr::to_string_lossy)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
pub mod tests {
    use prost::Message;
    use std::sync::LazyLock;
    use tiny_http::*;

    /// Mock OTLP collector for integration testing.
    ///
    /// The Rust test binary invokes this constructor once before initializing Postgres.
    /// The server here listens for OTLP requests on a random port until the Rust test binary exits.
    ///
    /// NOTE: The `[pg_test]` macro (1) turns the function body into a SQL function annotated with
    /// `#[cfg(feature = "pg_test")]` and (2) changes the test function into one remote call of that
    /// SQL function.
    ///
    /// For the test function body to validate the OTLP requests sent to this listener, it must
    /// reach (back) from the Postgres process to the test runner process.
    pub static HTTP_OTLP_SERVER: LazyLock<ListenAddr> = LazyLock::new(|| {
        let addr = ConfigListenAddr::from_socket_addrs("127.0.0.1:0").unwrap();
        let config = ServerConfig { addr, ssl: None };
        let server = Server::new(config).unwrap();
        let mut sink = OTLP::default();
        let bound = server.server_addr();

        std::thread::spawn(move || {
            while let Ok(request) = server.recv() {
                sink.handle(request);
            }
        });

        bound
    });

    #[derive(Default)]
    struct OTLP {
        logs: Vec<crate::otlp::ResourceLogs>,
        traces: Vec<crate::otlp::ResourceSpans>,
    }

    impl OTLP {
        fn handle(&mut self, request: Request) {
            match (request.method(), request.url()) {
                // https://opentelemetry.io/docs/specs/otlp#otlphttp
                (Method::Post, "/v1/logs") => self.handle_otlp_logs(request),
                (Method::Post, "/v1/traces") => self.handle_otlp_traces(request),

                // Returns some or all of a requested signal; "/test/{signal}[/{count}]"
                (Method::Get, url) if url.starts_with("/test/") => {
                    let path = url.strip_prefix("/test/").unwrap();
                    let (signal, count) = path.split_once('/').unwrap_or((path, ""));
                    let (signal, count) = (signal.to_owned(), Self::number(Some(count)));
                    self.handle_test(request, signal, count);
                }
                _ => self.reject(request, None),
            }
        }

        fn number(s: Option<&str>) -> Option<usize> {
            s.map(str::as_bytes).and_then(atoi::atoi)
        }

        fn handle_otlp_logs(&mut self, mut request: Request) {
            let n = request.body_length().unwrap_or(0);
            let mut body = crate::BytesMut::zeroed(n);
            let _ = request.as_reader().read_exact(body.as_mut());

            match crate::otlp::ExportLogsServiceRequest::decode(body) {
                Err(error) => self.reject(request, Some(error.into())),
                Ok(export) => {
                    self.logs.extend(export.resource_logs);
                    let _ = request.respond(Response::empty(200));
                }
            }
        }

        fn handle_otlp_traces(&mut self, mut request: Request) {
            let n = request.body_length().unwrap_or(0);
            let mut body = crate::BytesMut::zeroed(n);
            let _ = request.as_reader().read_exact(body.as_mut());

            match crate::otlp::ExportTraceServiceRequest::decode(body) {
                Err(error) => self.reject(request, Some(error.into())),
                Ok(export) => {
                    self.traces.extend(export.resource_spans);
                    let _ = request.respond(Response::empty(200));
                }
            }
        }

        fn handle_test(&mut self, request: Request, signal: String, count: Option<usize>) {
            use crate::otlp::*;

            let body = match signal.as_str() {
                "logs" => {
                    let n = count.map_or(self.logs.len(), |n| n.min(self.logs.len()));
                    prost::Message::encode_to_vec(&LogsData::new(self.logs.drain(..n)))
                }
                "traces" => {
                    let n = count.map_or(self.traces.len(), |n| n.min(self.traces.len()));
                    prost::Message::encode_to_vec(&TracesData::new(self.traces.drain(..n)))
                }
                _ => {
                    return self.reject(request, None);
                }
            };

            let proto: Header = "Content-Type: application/x-protobuf".parse().unwrap();
            let _ = request.respond(
                Response::empty(200)
                    .with_header(proto)
                    .with_data(&body[..], Some(body.len())),
            );
        }

        fn reject(&self, request: Request, error: Option<eyre::Error>) {
            if let Some(error) = error {
                let body = error.to_string();
                let _ = request.respond(Response::new(
                    StatusCode(400),
                    Vec::new(),
                    body.as_bytes(),
                    Some(body.len()),
                    None,
                ));
            } else {
                let _ = request.respond(Response::empty(400));
            }
        }
    }
}
