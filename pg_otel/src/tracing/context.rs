// SPDX-License-Identifier: MIT

pub use opentelemetry::{SpanId, TraceFlags, TraceId};

type Str<'a> = std::borrow::Cow<'a, str>;

#[derive(Debug, PartialEq)]
pub struct Headers<'a>(Str<'a>, Option<Str<'a>>);

#[derive(Clone, Debug, PartialEq)]
pub struct SpanContext<'a> {
    pub id: SpanId,
    pub trace: std::borrow::Cow<'a, TraceContext<'a>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceContext<'a> {
    pub id: TraceId,
    pub flags: TraceFlags,
    pub state: Option<Str<'a>>,
}

impl<'a> SpanContext<'a> {
    pub fn into_owned(self) -> SpanContext<'static> {
        SpanContext {
            id: self.id,
            trace: std::borrow::Cow::Owned(TraceContext {
                id: self.trace.id,
                flags: self.trace.flags,
                state: self
                    .trace
                    .state
                    .as_ref()
                    .map(|s| std::borrow::Cow::Owned(s.to_string())),
            }),
        }
    }
}

impl<'a> std::convert::TryFrom<Headers<'a>> for SpanContext<'a> {
    type Error = ();

    fn try_from(value: Headers<'a>) -> Result<Self, Self::Error> {
        let Headers(traceparent, tracestate) = value;
        let (version, rest) = traceparent.split_once('-').ok_or(())?;
        match version {
            "00" => {
                let (trace, rest) = rest.split_once('-').ok_or(())?;
                let (span, flags) = rest.split_once('-').ok_or(())?;

                if trace.len() != 32 || span.len() != 16 || flags.len() != 2 {
                    return Err(());
                }

                Ok(SpanContext {
                    id: SpanId::from_hex(span).map_err(|_| ())?,
                    trace: std::borrow::Cow::Owned(TraceContext {
                        id: TraceId::from_hex(trace).map_err(|_| ())?,
                        flags: TraceFlags::new(u8::from_str_radix(flags, 16).map_err(|_| ())?),
                        state: tracestate,
                    }),
                })
            }
            _ => Err(()),
        }
    }
}

impl<'a> Headers<'a> {
    #[cfg(test)]
    fn new(traceparent: &'a str, tracestate: Option<&'a str>) -> Self {
        Headers(Str::Borrowed(traceparent), tracestate.map(Str::Borrowed))
    }

