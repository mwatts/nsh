//! Spans: runs of characters inside the text views, which the steps name
//! as they name views. The selector's last part matches characters instead
//! of views: `id:<testID>` the span of a nested `<Text>` with that test id,
//! `text:<text>` (or `/<regex>/`) each place the text reads it, a bare
//! string a test id, else a text; the parts before it name the views to
//! look in. A press on a span lands on its first character, where the
//! platforms' link hit testing finds the innermost pressable span
//! ([`nsh_core::AttributedText::link_at`]), else the block's.

use nsh_core::{AttributedText, EventKind, NodeId, ResolvedTextStyle};

use super::{Matcher, Pick, Screen, Selector, TextMatch};

/// Characters of a text view a span selector matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpanHit {
    /// The text view.
    pub(crate) view: NodeId,
    /// Start offset (UTF-16 units, inclusive).
    pub(crate) start: u32,
    /// End offset (UTF-16 units, exclusive).
    pub(crate) end: u32,
}

/// The UTF-16 offset of byte `at` of `text`.
fn utf16_at(text: &str, at: usize) -> u32 {
    text[..at].encode_utf16().count() as u32
}

/// Where `want` reads in `text`, as UTF-16 ranges.
fn occurrences(text: &str, want: &TextMatch) -> Vec<(u32, u32)> {
    let ranges: Vec<(usize, usize)> = match want {
        TextMatch::Exact(want) if want.is_empty() => Vec::new(),
        TextMatch::Exact(want) => text
            .match_indices(want.as_str())
            .map(|(at, s)| (at, at + s.len()))
            .collect(),
        TextMatch::Regex(re) => re
            .find_iter(text)
            .filter(|m| !m.is_empty())
            .map(|m| (m.start(), m.end()))
            .collect(),
    };
    ranges
        .into_iter()
        .map(|(a, b)| (utf16_at(text, a), utf16_at(text, b)))
        .collect()
}

impl Screen {
    /// The spans on screen `selector` names, in the order a user's eyes
    /// find their views; an error for a last part that names no spans
    /// (`label:`, `role:`, `view:`).
    pub(crate) fn spans(&self, selector: &Selector) -> Result<Vec<SpanHit>, String> {
        let scope = selector.scope().map(|s| self.resolve(&s));
        let texts: Vec<(NodeId, &AttributedText)> = self
            .by_layer()
            .into_iter()
            .filter(|id| {
                scope
                    .as_ref()
                    .is_none_or(|scope| scope.iter().any(|s| *s == *id || self.is_inside(*id, *s)))
            })
            .filter_map(|id| Some((id, self.nodes.get(&id)?.attributed.as_deref()?)))
            .collect();
        let by_test_id = |want: &str| -> Vec<SpanHit> {
            texts
                .iter()
                .flat_map(|(view, text)| {
                    text.test_ids
                        .iter()
                        .filter(|t| &*t.test_id == want)
                        .map(|t| SpanHit {
                            view: *view,
                            start: t.start,
                            end: t.end,
                        })
                })
                .collect()
        };
        let by_text = |want: &TextMatch| -> Vec<SpanHit> {
            texts
                .iter()
                .flat_map(|(view, text)| {
                    occurrences(&text.text, want)
                        .into_iter()
                        .map(|(start, end)| SpanHit {
                            view: *view,
                            start,
                            end,
                        })
                })
                .collect()
        };
        let last = selector.parts.last().expect("a selector has a part");
        let mut found = match &last.matcher {
            Matcher::TestId(want) => by_test_id(want),
            Matcher::Text(want) => by_text(want),
            Matcher::Any(want) => {
                let ids = by_test_id(want);
                if ids.is_empty() {
                    by_text(&TextMatch::Exact(want.trim().to_owned()))
                } else {
                    ids
                }
            }
            Matcher::Label(_) | Matcher::Role { .. } | Matcher::View(_) => {
                return Err(format!(
                    "`{selector}`: a span is named by `id:<testID>` or `text:<text>`"
                ));
            }
        };
        if let Some(n) = last.nth {
            found = found.get(n).copied().into_iter().collect();
        }
        Ok(found)
    }

