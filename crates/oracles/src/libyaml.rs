//! libyaml adapter, via `unsafe-libyaml` 0.2.11 (the c2rust translation of
//! libyaml 0.2.5 that `serde_yaml` and `serde_yaml_ng` parse with).
//!
//! libyaml's events carry everything the suite notation records (document
//! markers, collection styles, anchor names, resolved tags), so it is scored
//! with [`Profile::FULL`]. Encoding is left to libyaml's own detection (as in
//! libyaml's `run-parser-test-suite`), so BOM handling is libyaml's.
//!
//! `unsafe-libyaml` exposes only `unsafe fn`s over raw pointers, so this
//! module is the one place in the workspace that writes `unsafe` blocks
//! (crate-level `deny`, allowed here only). `unsafe`-containing
//! YAML implementations to this unpublished crate; every block carries a
//! `SAFETY` note.
#![allow(unsafe_code)]

use core::ffi::{CStr, c_char};
use core::mem::MaybeUninit;

use test_suite::standard::{Observation, Oracle, Profile};
use unsafe_libyaml as unsafe_sys;

use crate::{MAX_EVENTS, escape};

/// libyaml, through `unsafe-libyaml`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LibYaml;

/// Deletes the parser on every exit path.
struct ParserGuard(*mut unsafe_sys::yaml_parser_t);

impl Drop for ParserGuard {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a successful `yaml_parser_initialize`
        // on a live boxed parser, and is deleted exactly once, here.
        unsafe { unsafe_sys::yaml_parser_delete(self.0) }
    }
}

/// A NUL-terminated libyaml string, or `None` for a null pointer.
///
/// # Safety
///
/// `ptr` is null or points to a NUL-terminated string that outlives the call.
unsafe fn c_text(ptr: *const u8) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null and NUL-terminated by the caller's contract.
    let s = unsafe { CStr::from_ptr(ptr.cast::<c_char>()) };
    Some(s.to_string_lossy().into_owned())
}

/// ` &anchor <tag>` properties.
///
/// # Safety
///
/// As for [`c_text`], for both pointers.
unsafe fn props(anchor: *const u8, tag: *const u8) -> String {
    let mut out = String::new();
    // SAFETY: forwarded caller contract.
    if let Some(a) = unsafe { c_text(anchor) } {
        out.push_str(" &");
        out.push_str(&a);
    }
    // SAFETY: forwarded caller contract.
    if let Some(t) = unsafe { c_text(tag) } {
        out.push_str(" <");
        out.push_str(&t);
        out.push('>');
    }
    out
}