    /// Extracts W3C `traceparent` and `tracestate` headers from comma-separated key-value pairs
    /// emitted by [SQLCommenter](https://google.github.io/sqlcommenter).
    fn from_comment(comment: &'a str) -> Option<Headers<'a>> {
        let mut traceparent = None;
        let mut tracestate = None;

        for (k, v) in CommenterIterator::new(comment) {
            if k == "tracestate" {
                tracestate = Some(v);
            } else if k == "traceparent" {
                traceparent = Some(v);
            }
        }

        Some(Headers(traceparent?, tracestate))
    }

    /// Finds W3C `traceparent` and `tracestate` headers inside a leading or trailing SQL comment.
    ///
    /// Compatible with [SQLCommenter](https://google.github.io/sqlcommenter) which adds W3C trace
    /// context as comma-separated key-value pairs inside a C-style block comment; `/* … */`
    fn from_sql(sql: &'a str) -> Option<Headers<'a>> {
        // Look for a trailing comment by removing any trailing whitespace and semicolons.
        let trailing = sql.trim_end_matches(|c: char| c.is_whitespace() || c == ';');
        if let Some(comment) = trailing.strip_suffix("*/")
            && let Some((_, comment)) = comment.rsplit_once("/*")
            && let Some(headers) = Self::from_comment(comment)
        {
            return Some(headers);
        }

        // Look for a leading comment by removing any leading whitespace.
        let leading = sql.trim_start();
        if let Some(comment) = leading.strip_prefix("/*")
            && let Some((comment, _)) = comment.split_once("*/")
            && let Some(headers) = Self::from_comment(comment)
        {
            return Some(headers);
        }

        None
    }

    /// Finds W3C `traceparent` and `tracestate` headers in SQL comments *between* SQL statements.
    pub fn from_statement(sql: &'a str, stmt_begin: i32, stmt_length: i32) -> Option<Headers<'a>> {
        // The [pg_sys::PlannedStmt] passed to hooks may not have a denoted beginning or end.
        // In that case, look at the beginning and end of the entire SQL text.
        if stmt_begin < 0 || stmt_length <= 0 {
            return Self::from_sql(sql);
        }

        let stmt_end = stmt_begin + stmt_length;

        // Look for a comment in the area behind the statement
        if let Some((_, behind)) = sql.split_at_checked(stmt_end as usize)
            && let Some(parsed) = Self::from_sql(behind.split_once(';').map_or(behind, |(s, _)| s))
        {
            return Some(parsed);
        }

        // Look for a comment in the area ahead of the statement
        if let Some((ahead, _)) = sql.split_at_checked(stmt_begin as usize)
            && let Some(parsed) = Self::from_sql(ahead.rsplit_once(';').map_or(ahead, |(_, s)| s))
        {
            return Some(parsed);
        }

        // debug_assert!(stmt_begin >= 0);
        // debug_assert!(stmt_begin < stmt_end);
        // if stmt_begin == 0 {
        //     return Self::from_sql(&sql[..stmt_end.min(sql.len() as i32) as usize]);
        // }

        None
    }
}

/// A zero-copy iterator over comma-separated key-value pairs in an [SQLCommenter](https://google.github.io/sqlcommenter/spec) comment body.
///
/// - Key-value pairs are separated by commas `,`.
/// - Keys and values are separated by `=`.
/// - Values are enclosed in single quotes `'…'` (or double quotes `"…"`).
/// - Special characters are percent-encoded then backslash-escaped.
///
/// Input:
/// `traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',tracestate='congo%3Dt61rcWkgMzE'`
///
/// Yields:
/// - `("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")`
/// - `("tracestate", "congo=t61rcWkgMzE")`
struct CommenterIterator<'a>(&'a str);

impl<'a> CommenterIterator<'a> {
    fn new(input: &'a str) -> Self {
        Self(input.trim())
    }
}

impl<'a> CommenterIterator<'a> {
    fn is_encoding(c: char) -> bool {
        // Unreserved per RFC 3986 § 2 or percent U+0025 or backslash U+005C escape.
        c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '\\' | '_' | '~' | '%')
    }

    // Reverses the double-encoding defined by [SQLCommenter](https://google.github.io/sqlcommenter/spec#parsing).
    fn decode(encoded: &'a str) -> Str<'a> {
        Self::reverse_percent_encoding(Self::reverse_backslashes(Str::Borrowed(encoded)))
    }

    fn reverse_backslashes(encoded: Str<'a>) -> Str<'a> {
        let Some((head, mut tail)) = encoded.split_once('\\') else {
            return encoded;
        };

        let mut result = String::with_capacity(encoded.len());
        result.push_str(head);

        loop {
            if let Some(c) = tail.chars().next() {
                result.push(c);
                tail = &tail[c.len_utf8()..];
            } else {
                result.push('\\');
                break;
            }

            if let Some((next_head, next_tail)) = tail.split_once('\\') {
                result.push_str(next_head);
                tail = next_tail;
            } else {
                result.push_str(tail);
                break;
            }
        }

        Str::Owned(result)
    }

    fn reverse_percent_encoding(encoded: Str) -> Str {
        let Some((head, mut tail)) = encoded.split_once('%') else {
            return encoded;
        };

        #[inline(always)]
        fn hex(byte: u8) -> Option<u8> {
            match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            }
        }

        let mut result = String::with_capacity(encoded.len());
        result.push_str(head);

        loop {
            if let (Some(hi), Some(lo)) = (
                tail.bytes().nth(0).and_then(hex),
                tail.bytes().nth(1).and_then(hex),
            ) {
                // Append the decoded value and step past it.
                result.push(((hi << 4) | lo) as char);
                tail = &tail[2..];
            } else {
                // Append '%' when the encoding is invalid or incomplete.
                result.push('%');
            }

            if let Some((head, next)) = tail.split_once('%') {
                result.push_str(head);
                tail = next;
            } else {
                result.push_str(tail);
                return Str::Owned(result);
            }
        }
    }
}

impl<'a> Iterator for CommenterIterator<'a> {
    type Item = (Str<'a>, Str<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // skip whitespace and commas
            self.0 = self
                .0
                .trim_start_matches(|c: char| c.is_whitespace() || c == ',');

            if self.0.is_empty() {
                return None;
            }

            // Locate '=' and grab the encoded key before it.
            let (key, rest) = self.0.split_once('=')?;
            let key = key.trim();
            let key = key
                .rsplit_once(|c: char| !Self::is_encoding(c))
                .map(|(_, k)| k)
                .unwrap_or(key);

            // Move past '=' and retry when the key is bad.
            self.0 = rest.trim_start();
            if key.is_empty() {
                continue;
            }

            if self.0.is_empty() {
                return None;
            }

            let quote = self.0.chars().next()?;
            let (value, next) = if !matches!(quote, '\'' | '"') {
                self.0
                    .split_once(|c: char| c == ',' || c.is_whitespace())
                    .unwrap_or((self.0, ""))
            } else {
                // Move past the open quote, locate ',' then look backward for the close quote.
                let inner = &self.0[1..];
                let (candidate, next) = inner.split_once(',').unwrap_or((inner, ""));

                // Move past ',' and retry when there is no close quote.
                let Some(i) = rfind_quote(candidate.trim_end(), quote) else {
                    self.0 = next;
                    continue;
                };

                (&candidate[..i], &inner[i + 1..])
            };

            self.0 = next;
            return Some((Self::decode(key), Self::decode(value)));
        }
    }
}

/// Locate the byte index of the last `quote` character in `s` that is not escaped by a backslash.
fn rfind_quote(s: &str, quote: char) -> Option<usize> {
    let mut t = s;

    // Jump to the quote then look backward for slashes.
    // An even number of slashes means the quote is not escaped.
    while let Some(i) = t.rfind(quote) {
        let slashes = t[..i].bytes().rev().take_while(|&b| b == b'\\').count();
        if slashes % 2 == 0 {
            return Some(i);
        }

        // The quote is escaped; skip it and retry.
        t = &t[..i];
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{CommenterIterator, Headers, SpanContext, Str, TraceContext};
    use googletest::prelude::*;
    use opentelemetry::{SpanId, TraceFlags, TraceId};

    #[test]
    fn test_decode() {
        // String without encoding is borrowed (zero allocation)
        match CommenterIterator::decode("plain_text") {
            Str::Borrowed(s) => assert_that!(s, eq("plain_text")),
            Str::Owned(_) => panic!("expected borrowed string for plain text"),
        }

        // backslashes
        assert_that!(CommenterIterator::decode(r"foo\'bar"), eq("foo'bar"));
        assert_that!(CommenterIterator::decode(r"\A\x\'"), eq("Ax'"));
        assert_that!(CommenterIterator::decode(r"foo\\bar"), eq(r"foo\bar"));
        assert_that!(CommenterIterator::decode(r"trailing\"), eq(r"trailing\"));

        // invalid percent encoding
        assert_that!(CommenterIterator::decode("foo%ZZbar"), eq("foo%ZZbar"));
        assert_that!(CommenterIterator::decode("foo%2"), eq("foo%2"));
        assert_that!(CommenterIterator::decode("foo%"), eq("foo%"));
        assert_that!(CommenterIterator::decode("foo%ZZ%20bar"), eq("foo%ZZ bar"));
    }

    #[test]
    fn test_commenter_iterator_spec_compliant() {
        let input = "traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',tracestate='congo%3Dt61rcWkgMzE',framework='express%20framework'";

        assert_that!(
            CommenterIterator::new(input).collect::<Vec<_>>(),
            eq(&vec![
                (
                    Str::Borrowed("traceparent"),
                    Str::Borrowed("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
                ),
                (
                    Str::Borrowed("tracestate"),
                    Str::Owned("congo=t61rcWkgMzE".to_string())
                ),
                (
                    Str::Borrowed("framework"),
                    Str::Owned("express framework".to_string())
                )
            ])
        );
    }

    #[test]
    fn test_commenter_iterator_malformed_recovery() {
        let input = "broken='unclosed,tracestate='congo=t61rcWkgMzE',framework='rails'";

        assert_that!(
            CommenterIterator::new(input).collect::<Vec<_>>(),
            eq(&vec![
                (
                    Str::Borrowed("tracestate"),
                    Str::Borrowed("congo=t61rcWkgMzE")
                ),
                (Str::Borrowed("framework"), Str::Borrowed("rails"))
            ])
        );
    }

    #[test]
    fn test_commenter_iterator_unquoted_fallback() {
        let input = "framework=django,action=index";

        assert_that!(
            CommenterIterator::new(input).collect::<Vec<_>>(),
            eq(&vec![
                (Str::Borrowed("framework"), Str::Borrowed("django")),
                (Str::Borrowed("action"), Str::Borrowed("index"))
            ])
        );
    }

    #[test]
    fn test_commenter_iterator_escaping() {
        let input = r"custom_key='foo\'bar',tracestate='congo=t61rcWkgMzE'";

        assert_that!(
            CommenterIterator::new(input).collect::<Vec<_>>(),
            eq(&vec![
                (
                    Str::Borrowed("custom_key"),
                    Str::Owned("foo'bar".to_string())
                ),
                (
                    Str::Borrowed("tracestate"),
                    Str::Borrowed("congo=t61rcWkgMzE")
                )
            ])
        );
    }

    #[test]
    fn test_from_sql() {
        assert_that!(
            Headers::from_sql(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',tracestate='congo=t61rcWkgMzE' */;",
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // no tracestate
        assert_that!(
            Headers::from_sql(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */;",
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None
            ))),
        );

        // bad tracestate
        assert_that!(
            Headers::from_sql("SELECT * FROM users /* traceparent='00-invalid-span-01' */;"),
            some(eq(&Headers::new("00-invalid-span-01", None))),
        );

        // no flags
        assert_that!(
            Headers::from_sql(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7' */;"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
                None
            ))),
        );

        // no comment
        assert_that!(
            Headers::from_sql(
                "SELECT * FROM users WHERE traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01';"
            ),
            none(),
        );

        // no traceparent
        assert_that!(
            Headers::from_sql("SELECT * FROM users /* tracestate='congo=t61rcWkgMzE' */;"),
            none(),
        );
    }

    #[test]
    fn test_from_statement() {
        struct Case<'a> {
            name: &'a str,
            sql: &'a str,
            begin: i32,
            length: i32,
            expected: (Option<&'a str>, Option<&'a str>),
        }

        let cases = [
            //
            // Leading
            //
            Case {
                name: "multi-statement leading: stmt 1 gets its leading comment",
                sql: "/* traceparent='tp1' */ SELECT 1; /* traceparent='tp2' */ SELECT 2;",
                begin: 24, //-----------------^
                length: 8, //-------------------------^
                expected: (Some("tp1"), None),
            },
            Case {
                name: "multi-statement leading: stmt 2 gets its leading comment",
                sql: "/* traceparent='tp1' */ SELECT 1; /* traceparent='tp2' */ SELECT 2;",
                begin: 58, //---------------------------------------------------^
                length: 8, //-----------------------------------------------------------^
                expected: (Some("tp2"), None),
            },
            Case {
                name: "multi-statement leading: stmt 2 without comment does NOT find another",
                sql: "/* traceparent='tp1' */ SELECT 1; SELECT 2;",
                begin: 34, //---------------------------^
                length: 8, //-----------------------------------^
                expected: (None, None),
            },
            //
            // Trailing
            //
            Case {
                name: "multi-statement trailing: stmt 1 gets its trailing comment",
                sql: "SELECT 1 /* traceparent='tp1' */; SELECT 2 /* traceparent='tp2' */;",
                begin: 0,  //
                length: 8, //^
                expected: (Some("tp1"), None),
            },
            Case {
                name: "multi-statement trailing: stmt 2 gets its trailing comment",
                sql: "SELECT 1 /* traceparent='tp1' */; SELECT 2 /* traceparent='tp2' */;",
                begin: 34, //---------------------------^
                length: 8, //-----------------------------------^
                expected: (Some("tp2"), None),
            },
            Case {
                name: "multi-statement trailing: stmt 1 without comment does NOT find another",
                sql: "SELECT 1; SELECT 2 /* traceparent='tp2' */;",
                begin: 0,  //
                length: 8, //^
                expected: (None, None),
            },
            //
            // Mixed, leading and trailing
            //
            Case {
                name: "multi-statement mixed: stmt 1 gets its leading, stmt 2 trailing",
                sql: "/* traceparent='tp1' */ SELECT 1; SELECT 2 /* traceparent='tp2' */;",
                begin: 24, //-----------------^
                length: 8, //-------------------------^
                expected: (Some("tp1"), None),
            },
            Case {
                name: "multi-statement mixed: stmt 2 gets its trailing, stmt 1 leading",
                sql: "/* traceparent='tp1' */ SELECT 1; SELECT 2 /* traceparent='tp2' */;",
                begin: 34, //---------------------------^
                length: 8, //-----------------------------------^
                expected: (Some("tp2"), None),
            },
            Case {
                name: "multi-statement mixed: stmt 1 gets its trailing, stmt 2 leading",
                sql: "SELECT 1 /* traceparent='tp1' */; /* traceparent='tp2' */ SELECT 2;",
                begin: 0,  //
                length: 8, //^
                expected: (Some("tp1"), None),
            },
            Case {
                name: "multi-statement mixed: stmt 2 gets its leading, stmt 1 trailing",
                sql: "SELECT 1 /* traceparent='tp1' */; /* traceparent='tp2' */ SELECT 2;",
                begin: 58, //---------------------------------------------------^
                length: 8, //-----------------------------------------------------------^
                expected: (Some("tp2"), None),
            },
            //
            // Possible false-positives
            //
            Case {
                name: "internal comment",
                sql: "SELECT 1 /* traceparent='tp1' */ WHERE 1;",
                begin: 0,   //
                length: 39, //--------------------------------^
                expected: (None, None),
            },
            Case {
                name: "internal comment in prior statement",
                sql: "SELECT 1 /* traceparent='tp1' */ WHERE 1; SELECT 2;",
                begin: 42, //-----------------------------------^
                length: 8, //-------------------------------------------^
                expected: (None, None),
            },
            Case {
                name: "unclosed comment ignored",
                sql: "SELECT 1 -- traceparent='tp1' */",
                begin: 0,  //
                length: 8, //^
                expected: (None, None),
            },
            Case {
                name: "multiple empty statements prior ignored",
                sql: "SELECT 1 /* traceparent='tp1' */; ; ; SELECT 2;",
                begin: 39, //-------------------------------^
                length: 8, //---------------------------------------^
                expected: (None, None),
            },
            //
            // Possible false-negatives
            //
            Case {
                name: "unpopulated offsets: trailing comment found",
                sql: "CREATE TEMP TABLE t1 (id int) /* traceparent='tp1' */;",
                begin: 0,
                length: 0,
                expected: (Some("tp1"), None),
            },
            Case {
                name: "unpopulated offsets: trailing comment before trailing semicolons",
                sql: "SELECT 1 /* traceparent='tp1' */; ; ;",
                begin: 0,
                length: 0,
                expected: (Some("tp1"), None),
            },
            Case {
                name: "unpopulated offsets: leading comment found",
                sql: "/* traceparent='tp1' */ CREATE TEMP TABLE t1 (id int);",
                begin: 0,
                length: 0,
                expected: (Some("tp1"), None),
            },
            Case {
                name: "multiline trailing comment",
                sql: "SELECT 1 /*\n traceparent='tp1',\ntracestate='ts1'\n*/;",
                begin: 0,  //
                length: 8, //^
                expected: (Some("tp1"), Some("ts1")),
            },
            Case {
                name: "multiline leading comment",
                sql: "/*\n traceparent='tp1',\ntracestate='ts1'\n*/ SELECT 1;",
                begin: 43, //---------------------------------------^
                length: 8, //-----------------------------------------------^
                expected: (Some("tp1"), Some("ts1")),
            },
        ];

        for case in cases {
            let headers = Headers::from_statement(case.sql, case.begin, case.length);
            let headers = headers.as_ref();
            let actual_tp = headers.map(|h| h.0.as_ref());
            let actual_ts = headers.and_then(|h| h.1.as_ref().map(|s| s.as_ref()));

            scoped_trace!("{}", case.name);
            assert_that!((actual_tp, actual_ts), eq(case.expected),);
        }
    }

    #[test]
    fn test_context_try_from_headers_valid() {
        struct Case<'a> {
            name: &'a str,
            traceparent: &'a str,
            tracestate: Option<&'a str>,
            expected_trace: TraceId,
            expected_span: SpanId,
            expected_flags: TraceFlags,
        }

        let cases = [
            Case {
                name: "sampled",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                tracestate: None,
                expected_trace: TraceId::from_bytes([
                    0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e,
                    0x0e, 0x47, 0x36,
                ]),
                expected_span: SpanId::from_bytes([0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7]),
                expected_flags: TraceFlags::SAMPLED,
            },
            Case {
                name: "unsampled",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
                tracestate: None,
                expected_trace: TraceId::from_bytes([
                    0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e,
                    0x0e, 0x47, 0x36,
                ]),
                expected_span: SpanId::from_bytes([0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7]),
                expected_flags: TraceFlags::default(),
            },
            Case {
                name: "unsampled with state",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
                tracestate: Some("congo=t61rcWkgMzE"),
                expected_trace: TraceId::from_bytes([
                    0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e,
                    0x0e, 0x47, 0x36,
                ]),
                expected_span: SpanId::from_bytes([0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7]),
                expected_flags: TraceFlags::default(),
            },
        ];

        for case in cases {
            let headers = Headers::new(case.traceparent, case.tracestate);

            scoped_trace!("{}", case.name);
            assert_that!(
                SpanContext::try_from(headers),
                ok(eq(&SpanContext {
                    id: case.expected_span,
                    trace: std::borrow::Cow::Owned(TraceContext {
                        id: case.expected_trace,
                        flags: case.expected_flags,
                        state: case.tracestate.map(Str::Borrowed),
                    }),
                })),
            );
        }
    }

    #[test]
    fn test_context_try_from_headers_invalid() {
        for (name, traceparent) in [
            (
                "bad version",
                "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
            (
                "bad trace id",
                "00-4bf92f3577b34da6a3ce929d0e0e473g-00f067aa0ba902b7-01",
            ),
            (
                "bad span id",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902bg-01",
            ),
            (
                "bad flags",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0g",
            ),
            ("too short", "00-4bf92f3577b34da6a3ce-00f067aa-01"),
            ("missing fields", "00-4bf92f3577b34da6a3ce929d0e0e4736"),
        ] {
            scoped_trace!("{name}");
            assert_that!(
                SpanContext::try_from(Headers::new(traceparent, None)),
                err(eq(&())),
            );
        }
    }
}
