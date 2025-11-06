// SPDX-License-Identifier: ISC

use super::Error;
use opentelemetry_otlp as otlp;
use std::ffi::CStr;
use std::{convert, fmt, str};

#[derive(PartialEq)]
pub struct ExportCompression(otlp::Compression);

impl ExportCompression {
    pub fn from_ptr(raw: &*const std::ffi::c_char) -> Result<Option<Self>, Error> {
        match (!raw.is_null()).then(|| unsafe { CStr::from_ptr(*raw) }) {
            None => Ok(None),
            Some(cstr) if cstr.is_empty() => Ok(None),
            Some(cstr) => cstr.try_into().map(Some),
        }
    }

    fn new(raw: &str) -> Result<Self, Error> {
        let lower = raw.to_lowercase();

        Ok(Self(lower.parse().map_err(Error::ExportCompression)?))
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

impl convert::From<otlp::Compression> for ExportCompression {
    fn from(raw: otlp::Compression) -> Self {
        Self(raw)
    }
}

impl convert::From<ExportCompression> for otlp::Compression {
    fn from(value: ExportCompression) -> otlp::Compression {
        value.0
    }
}

#[cfg(test)]
mod test {
    use super::ExportCompression;
    use googletest::prelude::*;
    use opentelemetry_otlp::Compression as otlp;
    use rstest::rstest;

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
        assert_that!(ExportCompression(input), displays_as(eq(expected)));
    }

    #[rstest]
    #[case(otlp::Gzip)]
    #[case(otlp::Zstd)]
    fn from_into(#[case] expected: otlp) {
        assert_eq!(expected, ExportCompression(expected).into());
        assert_eq!(ExportCompression(expected), expected.into());
    }

    #[rstest]
    #[case::empty(c"".as_ptr())]
    #[case::null(std::ptr::null())]
    fn from_ptr_none(#[case] input: *const std::ffi::c_char) {
        assert_that!(ExportCompression::from_ptr(&input), ok(none()));
    }

    #[rstest]
    #[case::wrong(c"other")]
    #[case::not_utf8(c"\xf0\x28\x8c\x28")]
    fn from_ptr_invalid(#[case] input: &std::ffi::CStr) {
        assert_that!(ExportCompression::from_ptr(&input.as_ptr()), err(anything()));
    }

    #[rstest]
    #[case::correct(c"gzip")]
    #[case::correct(c"zstd")]
    fn from_ptr_valid(#[case] input: &std::ffi::CStr) {
        assert_that!(ExportCompression::from_ptr(&input.as_ptr()), ok(some(anything())));
    }

    #[rstest]
    #[case::empty("")]
    #[case::wrong("other")]
    fn parse_invalid(#[case] input: &str) {
        assert_that!(input.parse::<ExportCompression>(), err(anything()));
    }

    #[rstest]
    #[case("gzip", otlp::Gzip)]
    #[case("GZIP", otlp::Gzip)]
    #[case("zstd", otlp::Zstd)]
    #[case("ZStd", otlp::Zstd)]
    fn parse_valid(#[case] input: &str, #[case] expected: otlp) {
        assert_that!(input.parse::<ExportCompression>(), ok(eq(&ExportCompression(expected))));
    }
}
