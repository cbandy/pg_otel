// SPDX-License-Identifier: ISC

use super::Error;
use enumset::EnumSet;
use std::ffi::CStr;
use std::{convert, str};

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
    use super::{ExportSignal::*, ExportSignalSet};
    use enumset::EnumSet;
    use googletest::prelude::*;
    use rstest::rstest;

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
        assert_that!(ExportSignalSet::from_ptr(&input.as_ptr()), err(anything()));
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
        assert_that!(ExportSignalSet::from_ptr(&input), ok(eq(&ExportSignalSet::from(expected))));
    }

    #[rstest]
    #[case::all_wrong("other")]
    #[case::one_wrong("logs, other")]
    fn parse_invalid(#[case] input: &str) {
        assert_that!(input.parse::<ExportSignalSet>(), err(anything()));
    }

    #[rstest]
    #[case("", EnumSet::empty())]
    #[case("logs", EnumSet::empty() | Logs)]
    #[case("LOG", EnumSet::empty() | Logs)]
    #[case("LOGS", EnumSet::empty() | Logs)]
    #[case("log, logs", EnumSet::empty() | Logs)]
    #[case("log, log, log", EnumSet::empty() | Logs)]
    fn parse_valid(#[case] input: &str, #[case] expected: EnumSet<super::ExportSignal>) {
        assert_that!(input.parse::<ExportSignalSet>(), ok(eq(&ExportSignalSet::from(expected))));
    }
}
