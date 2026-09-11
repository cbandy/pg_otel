// SPDX-License-Identifier: MIT

mod errors;
mod queries;
mod transactions;
mod utility;

use prost::Message;
use std::{thread, time};

use crate::otlp::*;

struct TracingHarness {
    client: postgres::Client,
    endpoint: String,
}

impl TracingHarness {
    fn new() -> Result<Self, postgres::Error> {
        crate::acquire_test_lock();
        let client = crate::connect_test_client();
        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());

        Ok(Self { client, endpoint })
    }

    /// Drains any residual spans from the mock collector and sets `otel.export = 'traces'`.
    fn enable_tracing(&mut self) -> Result<(), postgres::Error> {
        let _ = reqwest::blocking::get(&self.endpoint);
        self.client.batch_execute("SET pg_otel.export = 'traces'")
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

            if let Ok(response) = reqwest::blocking::get(&self.endpoint)
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
}
