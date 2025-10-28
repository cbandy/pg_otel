// SPDX-License-Identifier: ISC

use super::Error;
use opentelemetry_otlp as otlp;
use std::ffi::CStr;
use std::{convert, fmt, str};

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

impl convert::From<otlp::Protocol> for ExportProtocol {
    fn from(raw: otlp::Protocol) -> Self {
        Self(raw)
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

#[cfg(test)]
mod test {
    use super::ExportProtocol;
    use opentelemetry_otlp::Protocol as otlp;
    use rstest::rstest;

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
        assert_eq!(ExportProtocol(expected), expected.into());
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
