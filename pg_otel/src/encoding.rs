// SPDX-License-Identifier: MIT

//! URL percent-decoding utilities per RFC 3986.

type Str<'a> = std::borrow::Cow<'a, str>;

#[inline(always)]
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes a string, converting `%XX` sequences into UTF-8 characters.
///
/// Returns an error if any `%` sequence is malformed (e.g. invalid hex or truncated)
/// or if the decoded bytes do not form valid UTF-8.
///
/// If `s` contains no `%` character, it is returned as the input [`Str`]
/// without any heap allocations.
pub fn percent_decode<'a>(s: impl Into<Str<'a>>) -> eyre::Result<Str<'a>> {
    percent_decode_internal(s.into(), true)
}

/// Permissively percent-decodes a string, converting `%XX` sequences into UTF-8 characters.
///
/// Incomplete or invalid `%` sequences retain the literal `%` and continue parsing.
/// Invalid UTF-8 byte sequences are replaced using [`String::from_utf8_lossy`].
///
/// If `s` contains no `%` character, it is returned as the input [`Str`]
/// without any heap allocations.
pub fn percent_decode_lossy<'a>(s: impl Into<Str<'a>>) -> Str<'a> {
    percent_decode_internal(s.into(), false).expect("lossy decoding never fails")
}

fn percent_decode_internal<'a>(s: Str<'a>, strict: bool) -> eyre::Result<Str<'a>> {
    let Some((head, mut tail)) = s.split_once('%') else {
        return Ok(s);
    };

    let mut bytes = Vec::with_capacity(s.len());
    bytes.extend_from_slice(head.as_bytes());

    loop {
        if tail.len() >= 2
            && let (Some(hi), Some(lo)) = (hex(tail.as_bytes()[0]), hex(tail.as_bytes()[1]))
        {
            bytes.push((hi << 4) | lo);
            tail = &tail[2..];
        } else if strict {
            if tail.len() < 2 {
                eyre::bail!("invalid percent-encoding in '{s}': truncated sequence");
            } else {
                let hex_str = std::str::from_utf8(&tail.as_bytes()[..2]).unwrap_or("??");
                eyre::bail!("invalid percent-encoding in '{s}': '%{hex_str}' is not valid hex");
            }
        } else {
            bytes.push(b'%');
        }

        if let Some((h, next)) = tail.split_once('%') {
            bytes.extend_from_slice(h.as_bytes());
            tail = next;
        } else {
            bytes.extend_from_slice(tail.as_bytes());
            break;
        }
    }

    if strict {
        let decoded = String::from_utf8(bytes)
            .map_err(|e| eyre::eyre!("invalid UTF-8 after percent-decoding in '{s}': {e}"))?;
        Ok(Str::Owned(decoded))
    } else {
        Ok(match String::from_utf8(bytes) {
            Ok(decoded) => Str::Owned(decoded),
            Err(e) => Str::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[test]
    fn unencoded_borrowed() {
        match percent_decode("plain_text").unwrap() {
            Str::Borrowed(s) => assert_that!(s, eq("plain_text")),
            Str::Owned(_) => panic!("expected borrowed Str for plain text"),
        }
        match percent_decode_lossy("plain_text") {
            Str::Borrowed(s) => assert_that!(s, eq("plain_text")),
            Str::Owned(_) => panic!("expected borrowed Str for plain text"),
        }
    }

    #[test]
    fn ascii_decoding() {
        assert_that!(
            percent_decode("hello%20world%21").unwrap().as_ref(),
            eq("hello world!")
        );
        assert_that!(
            percent_decode("foo%2Cbar%3Dbaz").unwrap().as_ref(),
            eq("foo,bar=baz")
        );
    }

    #[test]
    fn utf8_multibyte_decoding() {
        // 'ü' is 0xC3, 0xBC
        assert_that!(
            percent_decode("foo%C3%BCbar").unwrap().as_ref(),
            eq("fooübar")
        );
        // '✨' (sparkles) is 0xE2, 0x9C, 0xA8
        assert_that!(
            percent_decode("%E2%9C%A8sparkle").unwrap().as_ref(),
            eq("✨sparkle")
        );
        assert_that!(percent_decode_lossy("foo%C3%BCbar").as_ref(), eq("fooübar"));
    }

    #[test]
    fn strict_error_handling() {
        assert_that!(
            percent_decode("foo%ZZbar"),
            err(displays_as(contains_substring("is not valid hex")))
        );
        assert_that!(
            percent_decode("foo%2"),
            err(displays_as(contains_substring("truncated sequence")))
        );
        assert_that!(
            percent_decode("foo%"),
            err(displays_as(contains_substring("truncated sequence")))
        );
    }

    #[test]
    fn permissive_error_handling() {
        assert_that!(percent_decode_lossy("foo%ZZbar").as_ref(), eq("foo%ZZbar"));
        assert_that!(percent_decode_lossy("foo%2").as_ref(), eq("foo%2"));
        assert_that!(percent_decode_lossy("foo%").as_ref(), eq("foo%"));
        assert_that!(
            percent_decode_lossy("foo%ZZ%20bar").as_ref(),
            eq("foo%ZZ bar")
        );
    }
}
