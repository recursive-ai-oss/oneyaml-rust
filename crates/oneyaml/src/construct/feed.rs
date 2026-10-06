//! The semantic view of the parser's event stream (the one parser,
//! read incrementally; nothing here buffers more than one event).

use oneyaml_core::{Diagnostic, Span};
use oneyaml_syntax::event::DirectiveKind;
use oneyaml_syntax::{EventKind, Events};

use super::{Ev, Node, Source, TagCache};
use crate::error::Error;

/// Semantic events from [`oneyaml_syntax::events_with`]: trivia skipped,
/// warnings collected, the stream ended by the first `Error` event (R5:
/// strict consumers stop there).
pub(crate) struct Feed<'a> {
    events: Events<'a>,
    end: usize,
    yaml11_next: bool,
    warnings: crate::diag_cap::Capped,
    done: bool,
    /// Each distinct tag resolved once.
    tags: TagCache<'a>,
}

impl<'a> Feed<'a> {
    pub(crate) fn new(events: Events<'a>, source_len: usize) -> Self {
        Self {
            events,
            end: source_len,
            yaml11_next: false,
            warnings: crate::diag_cap::Capped::with_keep(usize::MAX),
            done: false,
            tags: TagCache::default(),
        }
    }

    fn stream_end(&mut self) -> Ev<'a> {
        self.done = true;
        Ev::new(Node::StreamEnd, Span::empty(self.end))
    }

    /// An event's tag; an unresolvable one (`Y9000`) ends the stream.
    fn tag(
        &mut self,
        t: Option<oneyaml_syntax::event::TagSource<'a>>,
    ) -> Result<super::Tag<'a>, Error> {
        let r = self.tags.tag(t);
        if r.is_err() {
            self.done = true;
        }
        r
    }
}

/// `%YAML 1.0` and `%YAML 1.1` select the 1.1 behaviours when honoured (with
/// `honor_version_directive`); the parser has already reported the directive.
pub(crate) fn is_yaml11(version: &str) -> bool {
    matches!(version.split_once('.'), Some(("1", minor)) if minor.parse::<u32>().is_ok_and(|m| m <= 1))
}

impl<'a> Source<'a> for Feed<'a> {
    fn next_event(&mut self) -> Result<Ev<'a>, Error> {
        if self.done {
            return Ok(self.stream_end());
        }
        loop {
            let Some(e) = self.events.next() else {
                return Ok(self.stream_end());
            };
            let span = e.span();
            if e.is_warning() || e.is_error() {
                match e.into_event().into_kind() {
                    EventKind::Warning(d) => self.warnings.push(*d),
                    EventKind::Error(d) => {
                        self.done = true;
                        return Err(Error::from_diagnostic(*d));
                    }
                    _ => {}
                }
                continue;
            }
            let anchor = e.anchor().map(|a| a.name);
            let node = match e.kind() {
                EventKind::StreamEnd => return Ok(self.stream_end()),
                EventKind::Directive(_) => {
                    if let Some(DirectiveKind::Yaml { version, .. }) = e.directive() {
                        self.yaml11_next = is_yaml11(version);
                    }
                    continue;
                }
                EventKind::DocumentStart { .. } => Node::DocStart {
                    yaml11: std::mem::take(&mut self.yaml11_next),
                },
                EventKind::DocumentEnd { .. } => Node::DocEnd,
                EventKind::Scalar { style, .. } => {
                    let plain = matches!(style, oneyaml_syntax::event::ScalarStyle::Plain);
                    let Some(value) = e.value() else {
                        // The parser validated its own scalar (event docs,
                        // *Values are lazy*).
                        debug_assert!(false, "scalar value unreadable");
                        self.done = true;
                        return Err(Error::internal("scalar value unreadable", Some(span)));
                    };
                    Node::Scalar {
                        value,
                        plain,
                        tag: self.tag(e.tag_source())?,
                    }
                }
                EventKind::SequenceStart { .. } => Node::SeqStart {
                    tag: self.tag(e.tag_source())?,
                },
                EventKind::SequenceEnd => Node::SeqEnd,
                EventKind::MappingStart { .. } => Node::MapStart {
                    tag: self.tag(e.tag_source())?,
                },
                EventKind::MappingEnd => Node::MapEnd,
                EventKind::Alias => match e.alias_name() {
                    Some(name) => Node::Alias { name },
                    None => {
                        debug_assert!(false, "alias name unreadable");
                        self.done = true;
                        return Err(Error::internal("alias name unreadable", Some(span)));
                    }
                },
                // Trivia, StreamStart, and kinds added later carry no
                // meaning for construction.
                _ => continue,
            };
            return Ok(Ev {
                node,
                anchor,
                span,
                replayed: false,
            });
        }
    }

    fn take_warnings(&mut self) -> (Vec<Diagnostic>, crate::diag_cap::Dropped) {
        self.warnings.take()
    }

    fn collect_warnings(&mut self, warnings: crate::diag_cap::Capped) {
        self.warnings = warnings;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::construct::Tag;
    use oneyaml_core::Settings;

    fn nodes(src: &str) -> (Vec<Node<'_>>, Option<Error>) {
        let mut f = Feed::new(
            oneyaml_syntax::events_with(src, &Settings::new()),
            src.len(),
        );
        let mut out = Vec::new();
        loop {
            match f.next_event() {
                Ok(e) if e.node == Node::StreamEnd => return (out, None),
                Ok(e) => out.push(e.node),
                Err(e) => return (out, Some(e)),
            }
        }
    }

    #[test]
    fn semantic_events_only() {
        let (n, err) = nodes("%YAML 1.1\n--- &a !!str x # c\n...\n--- [*a]\n");
        assert!(err.is_none());
        assert_eq!(n[0], Node::DocStart { yaml11: true });
        assert!(
            matches!(&n[1], Node::Scalar { value, plain: true, tag: Tag::Named(t) }
            if value == "x" && t == "tag:yaml.org,2002:str")
        );
        assert_eq!(n[2], Node::DocEnd);
        assert_eq!(n[3], Node::DocStart { yaml11: false });
        assert_eq!(n[5], Node::Alias { name: "a" });
        let (n, _) = nodes("! 'a'");
        assert!(matches!(
            &n[1],
            Node::Scalar {
                plain: false,
                tag: Tag::NonSpecific,
                ..
            }
        ));
    }

    #[test]
    fn first_error_ends_the_stream() {
        let (n, err) = nodes("a: [\n");
        assert!(!n.is_empty());
        assert_eq!(err.unwrap().kind(), crate::ErrorKind::Syntax);
        assert!(is_yaml11("1.0") && is_yaml11("1.1"));
        assert!(!is_yaml11("1.2") && !is_yaml11("2.0") && !is_yaml11("1.x"));
    }
}
