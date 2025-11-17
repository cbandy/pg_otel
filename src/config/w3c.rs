// SPDX-License-Identifier: ISC

use super::Error;
use opentelemetry as otel;
use std::{convert, ffi, str};

#[derive(Debug, Clone)]
pub struct Baggage(Vec<otel::KeyValue>);

/// This is a set of key-value pairs in the format of a W3C Baggage string: https://www.w3.org/TR/baggage
///
/// Properties of a value are included in the value: https://www.w3.org/TR/baggage#property
impl Baggage {
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn try_from_ptr(raw: &*const ffi::c_char) -> Result<Self, Error> {
        match (!raw.is_null()).then(|| unsafe { ffi::CStr::from_ptr(*raw) }) {
            None => return Ok(Self::empty()),
            Some(cstr) => cstr.try_into(),
        }
    }

    fn new(unparsed: &str) -> Result<Self, Error> {
        let mut inner = Vec::new();

        for item in unparsed.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }

            let Some((k, v)) = item.split_once('=') else {
                return Err(Error::Baggage(format!("missing value for key {item:?}")));
            };

            let k = k.trim();
            if k.is_empty() {
                return Err(Error::Baggage(format!("missing key in {item:?}")));
            }

            inner.push(otel::KeyValue::new(k.to_owned(), v.trim().to_owned()));
        }

        Ok(Self(inner))
    }
}

impl str::FromStr for Baggage {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Self::new(s)
    }
}

impl convert::TryFrom<&ffi::CStr> for Baggage {
    type Error = Error;

    fn try_from(raw: &ffi::CStr) -> Result<Self, Error> {
        raw.to_str()?.parse()
    }
}

impl convert::TryFrom<Baggage> for http::HeaderMap {
    type Error = Error;

    fn try_from(value: Baggage) -> Result<Self, Error> {
        let mut result = Self::with_capacity(value.0.len());

        for kv in &value.0 {
            let Ok(k) = kv.key.as_str().parse::<http::HeaderName>() else {
                return Err(Error::Baggage(format!("invalid header name: {:?}", kv.key)));
            };
            let Ok(v) = kv.value.as_str().parse::<http::HeaderValue>() else {
                return Err(Error::Baggage(format!("invalid header value: {:?}", kv.value)));
            };
            result.append(k, v);
        }

        Ok(result)
    }
}

impl core::iter::IntoIterator for Baggage {
    type Item = otel::KeyValue;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::Baggage;
    use googletest::prelude::*;
    use opentelemetry as otel;
    use rstest::rstest;

    #[rstest]
    #[case::no_key("=")]
    #[case::no_key("=b")]
    #[case::no_value("a")]
    #[case::no_value("a=b, c")]
    fn parse_invalid(#[case] input: &str) {
        let cstr = std::ffi::CString::new(input).unwrap();

        assert_that!(input.parse::<Baggage>(), err(anything()));
        assert_that!(TryInto::<Baggage>::try_into(cstr.as_c_str()), err(anything()));
    }

    #[rstest]
    #[case::empty("", &[])]
    #[case::one("ab=cd", &[
        otel::KeyValue::new("ab","cd")
    ])]
    #[case::two("x=y, a = b", &[
        otel::KeyValue::new("x","y"),
        otel::KeyValue::new("a","b")
    ])]
    #[case::one_comma(",", &[])]
    #[case::all_commas(",,,", &[])]
    #[case::extra_commas(",,ab=cd,e=f ,  ,,", &[
        otel::KeyValue::new("ab","cd"),
        otel::KeyValue::new("e","f")
    ])]
    fn parse_valid(#[case] input: &str, #[case] expected: &[otel::KeyValue]) {
        let actual = input.parse::<Baggage>().unwrap();

        assert_that!(actual.0, eq(expected));
        assert_that!(actual.into_iter().collect::<Vec<_>>(), eq(expected));
    }

    #[rstest]
    #[case::empty("", 0, http::HeaderMap::default())]
    #[case::one("ab=cd", 1,
        http::HeaderMap::from_iter([
            ("ab".try_into().unwrap(),"cd".try_into().unwrap())
        ])
    )]
    #[case::two("x=y, a = b", 2,
        http::HeaderMap::from_iter([
            ("x".try_into().unwrap(),"y".try_into().unwrap()),
            ("a".try_into().unwrap(),"b".try_into().unwrap())
        ])
    )]
    #[case::duplicate_key("a=b,a=c", 2,
        http::HeaderMap::from_iter([
            ("a".try_into().unwrap(),"b".try_into().unwrap()),
            ("a".try_into().unwrap(),"c".try_into().unwrap())
        ])
    )]
    fn into_header_map(
        #[case] input: &str,
        #[case] length: usize,
        #[case] values: http::HeaderMap,
    ) {
        let result = TryInto::<http::HeaderMap>::try_into(Baggage::new(input).unwrap());

        assert_that!(result, ok(matches_pattern!(&http::HeaderMap { len(): eq(length) })));
        assert_that!(result, ok(eq(&values)));
    }

    #[rstest]
    #[case::invisible_ascii_key("a\nb = cd")]
    #[case::invisible_ascii_value("ab = c\nd")]
    #[case::unicode_key("🌟 = b")]
    fn into_header_map_invalid(#[case] input: &str) {
        let parsed = Baggage::new(input).unwrap();

        assert_that!(TryInto::<http::HeaderMap>::try_into(parsed), err(anything()));
    }

    #[rstest]
    #[case::visible_ascii("a = b, c=d, ef = gg")]
    #[case::unicode_value("a = 🌟")]
    fn into_header_map_valid(#[case] input: &str) {
        let parsed = Baggage::new(input).unwrap();

        assert_that!(TryInto::<http::HeaderMap>::try_into(parsed), ok(anything()));
    }

    #[rstest]
    #[case::null(std::ptr::null())]
    #[case::blank(c"".as_ptr())]
    fn try_from_ptr_empty(#[case] input: *const std::ffi::c_char) {
        assert_that!(Baggage::empty().0, elements_are!());
        assert_that!(Baggage::try_from_ptr(&input).unwrap().0, elements_are!());
    }

    #[rstest]
    #[case::no_key(c"=b")]
    #[case::no_value(c"a")]
    #[case::no_value(c"a=b, c")]
    #[case::not_utf8(c"\xf0\x28\x8c\x28")]
    fn try_from_ptr_invalid(#[case] input: &std::ffi::CStr) {
        assert_that!(Baggage::try_from_ptr(&input.as_ptr()), err(anything()));
    }

    #[rstest]
    #[case::null(std::ptr::null())]
    #[case::empty(c"".as_ptr())]
    #[case::visible_ascii(c"a = b, c=d, ef = gg".as_ptr())]
    #[case::invisible_ascii_key(c"a\nb = cd".as_ptr())]
    #[case::unicode_key(c"🌟 = b".as_ptr())]
    #[case::unicode_value(c"a = 🌟".as_ptr())]
    fn try_from_ptr_valid(#[case] input: *const std::ffi::c_char) {
        assert_that!(Baggage::try_from_ptr(&input), ok(anything()));
    }
}
