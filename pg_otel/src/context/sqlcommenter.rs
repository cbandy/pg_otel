// SPDX-License-Identifier: MIT

use crate::Str;

/// An iterator over comma-separated key-value pairs in an [SQLCommenter](https://google.github.io/sqlcommenter/spec) comment body.
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
///
/// # Performance
/// This avoids copying unencoded pairs and allocates only when unescaping or percent-decoding.
pub struct CommenterIterator<'a>(&'a str);

impl<'a> CommenterIterator<'a> {
    pub fn new(input: &'a str) -> Self {
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
        crate::encoding::percent_decode_lossy(Self::reverse_backslashes(encoded))
    }

    fn reverse_backslashes(encoded: &'a str) -> Str<'a> {
        let Some((head, mut tail)) = encoded.split_once('\\') else {
            return Str::Borrowed(encoded);
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
    use super::*;
    use googletest::prelude::*;

    #[test]
    fn test_commenter_iterator_decode() {
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

        // valid percent encoding and multi-byte UTF-8
        assert_that!(CommenterIterator::decode("foo%20bar"), eq("foo bar"));
        assert_that!(CommenterIterator::decode("foo%C3%BCbar"), eq("fooübar"));
        assert_that!(CommenterIterator::decode("emoji%E2%9C%A8"), eq("emoji✨"));
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
}
