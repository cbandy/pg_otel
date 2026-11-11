// SPDX-License-Identifier: MIT

mod batches;
mod errors;
mod nested;
mod queries;
mod transactions;
mod utility;

use postgres::SimpleQueryMessage;
use prost::Message;
use std::{thread, time};
use url::Url as URL;

use crate::otlp::*;

struct TracingHarness {
    client: postgres::Client,
    traces: URL,
    logs: URL,
}

impl TracingHarness {
    fn new() -> Result<Self, postgres::Error> {
        crate::acquire_test_lock();
        let client = crate::connect_test_client();
        let base = crate::config::otlp_endpoint();

        Ok(Self {
            client,
            traces: base.join("/test/traces").unwrap(),
            logs: base.join("/test/logs").unwrap(),
        })
    }

    /// Drains any residual telemetry for the specified signals from the mock collector
    /// and sets `otel.export_traces` and/or `otel.export_logs`.
    fn enable_telemetry(&mut self, signals: &str) -> Result<(), postgres::Error> {
        let mut sql = String::new();
        if signals.contains("trace") || signals.contains("span") {
            let _ = reqwest::blocking::get(self.traces.clone());
            sql.push_str("SET otel.export_traces = on; ");
        }
        if signals.contains("log") {
            let _ = reqwest::blocking::get(self.logs.clone());
            sql.push_str("SET otel.export_logs = on; ");
        }
        self.client.batch_execute(&sql)
    }

    /// Drains any residual spans from the mock collector and sets `otel.export_traces = on`.
    fn enable_tracing(&mut self) -> Result<(), postgres::Error> {
        self.enable_telemetry("traces")
    }

    /// Send one [Query](https://www.postgresql.org/docs/current/protocol-flow.html#PROTOCOL-FLOW-SIMPLE-QUERY)
    /// message ('Q') to the backend.
    ///
    /// To execute an [Extended Query](https://www.postgresql.org/docs/current/protocol-flow.html#PROTOCOL-FLOW-EXT-QUERY)
    /// flow (Parse, Describe, Bind, Execute), use `client.query` instead.
    fn simple_query(&mut self, query: &str) -> Result<Vec<SimpleQueryMessage>, postgres::Error> {
        self.client.simple_query(query)
    }

    fn collect_logs_until<F>(&self, timeout: time::Duration, mut condition: F) -> Vec<LogRecord>
    where
        F: FnMut(&[LogRecord], bool) -> bool,
    {
        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < timeout {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(self.logs.clone())
                && let Ok(body) = response.bytes()
            {
                let is_empty = body.is_empty();
                if let Ok(decoded) = LogsData::decode(body) {
                    data.extend(decoded.resource_logs);
                }

                let logs: Vec<LogRecord> = data
                    .iter()
                    .flat_map(|rl| &rl.scope_logs)
                    .flat_map(|sl| &sl.log_records)
                    .cloned()
                    .collect();

                done = condition(&logs, is_empty);
            }
        }

        data.into_iter()
            .flat_map(|rl| rl.scope_logs)
            .flat_map(|sl| sl.log_records)
            .collect()
    }

    fn collect_logs_count(&self, count: usize, timeout: time::Duration) -> Vec<LogRecord> {
        self.collect_logs_until(timeout, |logs, _| logs.len() >= count)
    }

    fn collect_spans_until<F>(&self, timeout: time::Duration, mut condition: F) -> Vec<Span>
    where
        F: FnMut(&[Span], bool) -> bool,
    {
        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < timeout {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(self.traces.clone())
                && let Ok(body) = response.bytes()
            {
                let is_empty = body.is_empty();
                if let Ok(decoded) = TracesData::decode(body) {
                    data.extend(decoded.resource_spans);
                }

                let spans: Vec<Span> = data
                    .iter()
                    .flat_map(|rs| &rs.scope_spans)
                    .flat_map(|ss| &ss.spans)
                    .cloned()
                    .collect();

                done = condition(&spans, is_empty);
            }
        }

        data.into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect()
    }

    fn collect_spans(&self, timeout: time::Duration) -> Vec<Span> {
        self.collect_spans_until(timeout, |spans, is_empty| is_empty && !spans.is_empty())
    }

    fn collect_spans_count(&self, count: usize, timeout: time::Duration) -> Vec<Span> {
        self.collect_spans_until(timeout, |spans, _| spans.len() >= count)
    }

    fn collect_transaction_spans(&self, timeout: time::Duration) -> Vec<Span> {
        self.collect_spans_until(timeout, |spans, _| {
            spans.iter().any(|s| s.name == "TRANSACTION")
        })
    }

    fn collect_resource_spans(&self, timeout: time::Duration) -> Vec<ResourceSpans> {
        let start = time::Instant::now();
        let mut data = Vec::new();

        while start.elapsed() < timeout {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(self.traces.clone())
                && let Ok(body) = response.bytes()
                && !body.is_empty()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
                break;
            }
        }
        data
    }
}
