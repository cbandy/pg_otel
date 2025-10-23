// SPDX-License-Identifier: ISC

use enumset::EnumSet;
use http::uri;
use opentelemetry_otlp as otlp;
use std::ffi::CStr;
use std::{convert, fmt, str};
use thiserror;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Encoding(#[from] str::Utf8Error),

    #[error(transparent)]
    ExportCompression(#[from] otlp::ExporterBuildError),

    #[error("{0}")]
    ExportEndpoint(String),

    #[error("unknown protocol: {0:?}")]
    ExportProtocol(String),

    #[error("unknown signal: {0:?}")]
    ExportSignal(String),
}

#[derive(PartialEq)]
pub struct ExportCompression(otlp::Compression);

impl ExportCompression {
    pub fn from_ptr(raw: &*const std::ffi::c_char) -> Result<Option<Self>, Error> {
        match (!raw.is_null()).then(|| unsafe { CStr::from_ptr(*raw) }) {
            None => Ok(None),
            Some(cstr) if cstr.is_empty() => Ok(None),
            Some(cstr) => cstr.try_into().and_then(|_self| Ok(Some(_self))),
        }
    }

    fn new(raw: &str) -> Result<Self, Error> {
        let lower = raw.to_lowercase();

        Ok(Self {
            0: lower.parse().map_err(Error::ExportCompression)?,
        })
    }
}

impl fmt::Debug for ExportCompression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({})", self)
    }
}

impl fmt::Display for ExportCompression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl str::FromStr for ExportCompression {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

impl convert::TryFrom<&CStr> for ExportCompression {
    type Error = Error;

    fn try_from(raw: &CStr) -> Result<Self, Error> {
        raw.to_str()?.parse()
    }
}

impl convert::Into<otlp::Compression> for ExportCompression {
    fn into(self) -> otlp::Compression {
        self.0
    }
}

#[derive(PartialEq)]
pub struct ExportEndpoint(uri::Uri);

impl ExportEndpoint {
    pub fn from_ptr(raw: &*const std::ffi::c_char) -> Result<Option<Self>, Error> {
        match (!raw.is_null()).then(|| unsafe { CStr::from_ptr(*raw) }) {
            None => Ok(None),
            Some(cstr) if cstr.is_empty() => Ok(None),
            Some(cstr) => cstr.try_into().and_then(|_self| Ok(Some(_self))),
        }
    }

    fn new(raw: &str) -> Result<Self, Error> {
        use uri::Scheme;

        let parsed = raw
            .parse::<uri::Uri>()
            .map_err(|e| Error::ExportEndpoint(e.to_string()))?;

        match parsed.scheme() {
            Some(s) if *s == Scheme::HTTP => (),
            Some(s) if *s == Scheme::HTTPS => (),
            _ => {
                return Err(Error::ExportEndpoint("URL must begin with http or https".into()));
            }
        }

        Ok(Self { 0: parsed })
    }
}

impl fmt::Debug for ExportEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({})", self)
    }
}

impl fmt::Display for ExportEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl str::FromStr for ExportEndpoint {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

impl convert::TryFrom<&CStr> for ExportEndpoint {
    type Error = Error;

    fn try_from(raw: &CStr) -> Result<Self, Error> {
        raw.to_str()?.parse()
    }
}

#[derive(PartialEq)]
pub struct ExportProtocol(otlp::Protocol);

impl ExportProtocol {
    pub fn from_ptr(raw: &*const std::ffi::c_char) -> Result<Option<Self>, Error> {
        match (!raw.is_null()).then(|| unsafe { CStr::from_ptr(*raw) }) {
            None => Ok(None),
            Some(cstr) if cstr.is_empty() => Ok(None),
            Some(cstr) => cstr.try_into().and_then(|_self| Ok(Some(_self))),
        }
    }

    fn new(raw: &str) -> Result<Self, Error> {
        let lower = raw.to_lowercase();

        Ok(Self {
            0: match lower.as_str() {
                "grpc" => Ok(otlp::Protocol::Grpc),
                "http/protobuf" => Ok(otlp::Protocol::HttpBinary),
                "http/json" => Ok(otlp::Protocol::HttpJson),
                _ => Err(Error::ExportProtocol(raw.to_owned())),
            }?,
        })
    }
}

impl fmt::Debug for ExportProtocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({})", self)
    }
}

