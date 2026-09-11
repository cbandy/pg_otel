// SPDX-License-Identifier: MIT

use std::ffi;
use url::Url as URL;

pub struct Endpoint;

impl Endpoint {
    pub fn from(trusted: &crate::GucString) -> Option<URL> {
        trusted.get().map(|v| v.to_string_lossy().parse().unwrap())
    }

    pub fn validate(untrusted: &ffi::CStr) -> eyre::Result<()> {
        let s = untrusted.to_str()?;
        eyre::ensure!(
            s.starts_with("http://") || s.starts_with("https://"),
            r#"URL must begin with "http" or "https""#,
        );
        URL::parse(s)?;
        Ok(())
    }
}

pub struct Headers;

impl Headers {
    pub fn from(trusted: &crate::GucString) -> Vec<(String, String)> {
        trusted
            .get()
            .map(|v| Self::parse(&v.to_string_lossy()).unwrap())
            .unwrap_or_default()
    }

    pub fn validate(untrusted: &ffi::CStr) -> eyre::Result<()> {
        Self::parse(untrusted.to_str()?)?;
        Ok(())
    }

    pub fn parse(raw: &str) -> eyre::Result<Vec<(String, String)>> {
        let mut results = Vec::new();
        if raw.trim().is_empty() {
            return Ok(results);
        }

        // Step 1: Split into raw tokens on unescaped commas
        let mut raw_tokens = Vec::new();
        let mut current = String::new();
        let mut chars = raw.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(&',') = chars.peek() {
                    chars.next();
                    current.push(',');
                    continue;
                }
                current.push('\\');
            } else if c == ',' {
                raw_tokens.push(current);
                current = String::new();
            } else {
                current.push(c);
            }
        }
        if !current.is_empty() {
            raw_tokens.push(current);
        }

        // Step 2: Parse each token into a key-value pair
        for token in raw_tokens {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue; // Skip empty tokens (handling leading/trailing/multiple commas & newlines)
            }

            // Find first unescaped ':' or '='
            let mut delim_pos = None;
            let mut t_chars = trimmed.char_indices().peekable();
            while let Some((idx, ch)) = t_chars.next() {
                if ch == '\\' {
                    t_chars.next(); // Skip escaped character
                } else if ch == ':' || ch == '=' {
                    delim_pos = Some((idx, ch));
                    break;
                }
            }

            let Some((pos, _ch)) = delim_pos else {
                eyre::bail!("invalid header syntax: missing ':' or '=' in '{trimmed}'");
            };

            let key = trimmed[..pos].trim();
            let val = trimmed[pos + 1..].trim();

            if key.is_empty() {
                eyre::bail!("invalid header syntax: key cannot be empty in '{trimmed}'");
            }

            results.push((key.to_string(), val.to_string()));
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[test]
    fn headers_parse_basic() {
        let input = "X-API-Key: secret123, env=production";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("X-API-Key"), eq("secret123")),
                (eq("env"), eq("production")),
            ])
        );
    }

    #[test]
    fn headers_parse_first_delimiter_split() {
        let input = "X-Callback: https://example.com/api?a=1, Auth=token==";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("X-Callback"), eq("https://example.com/api?a=1")),
                (eq("Auth"), eq("token==")),
            ])
        );
    }

    #[test]
    fn headers_parse_multiline_and_extra_commas() {
        let input = "
            , X-Scope-OrgID: tenant_101 ,

            Authorization: Bearer my_secret_token ,
            ,
        ";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("X-Scope-OrgID"), eq("tenant_101")),
                (eq("Authorization"), eq("Bearer my_secret_token")),
            ])
        );
    }

    #[test]
    fn headers_parse_escaped_commas() {
        let input = r"Authorization: Bearer foo\,bar, env: prod";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("Authorization"), eq("Bearer foo,bar")),
                (eq("env"), eq("prod")),
            ])
        );
    }

    #[test]
    fn headers_parse_invalid_syntax_error() {
        let input = "invalid_header_without_delimiter";
        assert_that!(
            Headers::parse(input),
            err(displays_as(contains_substring(
                "invalid header syntax: missing ':' or '='"
            )))
        );

        let input_empty_key = ": value_without_key";
        assert_that!(
            Headers::parse(input_empty_key),
            err(displays_as(contains_substring(
                "invalid header syntax: key cannot be empty"
            )))
        );
    }
}