/// Renders one event as a suite-notation line.
///
/// # Safety
///
/// `ev` was filled by a successful `yaml_parser_parse` and not yet deleted,
/// so the union member selected by `type_` is the initialised one, and its
/// strings are owned by the event.
unsafe fn render(ev: &unsafe_sys::yaml_event_t) -> String {
    match ev.type_ {
        unsafe_sys::YAML_STREAM_START_EVENT => "+STR".to_owned(),
        unsafe_sys::YAML_STREAM_END_EVENT => "-STR".to_owned(),
        unsafe_sys::YAML_DOCUMENT_START_EVENT => {
            // SAFETY: `type_` is DOCUMENT_START, so `document_start` is the
            // initialised union member (caller contract).
            let implicit = unsafe { ev.data.document_start.implicit };
            if implicit { "+DOC" } else { "+DOC ---" }.to_owned()
        }
        unsafe_sys::YAML_DOCUMENT_END_EVENT => {
            // SAFETY: `type_` is DOCUMENT_END, so `document_end` is the
            // initialised union member (caller contract).
            let implicit = unsafe { ev.data.document_end.implicit };
            if implicit { "-DOC" } else { "-DOC ..." }.to_owned()
        }
        unsafe_sys::YAML_ALIAS_EVENT => {
            // SAFETY: `type_` is ALIAS, so `alias` is the initialised union
            // member (caller contract).
            let anchor = unsafe { ev.data.alias.anchor };
            // SAFETY: libyaml sets an alias's anchor to a NUL-terminated
            // string owned by the live event.
            let name = unsafe { c_text(anchor) };
            format!("=ALI *{}", name.unwrap_or_default())
        }
        unsafe_sys::YAML_SCALAR_EVENT => {
            // SAFETY: `type_` is SCALAR, so `scalar` is the initialised union
            // member (caller contract).
            let s = unsafe { ev.data.scalar };
            let style = match s.style {
                unsafe_sys::YAML_PLAIN_SCALAR_STYLE => ':',
                unsafe_sys::YAML_SINGLE_QUOTED_SCALAR_STYLE => '\'',
                unsafe_sys::YAML_DOUBLE_QUOTED_SCALAR_STYLE => '"',
                unsafe_sys::YAML_LITERAL_SCALAR_STYLE => '|',
                unsafe_sys::YAML_FOLDED_SCALAR_STYLE => '>',
                _ => '?',
            };
            let len = usize::try_from(s.length).unwrap_or(0);
            let bytes = if s.value.is_null() {
                &[][..]
            } else {
                // SAFETY: non-null; libyaml's scalar value is `length`
                // initialised bytes owned by the live event.
                unsafe { core::slice::from_raw_parts(s.value, len) }
            };
            let value = String::from_utf8_lossy(bytes);
            // SAFETY: anchor and tag are null or NUL-terminated strings
            // owned by the live event.
            let props = unsafe { props(s.anchor, s.tag) };
            format!("=VAL{props} {style}{}", escape(&value))
        }
        unsafe_sys::YAML_SEQUENCE_START_EVENT => {
            // SAFETY: `type_` is SEQUENCE_START, so `sequence_start` is the
            // initialised union member (caller contract).
            let s = unsafe { ev.data.sequence_start };
            let flow = if s.style == unsafe_sys::YAML_FLOW_SEQUENCE_STYLE {
                " []"
            } else {
                ""
            };
            // SAFETY: anchor and tag are null or NUL-terminated strings
            // owned by the live event.
            let props = unsafe { props(s.anchor, s.tag) };
            format!("+SEQ{flow}{props}")
        }
        unsafe_sys::YAML_SEQUENCE_END_EVENT => "-SEQ".to_owned(),
        unsafe_sys::YAML_MAPPING_START_EVENT => {
            // SAFETY: `type_` is MAPPING_START, so `mapping_start` is the
            // initialised union member (caller contract).
            let m = unsafe { ev.data.mapping_start };
            let flow = if m.style == unsafe_sys::YAML_FLOW_MAPPING_STYLE {
                " {}"
            } else {
                ""
            };
            // SAFETY: anchor and tag are null or NUL-terminated strings
            // owned by the live event.
            let props = unsafe { props(m.anchor, m.tag) };
            format!("+MAP{flow}{props}")
        }
        unsafe_sys::YAML_MAPPING_END_EVENT => "-MAP".to_owned(),
        _ => "<unknown libyaml event>".to_owned(),
    }
}

impl Oracle for LibYaml {
    fn name(&self) -> &'static str {
        "libyaml (unsafe-libyaml)"
    }

    fn version(&self) -> &'static str {
        "0.2.11"
    }

    fn profile(&self) -> Option<Profile> {
        Some(Profile::FULL)
    }

    fn observe(&mut self, input: &str) -> Observation {
        // The adapter could not run the parser: no verdict (scored as a
        // failure, never as a rejection).
        let fail = |error: &str| Observation {
            events: Some(Vec::new()),
            error: Some(error.to_owned()),
            round_trip: None,
            truncated: true,
        };
        let Ok(len) = input.len().try_into() else {
            return fail("input too long for libyaml");
        };
        // libyaml stores a pointer to the parser inside itself, so it is boxed
        // and never moved; `boxed` outlives `guard` (reverse drop order).
        let mut boxed: Box<MaybeUninit<unsafe_sys::yaml_parser_t>> =
            Box::new(MaybeUninit::uninit());
        let parser = boxed.as_mut_ptr();
        // SAFETY: `parser` is valid, aligned, writable memory for a parser.
        if unsafe { unsafe_sys::yaml_parser_initialize(parser) }.fail {
            return fail("yaml_parser_initialize failed");
        }
        let guard = ParserGuard(parser);
        // SAFETY: initialised parser; `input` outlives the parser (it is
        // deleted by `guard` before this function returns).
        unsafe { unsafe_sys::yaml_parser_set_input_string(guard.0, input.as_ptr(), len) };

        let mut lines = Vec::new();
        let mut error = None;
        // Cleared when the parser reaches STREAM-END or fails (C1).
        let mut truncated = true;
        for _ in 0..MAX_EVENTS {
            let mut event = MaybeUninit::<unsafe_sys::yaml_event_t>::uninit();
            // SAFETY: initialised parser; `event` is writable event storage.
            if unsafe { unsafe_sys::yaml_parser_parse(guard.0, event.as_mut_ptr()) }.fail {
                // SAFETY: the parser is live; `problem` is null or a static
                // NUL-terminated message.
                let problem = unsafe { c_text((&*guard.0).problem.cast::<u8>()) };
                error = Some(problem.unwrap_or_else(|| "libyaml error".to_owned()));
                truncated = false;
                break;
            }
            // SAFETY: `yaml_parser_parse` succeeded, so the event is
            // initialised; it is deleted right after rendering.
            let (line, end) = unsafe {
                let ev = event.assume_init_ref();
                let out = (render(ev), ev.type_ == unsafe_sys::YAML_STREAM_END_EVENT);
                unsafe_sys::yaml_event_delete(event.as_mut_ptr());
                out
            };
            lines.push(line);
            if end {
                truncated = false;
                break;
            }
        }
        drop(guard);
        Observation {
            events: Some(lines),
            error,
            round_trip: None,
            truncated,
        }
    }
}

