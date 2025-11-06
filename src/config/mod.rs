// SPDX-License-Identifier: ISC

mod compression;
mod endpoint;
mod postgres;
mod protocol;
mod signals;

pub use self::compression::*;
pub use self::endpoint::*;
pub use self::postgres::{define_guc_variables, exporting, loaded};
pub use self::protocol::*;
pub use self::signals::{ExportSignal::*, *};

use opentelemetry_otlp as otlp;
use opentelemetry_sdk as sdk;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone)]
pub struct Config {
    pub logs_otlp: OTLP,
    pub resource: sdk::Resource,
}

#[derive(Clone)]
pub struct OTLP {
    pub compression: Option<otlp::Compression>,
    pub endpoint: String,
    pub metadata: HashMap<String, String>,
    pub protocol: otlp::Protocol,
    pub timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Encoding(#[from] std::str::Utf8Error),

    #[error(transparent)]
    ExportCompression(#[from] otlp::ExporterBuildError),

    #[error("{0}")]
    ExportEndpoint(String),

    #[error("unknown protocol: {0:?}")]
    ExportProtocol(String),

    #[error("unknown signal: {0:?}")]
    ExportSignal(String),
}
