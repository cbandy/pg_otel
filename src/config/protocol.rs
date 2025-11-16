// SPDX-License-Identifier: ISC

use super::Error;
use opentelemetry_otlp as otlp;
use std::{convert, ffi, fmt, str};

#[derive(PartialEq)]
pub struct ExportProtocol(otlp::Protocol);

impl ExportProtocol {
    fn new(raw: &str) -> Result<Self, Error> {
        let lower = raw.to_lowercase();

        Ok(Self(match lower.as_str() {
            "grpc" => Ok(otlp::Protocol::Grpc),
            "http/protobuf" => Ok(otlp::Protocol::HttpBinary),
            "http/json" => Ok(otlp::Protocol::HttpJson),
            _ => Err(Error::ExportProtocol(raw.to_owned())),
        }?))
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
    fn from(value: otlp::Protocol) -> Self {
        Self(value)
    }
}

impl convert::From<ExportProtocol> for otlp::Protocol {
    fn from(value: ExportProtocol) -> otlp::Protocol {
        value.0
    }
}

impl convert::TryFrom<&ffi::CStr> for ExportProtocol {
    type Error = Error;

    fn try_from(raw: &ffi::CStr) -> Result<Self, Self::Error> {
        raw.to_str()?.parse()
    }
}

impl super::FromStr<'_> for ExportProtocol {
    fn try_from_ptr(raw: &*const ffi::c_char) -> Result<Option<Self>, Error> {
        match (!raw.is_null()).then(|| unsafe { ffi::CStr::from_ptr(*raw) }) {
            None => Ok(None),
            Some(cstr) if cstr.is_empty() => Ok(None),
            Some(cstr) => cstr.try_into().map(Some),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{super::FromStr, ExportProtocol};
    use googletest::prelude::*;
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
        assert_that!(ExportProtocol(input), displays_as(eq(expected)));
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
    #[case::empty("")]
    #[case::wrong("other")]
    fn parse_invalid(#[case] input: &str) {
        assert_that!(input.parse::<ExportProtocol>(), err(anything()));
    }

    #[rstest]
    #[case("grpc", otlp::Grpc)]
    #[case("GRpc", otlp::Grpc)]
    #[case("http/json", otlp::HttpJson)]
    #[case("http/JSON", otlp::HttpJson)]
    #[case("http/protobuf", otlp::HttpBinary)]
    fn parse_valid(#[case] input: &str, #[case] expected: otlp) {
        assert_that!(input.parse::<ExportProtocol>(), ok(eq(&ExportProtocol(expected))));
    }

    #[rstest]
    #[case::empty(c"".as_ptr())]
    #[case::null(std::ptr::null())]
    fn try_from_ptr_none(#[case] input: *const std::ffi::c_char) {
        assert_that!(ExportProtocol::try_from_ptr(&input), ok(none()));
    }

    #[rstest]
    #[case::wrong(c"other")]
    #[case::not_utf8(c"\xf0\x28\x8c\x28")]
    fn try_from_ptr_invalid(#[case] input: &std::ffi::CStr) {
        assert_that!(ExportProtocol::try_from_ptr(&input.as_ptr()), err(anything()));
    }

    #[rstest]
    #[case::correct(c"grpc")]
    #[case::correct(c"http/json")]
    #[case::correct(c"http/protobuf")]
    fn try_from_ptr_valid(#[case] input: &std::ffi::CStr) {
        assert_that!(ExportProtocol::try_from_ptr(&input.as_ptr()), ok(some(anything())));
    }
}