/// Benchmark drain: pulls every event to `STREAM-END`, deleting each one
/// immediately, with no rendering and no event cap (unlike
/// [`LibYaml::observe`]). Returns the number of events.
///
/// # Errors
/// libyaml's problem message, if it rejects the input (or cannot start).
pub fn drain(input: &str) -> Result<usize, String> {
    let len = input
        .len()
        .try_into()
        .map_err(|_| "input too long for libyaml".to_owned())?;
    // libyaml keeps pointers into the parser struct, so it is boxed and never
    // moved; `boxed` outlives `guard` (reverse drop order).
    let mut boxed: Box<MaybeUninit<unsafe_sys::yaml_parser_t>> = Box::new(MaybeUninit::uninit());
    let parser = boxed.as_mut_ptr();
    // SAFETY: `parser` is valid, aligned, writable memory for a parser.
    if unsafe { unsafe_sys::yaml_parser_initialize(parser) }.fail {
        return Err("yaml_parser_initialize failed".to_owned());
    }
    let guard = ParserGuard(parser);
    // SAFETY: initialised parser; `input` outlives the parser (it is deleted
    // by `guard` before this function returns).
    unsafe { unsafe_sys::yaml_parser_set_input_string(guard.0, input.as_ptr(), len) };
    let mut count = 0usize;
    loop {
        let mut event = MaybeUninit::<unsafe_sys::yaml_event_t>::uninit();
        // SAFETY: initialised parser; `event` is writable event storage.
        if unsafe { unsafe_sys::yaml_parser_parse(guard.0, event.as_mut_ptr()) }.fail {
            // SAFETY: the parser is live; `problem` is null or a static
            // NUL-terminated message.
            let problem = unsafe { c_text((&*guard.0).problem.cast::<u8>()) };
            return Err(problem.unwrap_or_else(|| "libyaml error".to_owned()));
        }
        // SAFETY: `yaml_parser_parse` succeeded, so the event is initialised;
        // its type is read, then it is deleted exactly once.
        let end = unsafe {
            let end = event.assume_init_ref().type_ == unsafe_sys::YAML_STREAM_END_EVENT;
            unsafe_sys::yaml_event_delete(event.as_mut_ptr());
            end
        };
        count = count.saturating_add(1);
        if end {
            drop(guard);
            return Ok(count);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn renders_events() {
        let o = LibYaml.observe("--- !!map &m {a: [\"b\\n\"]}\n...\n");
        assert_eq!(o.error, None);
        assert_eq!(
            o.events.unwrap(),
            [
                "+STR",
                "+DOC ---",
                "+MAP {} &m <tag:yaml.org,2002:map>",
                "=VAL :a",
                "+SEQ []",
                "=VAL \"b\\n",
                "-SEQ",
                "-MAP",
                "-DOC ...",
                "-STR"
            ]
        );
        let bad = LibYaml.observe("[");
        assert!(bad.error.is_some());
        assert_eq!(bad.events.unwrap(), ["+STR", "+DOC", "+SEQ []"]);
    }

    #[test]
    fn drain_counts_every_event_uncapped() {
        // +STR +DOC +SEQ =VAL =ALI -SEQ -DOC -STR
        assert_eq!(drain("- &a x\n- *a\n"), Ok(8));
        let many = "- x\n".repeat(crate::MAX_EVENTS);
        assert_eq!(drain(&many), Ok(crate::MAX_EVENTS + 6));
        assert!(drain("[").is_err());
    }
}
