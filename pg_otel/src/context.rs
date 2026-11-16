// SPDX-License-Identifier: MIT

mod sqlcommenter;

use self::sqlcommenter::CommenterIterator;
use crate::Str;
use core::cell::Cell;
use pgrx::{pg_sys, void_mut_ptr};
use std::rc::Rc;

thread_local! {
    static PRNG: Cell<PseudoRandom> = Cell::default();
}

/// Random number generator that uses Postgres [`pg_sys::pg_prng_uint64`].
#[derive(Clone, Copy, Default)]
struct PseudoRandom {
    state: pg_sys::pg_prng_state,
    pid: pg_sys::pid_t,
}

impl PseudoRandom {
    // Seed the pseudorandom state after fork(2) or when uninitialized.
    fn ensure_seeded(&mut self) {
        let pid = unsafe { pg_sys::MyProcPid };

        if self.pid != pid || self.pid == 0 {
            let mut seed = [0u8; 8];
            unsafe {
                pg_sys::pg_strong_random(seed.as_mut_ptr() as void_mut_ptr, 8);
                pg_sys::pg_prng_seed(&mut self.state, u64::from_ne_bytes(seed));
            }
            self.pid = pid;
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.ensure_seeded();

        unsafe { pg_sys::pg_prng_uint64(&mut self.state) }
    }

    fn next_u128(&mut self) -> u128 {
        self.ensure_seeded();

        let hi = unsafe { pg_sys::pg_prng_uint64(&mut self.state) } as u128;
        let lo = unsafe { pg_sys::pg_prng_uint64(&mut self.state) } as u128;

        (hi << 64) | lo
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SpanId(u64);

impl SpanId {
    // Generate a new (uniformly pseudorandom) span identifier.
    pub fn random() -> Self {
        PRNG.with(|cell| {
            let mut prng = cell.get();

            // Repeat until the value is nonzero; W3C Trace Context § 3.2.2.4
            loop {
                let id = prng.next_u64();
                if id != 0 {
                    cell.set(prng);
                    return Self(id);
                }
            }
        })
    }

    pub fn from_hex(hex: &str) -> Result<Self, ()> {
        match hex.len() {
            16 => u64::from_str_radix(hex, 16).map(Self).map_err(|_| ()),
            _ => Err(()),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }
}

impl From<SpanId> for Vec<u8> {
    fn from(id: SpanId) -> Self {
        id.0.to_be_bytes().to_vec()
    }
}

impl core::fmt::LowerHex for SpanId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl core::fmt::Debug for SpanId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SpanId({:016x})", self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TraceId(u128);

impl TraceId {
    // Generate a new (uniformly pseudorandom) trace identifier.
    pub fn random() -> Self {
        PRNG.with(|cell| {
            let mut prng = cell.get();

            // Repeat until the value is nonzero; W3C Trace Context § 3.2.2.3
            loop {
                let id = prng.next_u128();
                if id != 0 {
                    cell.set(prng);
                    return Self(id);
                }
            }
        })
    }

    pub fn from_hex(hex: &str) -> Result<Self, ()> {
        match hex.len() {
            32 => u128::from_str_radix(hex, 16).map(Self).map_err(|_| ()),
            _ => Err(()),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }
}

impl core::convert::From<TraceId> for Vec<u8> {
    fn from(id: TraceId) -> Self {
        id.0.to_be_bytes().to_vec()
    }
}

impl core::fmt::LowerHex for TraceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl core::fmt::Debug for TraceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TraceId({:032x})", self.0)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceFlags(u8);

impl TraceFlags {
    pub fn from_hex(hex: &str) -> Result<Self, ()> {
        match hex.len() {
            2 => u8::from_str_radix(hex, 16).map(Self).map_err(|_| ()),
            _ => Err(()),
        }
    }

    pub const W3C_RANDOM: Self = Self(0x02); // Indicates the right-most bytes are uniformly random; W3C Trace Context § 3.2.2.5
    pub const W3C_SAMPLED: Self = Self(0x01); // Indicates the caller may have recorded trace data; W3C Trace Context § 3.2.2.5

    pub fn is_random(self) -> bool {
        self.0 & 0x02 != 0
    }

    pub fn is_sampled(self) -> bool {
        self.0 & 0x01 != 0
    }
}

impl core::ops::BitOr for TraceFlags {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::convert::From<TraceFlags> for u32 {
    #[inline]
    fn from(f: TraceFlags) -> Self {
        f.0 as u32
    }
}

impl core::fmt::LowerHex for TraceFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}", self.0)
    }
}

impl core::fmt::Debug for TraceFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TraceFlags({:02x})", self.0)
    }
}

/// W3C Trace Context headers extracted from a statement or session.
#[derive(Debug, PartialEq)]
pub struct Headers<'a> {
    /// Required [W3C `traceparent` header](https://www.w3.org/TR/trace-context#traceparent-header)
    pub traceparent: Str<'a>,
    /// Optional [W3C `tracestate` header](https://www.w3.org/TR/trace-context#tracestate-header)
    pub tracestate: Option<Str<'a>>,
}

