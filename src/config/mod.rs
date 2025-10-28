// SPDX-License-Identifier: ISC

mod compression;
mod endpoint;
mod protocol;
mod signals;

pub(crate) use self::compression::*;
pub(crate) use self::endpoint::*;
pub(crate) use self::protocol::*;
pub(crate) use self::signals::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Encoding(#[from] std::str::Utf8Error),

    #[error(transparent)]
    ExportCompression(#[from] opentelemetry_otlp::ExporterBuildError),

    #[error("{0}")]
    ExportEndpoint(String),

    #[error("unknown protocol: {0:?}")]
    ExportProtocol(String),

    #[error("unknown signal: {0:?}")]
    ExportSignal(String),
}
