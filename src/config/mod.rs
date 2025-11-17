// SPDX-License-Identifier: ISC

mod compression;
mod endpoint;
mod postgres;
mod protocol;
mod signals;
mod w3c;

pub use self::compression::*;
pub use self::endpoint::*;
pub use self::postgres::{define_guc_variables, exporting, loaded};
pub use self::protocol::*;
pub use self::signals::{ExportSignal::*, *};
pub use self::w3c::*;

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
    #[error("malformed baggage: {0}")]
    Baggage(String),

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

pub trait FromStr<'a, T = Self>
where
    T: std::str::FromStr<Err = Error>,
    T: std::convert::TryFrom<&'a std::ffi::CStr, Error = Error>,
{
    fn try_from_ptr(raw: &*const std::ffi::c_char) -> Result<Option<T>, Error>;
}