/// Context identifying a span within an OpenTelemetry trace.
///
/// # Performance
/// Cloning a `SpanContext` is $O(1)$ with zero allocations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanContext {
    pub span_id: SpanId,
    pub trace_id: TraceId,
    pub trace_flags: TraceFlags,
    pub trace_state: Rc<str>,
}

impl SpanContext {
    /// Derives a child span context from a parent, inheriting the trace ID, trace flags,
    /// and trace state, while generating a new random span ID.
    pub fn child_of(parent: &Self) -> Self {
        Self {
            span_id: SpanId::random(),
            trace_id: parent.trace_id,
            trace_flags: parent.trace_flags,
            trace_state: parent.trace_state.clone(),
        }
    }

    /// Generates a new root trace context with a random trace ID, random span ID,
    /// and standard random sampled trace flags (`0x03`).
    pub fn root() -> Self {
        Self {
            span_id: SpanId::random(),
            trace_id: TraceId::random(),
            trace_flags: TraceFlags::W3C_RANDOM | TraceFlags::W3C_SAMPLED,
            trace_state: Rc::default(),
        }
    }

    // Representation defined by W3C Trace Context, Traceparent § 3.2
    pub fn w3c_traceparent(&self) -> String {
        format!(
            "00-{:032x}-{:016x}-{:02x}",
            self.trace_id, self.span_id, self.trace_flags
        )
    }
}

/// Parses a [`SpanContext`] from W3C [`Headers`].
///
/// # W3C Trace Context Specification
///
/// Implements parsing of the `traceparent` and `tracestate` headers according to [W3C Trace Context](https://www.w3.org/TR/trace-context):
///
/// - **`traceparent` format**: Expects version `00` followed by 32-hex of `trace-id`, 16-hex of `parent-id`, and 2-hex of `trace-flags` separated by hyphens.
/// - **`tracestate` dependency**: If `traceparent` fails to parse, this method returns `Err(())`; W3C Trace Context § 3.3
impl<'a> core::convert::TryFrom<Headers<'a>> for SpanContext {
    type Error = ();

    fn try_from(value: Headers<'a>) -> Result<Self, Self::Error> {
        let (version, rest) = value.traceparent.split_once('-').ok_or(())?;

        match version {
            "00" => {
                let (trace, rest) = rest.split_once('-').ok_or(())?;
                let (span, flags) = rest.split_once('-').ok_or(())?;

                Ok(SpanContext {
                    span_id: SpanId::from_hex(span)?,
                    trace_id: TraceId::from_hex(trace)?,
                    trace_flags: TraceFlags::from_hex(flags)?,
                    trace_state: value
                        .tracestate
                        .as_deref()
                        .map(Rc::from)
                        .unwrap_or_default(),
                })
            }
            _ => Err(()),
        }
    }
}