impl fmt::Display for ExportProtocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self.0 {
            otlp::Protocol::Grpc => "grpc",
            otlp::Protocol::HttpBinary => "http/protobuf",
            otlp::Protocol::HttpJson => "http/json",
        })
    }
}

impl str::FromStr for ExportProtocol {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

impl convert::Into<otlp::Protocol> for ExportProtocol {
    fn into(self) -> otlp::Protocol {
        self.0
    }
}

impl convert::TryFrom<&CStr> for ExportProtocol {
    type Error = Error;

    fn try_from(raw: &CStr) -> Result<Self, Self::Error> {
        raw.to_str()?.parse()
    }
}

#[derive(Debug, enumset::EnumSetType)]
#[non_exhaustive]
pub enum ExportSignal {
    Logs,
    Metrics,
    Traces,
}
#[derive(Debug, PartialEq)]
pub struct ExportSignalSet(EnumSet<ExportSignal>);

impl ExportSignalSet {
    pub fn contains(&self, signal: ExportSignal) -> bool {
        self.0.contains(signal)
    }

    pub fn empty() -> Self {
        ExportSignalSet(EnumSet::empty())
    }

    pub fn from_ptr(raw: &*const std::ffi::c_char) -> Result<Self, Error> {
        match (!raw.is_null()).then(|| unsafe { CStr::from_ptr(*raw) }) {
            None => Ok(Self::empty()),
            Some(cstr) => cstr.try_into(),
        }
    }

    fn new(raw: &str) -> Result<Self, Error> {
        let parsed: Result<Vec<ExportSignal>, _> = raw
            .split(',')
            .filter_map(|item| {
                let trimmed = item.trim_matches(|c: char| c == ',' || c.is_whitespace());
                let lower = trimmed.to_lowercase();

                match lower.as_str() {
                    "" => None,
                    "log" | "logs" => Some(Ok(ExportSignal::Logs)),
                    _ => Some(Err(Error::ExportSignal(trimmed.to_owned()))),
                }
            })
            .collect();

        Ok(EnumSet::from_iter(parsed?).into())
    }
}

impl convert::From<EnumSet<ExportSignal>> for ExportSignalSet {
    fn from(other: EnumSet<ExportSignal>) -> Self {
        ExportSignalSet(other)
    }
}

impl str::FromStr for ExportSignalSet {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

impl convert::TryFrom<&CStr> for ExportSignalSet {
    type Error = Error;