    /// The one span `selector` names. Distinct from [`Screen::span`],
    /// which looks up a pressable span by node id.
    pub(crate) fn span_hit(&self, selector: &Selector) -> Result<SpanHit, String> {
        match self.spans(selector)?[..] {
            [one] => Ok(one),
            [] => Err(format!("no span matches `{selector}`")),
            ref all => Err(format!(
                "`{selector}` matches {} spans; add `nth=<n>` or name the text views to look in",
                all.len()
            )),
        }
    }

    /// Where a press on `span` goes, as the platforms route a press on its
    /// first character: the innermost pressable span there, else the text
    /// view or its nearest pressable ancestor ([`Pick::Disabled`] when one
    /// on the way is disabled).
    pub(crate) fn span_press_target(&self, span: SpanHit) -> Pick {
        let link = self
            .nodes
            .get(&span.view)
            .and_then(|v| v.attributed.as_deref())
            .and_then(|text| text.link_at(span.start));
        if let Some(link) = link {
            return Pick::One(link.node);
        }
        match self.up_to(span.view, |v| v.events.contains(EventKind::Press)) {
            Some(target) if self.disabled_from(span.view, Some(target)) => Pick::Disabled,
            Some(target) => Pick::One(target),
            None => Pick::None,
        }
    }

    /// The style of the span `selector` names (its first character's,
    /// inherited from the spans around it: color, font, decoration and the
    /// background of a nested `<Text>`'s `backgroundColor`); an error when
    /// none or several match.
    pub fn span_style(&self, selector: &str) -> Result<ResolvedTextStyle, String> {
        let span = self.span_hit(&Selector::parse(selector)?)?;
        let style = self
            .nodes
            .get(&span.view)
            .and_then(|v| v.attributed.as_deref())
            .and_then(|text| text.span_at(span.start))
            .map(|s| s.style)
            .expect("a span is in a text view");
        Ok(style)
    }
}

#[cfg(test)]
mod tests {
    use nsh_core::ui::{Root, prelude::*};
    use nsh_engine::EngineConfig;

    use crate::headless::Headless;

    /// A Rust paragraph with a link and a marked word.
    #[component]
    fn Linked() -> impl IntoElement {
        let opened = signal(0_u32);
        view()
            .child(
                text("See ")
                    .child(
                        text("the guide")
                            .test_id("guide")
                            .underline()
                            .on_press(move || opened.update(|n| *n += 1)),
                    )
                    .child(" or ")
                    .child(text("this").test_id("marked").bg(rgb(0xfde68a))),
            )
            .child(text(move || format!("Opened {}", opened.get())))
    }

    #[test]
    fn rust_spans_are_pressed_and_read_by_test_id_or_text() {
        let mut app = Headless::start(EngineConfig::new(Root::of::<Linked>())).unwrap();
        app.first_render().unwrap();
        app.press_span("id:guide").unwrap();
        app.wait_for("Opened 1").unwrap();
        app.press_span("text:guide").unwrap();
        app.wait_for("Opened 2").unwrap();
        // Characters outside the link go to the block, which hears nothing.
        assert_eq!(
            app.press_span("text:See").unwrap_err(),
            "no pressable span matches `text:See`; on screen: [\"See the guide or this\", \"Opened 2\"]"
        );
        let screen = app.screen();
        assert!(screen.span_style("id:guide").unwrap().decoration.underline);
        assert_eq!(screen.span_style("id:guide").unwrap().background, None);
        let marked = screen.span_style("id:marked").unwrap().background.unwrap();
        assert_eq!(marked.color, nsh_core::Color(0xfde6_8aff));
        assert_eq!(
            screen
                .span_style("text:this")
                .unwrap()
                .background
                .map(|b| b.color),
            Some(marked.color)
        );
    }
}