impl<'a> Headers<'a> {
    pub fn new(traceparent: &'a str, tracestate: Option<&'a str>) -> Self {
        Headers {
            traceparent: Str::Borrowed(traceparent),
            tracestate: tracestate.map(Str::Borrowed),
        }
    }

    /// Extracts W3C `traceparent` and `tracestate` headers from comma-separated key-value pairs
    /// emitted by [SQLCommenter](https://google.github.io/sqlcommenter).
    ///
    /// Per [W3C Trace Context](https://www.w3.org/TR/trace-context), `traceparent` is required;
    /// if it is missing, any accompanying `tracestate` is ignored and this returns `None`.
    fn from_comment(comment: &'a str) -> Option<Headers<'a>> {
        let mut traceparent = None;
        let mut tracestate = None;

        for (k, v) in CommenterIterator::new(comment) {
            if k == "traceparent" {
                traceparent = Some(v);
            } else if k == "tracestate" {
                tracestate = Some(v);
            }
        }

        Some(Headers {
            traceparent: traceparent?,
            tracestate,
        })
    }

    /// Extracts headers from a comment at the head (front/left) of a slice.
    #[allow(dead_code)]
    pub fn from_head(sql: &'a str) -> Option<Headers<'a>> {
        let comment = sql.trim_start().strip_prefix("/*")?;
        let (comment, _) = comment.split_once("*/")?;
        Self::from_comment(comment)
    }

    /// Extracts headers from a comment at the tail (back/right) of a slice.
    ///
    /// Compatible with [SQLCommenter](https://google.github.io/sqlcommenter).
    pub fn from_tail(sql: &'a str) -> Option<Headers<'a>> {
        let comment = sql.trim_end().strip_suffix("*/")?;
        let (_, comment) = comment.rsplit_once("/*")?;
        Self::from_comment(comment)
    }
}

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    #[cfg(test)]
    use super::{Headers, TraceFlags};
    use super::{SpanContext, SpanId, TraceId};
    use googletest::prelude::*;

    #[test]
    fn test_headers_from_head() {
        // Comment at head of statement body (Postgres 13–17)
        assert_that!(
            Headers::from_head(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',tracestate='congo=t61rcWkgMzE' */ SELECT * FROM users"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Comment at head without tracestate
        assert_that!(
            Headers::from_head(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ SELECT * FROM users"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Comment at head with leading whitespace
        assert_that!(
            Headers::from_head(
                "  \t\n /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ SELECT 1"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Multiline comment at head
        assert_that!(
            Headers::from_head(
                "/*\n traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',\ntracestate='congo=t61rcWkgMzE'\n*/ SELECT 1"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Ignore comment at tail (not at head)
        assert_that!(
            Headers::from_head(
                "SELECT 1 /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */"
            ),
            none(),
        );

        // Ignore comment embedded in the middle
        assert_that!(
            Headers::from_head(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ WHERE id = 1"
            ),
            none(),
        );

        // Empty comment or no-traceparent comment returns None
        assert_that!(Headers::from_head("/**/ SELECT 1"), none());
        assert_that!(
            Headers::from_head("/* route='/users',action='index' */ SELECT 1"),
            none(),
        );
        assert_that!(
            Headers::from_head("/* tracestate='congo=t61rcWkgMzE' */ SELECT 1"),
            none(),
        );

        // Multiple comments: comment at head takes precedence when inspecting head
        assert_that!(
            Headers::from_head(
                "/* traceparent='00-11111111111111111111111111111111-1111111111111111-01' */ SELECT 1 /* traceparent='00-22222222222222222222222222222222-2222222222222222-01' */"
            ),
            some(eq(&Headers::new(
                "00-11111111111111111111111111111111-1111111111111111-01",
                None,
            ))),
        );
    }

    #[test]
    fn test_headers_from_tail() {
        // Comment at tail of statement body (all Postgres versions)
        assert_that!(
            Headers::from_tail(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',tracestate='congo=t61rcWkgMzE' */"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Comment at tail without tracestate
        assert_that!(
            Headers::from_tail(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Comment at tail followed by whitespace
        assert_that!(
            Headers::from_tail(
                "SELECT 1 /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */  \t\n "
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Multiline comment at tail
        assert_that!(
            Headers::from_tail(
                "SELECT 1 /*\n traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',\ntracestate='congo=t61rcWkgMzE'\n*/"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Postgres 18+ prefix slice: comment at tail of prefix (immediately preceding statement keyword)
        assert_that!(
            Headers::from_tail(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ "
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Postgres 18+ prefix slice: empty statement barrier (ends with ';', not '*/') ignored
        assert_that!(Headers::from_tail(" /* isolated */ ; "), none(),);

        // Statement slice with terminating semicolon ignored (slices stop before ';')
        assert_that!(
            Headers::from_tail(
                "SELECT 1 /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */;"
            ),
            none(),
        );

        // Ignore comment at head (not at tail)
        assert_that!(
            Headers::from_tail(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ SELECT 1"
            ),
            none(),
        );

        // Ignore comment embedded in the middle of slice
        assert_that!(
            Headers::from_tail(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ WHERE id = 1"
            ),
            none(),
        );

        // Empty comment or no-traceparent comment returns None
        assert_that!(Headers::from_tail("SELECT 1 /**/"), none());
        assert_that!(
            Headers::from_tail("SELECT 1 /* route='/users',action='index' */"),
            none(),
        );
        assert_that!(
            Headers::from_tail("SELECT * FROM users /* tracestate='congo=t61rcWkgMzE' */"),
            none(),
        );

        // Multiple comments: comment at tail takes precedence
        assert_that!(
            Headers::from_tail(
                "/* traceparent='00-11111111111111111111111111111111-1111111111111111-01' */ SELECT 1 /* traceparent='00-22222222222222222222222222222222-2222222222222222-01' */"
            ),
            some(eq(&Headers::new(
                "00-22222222222222222222222222222222-2222222222222222-01",
                None,
            ))),
        );
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
                expected_trace: TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736),
                expected_span: SpanId(0x00f0_67aa_0ba9_02b7),
                expected_flags: TraceFlags::W3C_SAMPLED,
            },
            Case {
                name: "unsampled",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
                tracestate: None,
                expected_trace: TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736),
                expected_span: SpanId(0x00f0_67aa_0ba9_02b7),
                expected_flags: TraceFlags::default(),
            },
            Case {
                name: "unsampled with state",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
                tracestate: Some("congo=t61rcWkgMzE"),
                expected_trace: TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736),
                expected_span: SpanId(0x00f0_67aa_0ba9_02b7),
                expected_flags: TraceFlags::default(),
            },
        ];

        for case in cases {
            let headers = Headers::new(case.traceparent, case.tracestate);

            scoped_trace!("{}", case.name);
            assert_that!(
                SpanContext::try_from(headers),
                ok(eq(&SpanContext {
                    span_id: case.expected_span,
                    trace_id: case.expected_trace,
                    trace_flags: case.expected_flags,
                    trace_state: case.tracestate.map(std::rc::Rc::from).unwrap_or_default(),
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
                err(anything()),
            );
        }
    }

    #[test]
    fn test_context_w3c_traceparent_roundtrip() {
        let trace_id = TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736);
        let span_id = SpanId(0x00f0_67aa_0ba9_02b7);

        let sampled = SpanContext {
            trace_id,
            span_id,
            trace_flags: TraceFlags::W3C_SAMPLED,
            trace_state: std::rc::Rc::default(),
        };
        assert_that!(
            sampled.w3c_traceparent(),
            eq("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        assert_that!(
            SpanContext::try_from(Headers::new(&sampled.w3c_traceparent(), None)),
            ok(eq(&sampled))
        );

        let unsampled = SpanContext {
            trace_id,
            span_id,
            trace_flags: TraceFlags::default(),
            trace_state: std::rc::Rc::default(),
        };
        assert_that!(
            unsampled.w3c_traceparent(),
            eq("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00")
        );
        assert_that!(
            SpanContext::try_from(Headers::new(&unsampled.w3c_traceparent(), None)),
            ok(eq(&unsampled))
        );
    }

    #[pgrx::pg_test]
    fn test_context_root_and_child() {
        let root = SpanContext::root();
        assert!(root.span_id.is_valid());
        assert!(root.trace_id.is_valid());
        assert!(root.trace_flags.is_random());
        assert!(root.trace_flags.is_sampled());
        assert!(root.trace_state.is_empty());

        let child = SpanContext::child_of(&root);
        assert_that!(child.trace_id, eq(root.trace_id));
        assert_that!(child.trace_flags, eq(root.trace_flags));
        assert_that!(&child.trace_state, eq(&root.trace_state));
        assert_that!(child.span_id, not(eq(root.span_id)));
        assert!(child.span_id.is_valid());
    }

    #[pgrx::pg_test]
    fn test_span_id_construction() {
        let span1 = SpanId::random();
        assert_that!(span1, eq(span1));
        assert!(span1.is_valid());

        let span2 = SpanId::random();
        assert_that!(span2, eq(span2));
        assert!(span2.is_valid());

        assert_that!(span1, not(eq(span2)), "generated");
        assert_that!(span2, not(eq(span1)), "generated");
    }

    #[test]
    fn test_span_id_conversions() {
        let span = SpanId(0x0123_4567_89ab_cdef);
        let v: Vec<u8> = span.into();
        assert_that!(v, eq(&vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]));
    }

    #[test]
    fn test_span_id_formatting() {
        let span = SpanId(0x00f0_67aa_0ba9_02b7);
        assert_that!(format!("{:016x}", span), eq("00f067aa0ba902b7"));
        assert_that!(format!("{:x}", span), eq("00f067aa0ba902b7"));
        assert_that!(format!("{:?}", span), eq("SpanId(00f067aa0ba902b7)"));
    }

    #[pgrx::pg_test]
    fn test_trace_id_construction() {
        let trace1 = TraceId::random();
        assert_that!(trace1, eq(trace1));
        assert!(trace1.is_valid());

        let trace2 = TraceId::random();
        assert_that!(trace2, eq(trace2));
        assert!(trace2.is_valid());

        assert_that!(trace1, not(eq(trace2)), "generated");
        assert_that!(trace2, not(eq(trace1)), "generated");
    }

    #[test]
    fn test_trace_id_conversions() {
        let trace = TraceId(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let v: Vec<u8> = trace.into();
        assert_that!(
            v,
            eq(&vec![
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef,
            ])
        );
    }

    #[test]
    fn test_trace_id_formatting() {
        let trace = TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736);
        assert_that!(
            format!("{:032x}", trace),
            eq("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_that!(
            format!("{:x}", trace),
            eq("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_that!(
            format!("{:?}", trace),
            eq("TraceId(4bf92f3577b34da6a3ce929d0e0e4736)")
        );
    }

    #[test]
    fn test_trace_flags_conversions() {
        let flags = TraceFlags::W3C_SAMPLED;
        let flags_u32: u32 = flags.into();
        assert_that!(flags_u32, eq(1u32));
    }

    #[test]
    fn test_trace_flags_formatting() {
        let flags = TraceFlags::W3C_SAMPLED;
        assert_that!(format!("{:02x}", flags), eq("01"));
        assert_that!(format!("{:x}", flags), eq("01"));
        assert_that!(format!("{:?}", flags), eq("TraceFlags(01)"));
    }

    #[test]
    fn test_trace_flags_bitor() {
        let flags = TraceFlags::W3C_RANDOM | TraceFlags::W3C_SAMPLED;
        assert_that!(flags.is_random(), eq(true));
        assert_that!(flags.is_sampled(), eq(true));
        assert_that!(format!("{:x}", flags), eq("03"));
    }
}