    fn try_from(raw: &CStr) -> Result<Self, Self::Error> {
        raw.to_str()?.parse()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use rstest::rstest;

    mod export_compression {
        use super::{ExportCompression, rstest};
        use opentelemetry_otlp::Compression as otlp;

        #[rstest]
        #[case(otlp::Gzip, "(gzip)")]
        #[case(otlp::Zstd, "(zstd)")]
        fn debug(#[case] input: otlp, #[case] expected: &str) {
            assert_eq!(format!("{:?}", ExportCompression(input)), expected);
        }

        #[rstest]
        #[case(otlp::Gzip, "gzip")]
        #[case(otlp::Zstd, "zstd")]
        fn display(#[case] input: otlp, #[case] expected: &str) {
            assert_eq!(format!("{}", ExportCompression(input)), expected);
        }

        #[rstest]
        #[case(otlp::Gzip)]
        #[case(otlp::Zstd)]
        fn from_into(#[case] expected: otlp) {
            assert_eq!(expected, ExportCompression(expected).into());
        }

        #[rstest]
        #[case::empty(c"".as_ptr())]
        #[case::null(std::ptr::null())]
        fn from_ptr_none(#[case] input: *const std::ffi::c_char) {
            assert_eq!(ExportCompression::from_ptr(&input).unwrap(), None);
        }

        #[rstest]
        #[case::wrong(c"other")]
        #[case::not_utf8(c"\xf0\x28\x8c\x28")]
        fn from_ptr_invalid(#[case] input: &std::ffi::CStr) {
            let result = ExportCompression::from_ptr(&input.as_ptr());
            assert!(result.is_err());
        }

        #[rstest]
        #[case::correct(c"gzip")]
        #[case::correct(c"zstd")]
        fn from_ptr_valid(#[case] input: &std::ffi::CStr) {
            let result = ExportCompression::from_ptr(&input.as_ptr());
            assert!(result.unwrap().is_some());
        }

        #[rstest]
        #[case::empty("")]
        #[case::wrong("other")]
        fn parse_invalid(#[case] input: &str) {
            assert!(input.parse::<ExportCompression>().is_err());
        }

        #[rstest]
        #[case("gzip", otlp::Gzip)]
        #[case("GZIP", otlp::Gzip)]
        #[case("zstd", otlp::Zstd)]
        #[case("ZStd", otlp::Zstd)]
        fn parse_valid(#[case] input: &str, #[case] expected: otlp) {
            assert_eq!(ExportCompression(expected), input.parse().unwrap());
        }
    }

    mod export_endpoint {
        use super::{ExportEndpoint, rstest};

        #[rstest]
        #[case("http://localhost", "(http://localhost/)")]
        #[case("https://[::1]:9999/ingest", "(https://[::1]:9999/ingest)")]
        fn debug(#[case] input: &str, #[case] expected: &str) {
            assert_eq!(format!("{:?}", ExportEndpoint::new(input).unwrap()), expected);
        }

        #[rstest]
        #[case("http://localhost", "http://localhost/")]
        #[case("https://[::1]:9999/ingest", "https://[::1]:9999/ingest")]
        fn display(#[case] input: &str, #[case] expected: &str) {
            assert_eq!(format!("{}", ExportEndpoint::new(input).unwrap()), expected);
        }

        #[rstest]
        #[case::empty(c"".as_ptr())]
        #[case::null(std::ptr::null())]
        fn from_ptr_none(#[case] input: *const std::ffi::c_char) {
            assert_eq!(ExportEndpoint::from_ptr(&input).unwrap(), None);
        }

        #[rstest]
        #[case::not_url(c"other")]
        #[case::not_utf8(c"\xf0\x28\x8c\x28")]
        #[case::wrong_scheme(c"ftp://localhost")]
        fn from_ptr_invalid(#[case] input: &std::ffi::CStr) {
            let result = ExportEndpoint::from_ptr(&input.as_ptr());
            assert!(result.is_err());
        }

        #[rstest]
        #[case::correct(c"http://localhost")]
        #[case::correct(c"https://[::1]:9999")]
        fn from_ptr_valid(#[case] input: &std::ffi::CStr) {
            let result = ExportEndpoint::from_ptr(&input.as_ptr());
            assert!(result.unwrap().is_some());
        }

        #[rstest]
        #[case::empty("")]
        #[case::not_url("other")]
        #[case::wrong_scheme("ftp://localhost")]
        fn parse_invalid(#[case] input: &str) {
            assert!(input.parse::<ExportEndpoint>().is_err());
        }

        #[rstest]
        #[case::correct("http://localhost")]
        #[case::correct("https://[::1]:9999")]
        fn parse_valid(#[case] input: &str) {
            assert!(input.parse::<ExportEndpoint>().is_ok());
        }
    }

    mod export_protocol {
        use super::{ExportProtocol, rstest};
        use opentelemetry_otlp::Protocol as otlp;

        #[rstest]
        #[case(otlp::Grpc, "(grpc)")]
        #[case(otlp::HttpBinary, "(http/protobuf)")]
        #[case(otlp::HttpJson, "(http/json)")]
        fn debug(#[case] input: otlp, #[case] expected: &str) {
            assert_eq!(format!("{:?}", ExportProtocol(input)), expected);
        }

        #[rstest]
        #[case(otlp::Grpc, "grpc")]
        #[case(otlp::HttpBinary, "http/protobuf")]
        #[case(otlp::HttpJson, "http/json")]
        fn display(#[case] input: otlp, #[case] expected: &str) {
            assert_eq!(format!("{}", ExportProtocol(input)), expected);
        }

        #[rstest]
        #[case(otlp::Grpc)]
        #[case(otlp::HttpBinary)]
        #[case(otlp::HttpJson)]
        fn from_into(#[case] expected: otlp) {
            assert_eq!(expected, ExportProtocol(expected).into());
        }

        #[rstest]
        #[case::empty(c"".as_ptr())]
        #[case::null(std::ptr::null())]
        fn from_ptr_none(#[case] input: *const std::ffi::c_char) {
            assert_eq!(ExportProtocol::from_ptr(&input).unwrap(), None);
        }

        #[rstest]
        #[case::wrong(c"other")]
        #[case::not_utf8(c"\xf0\x28\x8c\x28")]
        fn from_ptr_invalid(#[case] input: &std::ffi::CStr) {
            let result = ExportProtocol::from_ptr(&input.as_ptr());
            assert!(result.is_err());
        }

        #[rstest]
        #[case::correct(c"grpc")]
        #[case::correct(c"http/json")]
        #[case::correct(c"http/protobuf")]
        fn from_ptr_valid(#[case] input: &std::ffi::CStr) {
            let result = ExportProtocol::from_ptr(&input.as_ptr());
            assert!(result.unwrap().is_some());
        }

        #[rstest]
        #[case::empty("")]
        #[case::wrong("other")]
        fn parse_invalid(#[case] input: &str) {
            assert!(input.parse::<ExportProtocol>().is_err());
        }

        #[rstest]
        #[case("grpc", otlp::Grpc)]
        #[case("GRpc", otlp::Grpc)]
        #[case("http/json", otlp::HttpJson)]
        #[case("http/JSON", otlp::HttpJson)]
        #[case("http/protobuf", otlp::HttpBinary)]
        fn parse_valid(#[case] input: &str, #[case] expected: otlp) {
            assert_eq!(ExportProtocol(expected), input.parse().unwrap());
        }
    }

    mod export_signal_set {
        use super::{ExportSignal::*, ExportSignalSet, rstest};
        use enumset::EnumSet;

        #[test]
        fn contains() {
            assert_eq!(false, ExportSignalSet::empty().contains(Logs));
            assert_eq!(false, ExportSignalSet::empty().contains(Metrics));
            assert_eq!(false, ExportSignalSet::new("").unwrap().contains(Logs));
            assert_eq!(false, ExportSignalSet::new("").unwrap().contains(Metrics));
            assert_eq!(true, ExportSignalSet::new("logs").unwrap().contains(Logs));
            assert_eq!(false, ExportSignalSet::new("logs").unwrap().contains(Metrics));
        }

        #[test]
        fn debug() {
            assert_eq!(format!("{:?}", ExportSignalSet::empty()), "ExportSignalSet(EnumSet())");
            assert_eq!(
                format!("{:?}", ExportSignalSet::from(EnumSet::empty() | Logs)),
                "ExportSignalSet(EnumSet(Logs))"
            );
            assert_eq!(
                format!("{:?}", ExportSignalSet::from(EnumSet::empty() | Logs | Metrics)),
                "ExportSignalSet(EnumSet(Logs | Metrics))"
            );
        }

        #[test]
        fn empty() {
            assert_eq!(ExportSignalSet::empty(), ExportSignalSet::from(EnumSet::empty()));
        }

        #[rstest]
        #[case::all_wrong(c"other")]
        #[case::one_wrong(c"logs, other")]
        #[case::not_utf8(c"\xf0\x28\x8c\x28")]
        fn from_ptr_invalid(#[case] input: &std::ffi::CStr) {
            let result = ExportSignalSet::from_ptr(&input.as_ptr());
            assert!(result.is_err());
        }

        #[rstest]
        #[case::null(std::ptr::null(), EnumSet::empty())]
        #[case::empty(c"".as_ptr(), EnumSet::empty())]
        #[case(c"logs".as_ptr(), EnumSet::empty() | Logs)]
        #[case(c"log, logs".as_ptr(), EnumSet::empty() | Logs)]
        fn from_ptr_valid(
            #[case] input: *const std::ffi::c_char,
            #[case] expected: EnumSet<super::ExportSignal>,
        ) {
            let result = ExportSignalSet::from_ptr(&input);
            assert_eq!(ExportSignalSet::from(expected), result.unwrap());
        }

        #[rstest]
        #[case::all_wrong("other")]
        #[case::one_wrong("logs, other")]
        fn parse_invalid(#[case] input: &str) {
            assert!(input.parse::<ExportSignalSet>().is_err());
        }

        #[rstest]
        #[case("", EnumSet::empty())]
        #[case("logs", EnumSet::empty() | Logs)]
        #[case("LOG", EnumSet::empty() | Logs)]
        #[case("LOGS", EnumSet::empty() | Logs)]
        #[case("log, logs", EnumSet::empty() | Logs)]
        #[case("log, log, log", EnumSet::empty() | Logs)]
        fn parse_valid(#[case] input: &str, #[case] expected: EnumSet<super::ExportSignal>) {
            assert_eq!(ExportSignalSet::from(expected), input.parse().unwrap());
        }
    }
}
