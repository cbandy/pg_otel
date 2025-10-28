// SPDX-License-Identifier: ISC

use super::Error;
use http::uri;
use std::ffi::CStr;
use std::{convert, fmt, str};

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

#[cfg(test)]
mod test {
    use super::ExportEndpoint;
    use googletest::prelude::*;
    use rstest::rstest;

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
        assert_that!(ExportEndpoint::new(input), ok(displays_as(eq(expected))));
    }

    #[rstest]
    #[case::empty(c"".as_ptr())]
    #[case::null(std::ptr::null())]
    fn from_ptr_none(#[case] input: *const std::ffi::c_char) {
        assert_that!(ExportEndpoint::from_ptr(&input), ok(none()));
    }

    #[rstest]
    #[case::not_url(c"other")]
    #[case::not_utf8(c"\xf0\x28\x8c\x28")]
    #[case::wrong_scheme(c"ftp://localhost")]
    fn from_ptr_invalid(#[case] input: &std::ffi::CStr) {
        assert_that!(ExportEndpoint::from_ptr(&input.as_ptr()), err(anything()));
    }

    #[rstest]
    #[case::correct(c"http://localhost")]
    #[case::correct(c"https://[::1]:9999")]
    fn from_ptr_valid(#[case] input: &std::ffi::CStr) {
        assert_that!(ExportEndpoint::from_ptr(&input.as_ptr()), ok(some(anything())));
    }

    #[rstest]
    #[case::empty("")]
    #[case::not_url("other")]
    #[case::wrong_scheme("ftp://localhost")]
    fn parse_invalid(#[case] input: &str) {
        assert_that!(input.parse::<ExportEndpoint>(), err(anything()));
    }

    #[rstest]
    #[case::correct("http://localhost")]
    #[case::correct("https://[::1]:9999")]
    fn parse_valid(#[case] input: &str) {
        assert_that!(input.parse::<ExportEndpoint>(), ok(anything()));
    }
}
