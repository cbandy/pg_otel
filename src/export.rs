// SPDX-License-Identifier: ISC

use super::PG_OTEL_LIBRARY;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Decode(#[from] bincode::error::DecodeError),
}

pub struct Pipeline {
    config: crate::config::Config,
}

impl Pipeline {
    pub fn new(config: crate::config::Config) -> Self {
        Pipeline { config }
    }

    pub fn ingest(&mut self, result: crate::ipc::Result<crate::ipc::Message>) {
        if let Err(error) = result {
            pgrx::warning!("{PG_OTEL_LIBRARY}: unable to receive internally: {error}");
        } else {
            pgrx::notice!("{PG_OTEL_LIBRARY}: received {:?}", result.unwrap());
        }
    }
}
