//! A dependency-free horizontal bar chart in SVG.
//!
//! `bench` is unpublished, but a plotting stack (plotters and
//! its font/raster dependencies) would be the heaviest dependency in the
//! workspace for one fixed chart shape. This writes the SVG text directly:
//! deterministic (byte-stable output, fixed-precision coordinates), and
//! honest by construction: linear, zero-based bars, a labelled unit and
//! direction, a marked parity line, slower bars in their own colour and
//! hatching and labelled "slower", no log scale.
//!
//! One far outlier must not set the scale for every other bar (a 63× bar
//! would squash 1.1× and 0.79× into the same sliver). So the axis covers
//! the bars up to [`CUT`] (see [`scale`]); a longer bar runs a little past
//! the axis end, is drawn broken there (two slanted gaps), and is labelled
//! with its value, and a caption line says so.
//!
//! [`compact_svg`] is the small chart for the READMEs: one group per
//! operation, one thin bar per workload (the same linear, zero-based scale,
//! parity line, slower colour and cut), and beside each group its range and
//! how many workloads are slower. It drops the per-bar labels, not bars:
//! every workload keeps its own bar, so no loss is averaged away.

use std::fmt::Write as _;

/// Colours of one variant.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Background.
    pub background: &'static str,
    /// Main text.
    pub text: &'static str,
    /// Secondary text, axis.
    pub muted: &'static str,
    /// Grid lines.
    pub grid: &'static str,
    /// oneyaml's bars when it is faster.
    pub accent: &'static str,
    /// oneyaml's bars when it is slower.
    pub slower: &'static str,
}

/// For light backgrounds (GitHub light palette).
pub const LIGHT: Theme = Theme {
    background: "#ffffff",
    text: "#1f2328",
    muted: "#59636e",
    grid: "#d1d9e0",
    accent: "#0969da",
    slower: "#bc4c00",
};

/// For dark backgrounds (GitHub dark palette).
pub const DARK: Theme = Theme {
    background: "#0d1117",
    text: "#f0f6fc",
    muted: "#9198a1",
    grid: "#3d444d",
    accent: "#4493f8",
    slower: "#f0883e",
};

/// One bar: oneyaml relative to one competitor.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    /// Row label (workload and competitor).
    pub label: String,
    /// Competitor ÷ oneyaml; `None` draws a labelled row with no bar (not
    /// measured).
    pub value: Option<f64>,
    /// The printed value at the bar's end.
    pub text: String,
}

/// One operation's bars.
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    /// Panel heading.
    pub title: String,
    /// The bars.
    pub bars: Vec<Bar>,
}

const WIDTH: f64 = 760.0;
const LEFT: f64 = 250.0;
const RIGHT: f64 = 100.0;
const ROW: f64 = 22.0;
const BAR: f64 = 14.0;
const PANEL_HEAD: f64 = 24.0;
const PANEL_GAP: f64 = 10.0;
const TOP: f64 = 58.0;
/// The extra caption line when a bar is cut.
const CUT_NOTE: f64 = 18.0;
/// How far a cut bar runs past the axis end.
const OVERRUN: f64 = 22.0;
/// The estimated width of one character of a value label (13 px text).
const LABEL_CHAR: f64 = 7.0;
const BOTTOM: f64 = 46.0;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The axis maximum and tick step for the largest value: the next multiple
/// of the step, never below 1.5 so the parity line is always inside.
#[must_use]
pub fn axis(max_value: f64) -> (f64, f64) {
    let m = if max_value.is_finite() {
        max_value.max(1.5)
    } else {
        1.5
    };
    let step = if m <= 4.0 {
        0.5
    } else if m <= 8.0 {
        1.0
    } else {
        (m / 4.0).ceil()
    };
    ((m / step).ceil() * step, step)
}

/// Bars up to this many × always fit the axis; longer ones may be cut.
pub const CUT: f64 = 4.0;

/// The axis maximum and tick step for `values`: [`axis`] of the largest
/// value up to [`CUT`], so every such bar is drawn whole and only bars
/// longer than the axis (all above [`CUT`]) are cut.
#[must_use]
pub fn scale(values: impl IntoIterator<Item = f64>) -> (f64, f64) {
    let shown = values
        .into_iter()
        .filter(|v| v.is_finite() && *v <= CUT)
        .fold(0.0_f64, f64::max);
    axis(shown)
}

/// The chart.
#[must_use]
pub fn svg(panels: &[Panel], theme: &Theme, description: &str) -> String {
    let rows: usize = panels.iter().map(|p| p.bars.len()).sum();
    let plot = WIDTH - LEFT - RIGHT;
    let values = || {
        panels
            .iter()
            .flat_map(|p| p.bars.iter().filter_map(|b| b.value))
    };
    let (axis_max, step) = scale(values());
    let cut = values().any(|v| v > axis_max);
    let top = if cut { TOP + CUT_NOTE } else { TOP };
    let body = panels.len() as f64 * (PANEL_HEAD + PANEL_GAP) + rows as f64 * ROW;
    let height = top + body + BOTTOM;
    let x = |v: f64| LEFT + v.clamp(0.0, axis_max) / axis_max * plot;
    let axis_y = top + body;
    let axis_end = x(axis_max);
    let font = "font-family=\"system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif\"";

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{WIDTH:.0}\" height=\"{height:.0}\" viewBox=\"0 0 {WIDTH:.0} {height:.0}\" role=\"img\" aria-labelledby=\"title desc\" {font} font-size=\"13\">"
    );
    let _ = writeln!(
        s,
        "<title id=\"title\">oneyaml: × faster than each competitor (CPU instructions)</title>"
    );
    let _ = writeln!(s, "<desc id=\"desc\">{}</desc>", escape(description));
    let _ = writeln!(
        s,
        "<defs><pattern id=\"slower\" width=\"6\" height=\"6\" patternUnits=\"userSpaceOnUse\" patternTransform=\"rotate(45)\"><rect width=\"6\" height=\"6\" fill=\"{0}\" fill-opacity=\"0.4\"/><rect width=\"3\" height=\"6\" fill=\"{0}\"/></pattern></defs>",
        theme.slower
    );
    let _ = writeln!(
        s,
        "<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        theme.background
    );
    let _ = writeln!(
        s,
        "<text x=\"16\" y=\"24\" fill=\"{}\" font-size=\"15\" font-weight=\"600\">oneyaml: × faster than each competitor (higher is better)</text>",
        theme.text
    );
    let _ = writeln!(
        s,
        "<text x=\"16\" y=\"43\" fill=\"{}\">competitor CPU instructions ÷ oneyaml's · dashed line = 1.0× (equal) · orange, hatched = oneyaml slower</text>",
        theme.muted
    );
    if cut {
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"61\" fill=\"{}\">bars longer than {axis_max:.1}× run past the axis end and are drawn broken; the number is the value</text>",
            theme.muted
        );
    }

    // Tick labels; the grid is drawn per panel, so no line crosses a
    // panel heading.
    let ticks = (axis_max / step).round() as u32;
    for i in 0..=ticks {
        let v = f64::from(i) * step;
        let gx = x(v);
        let _ = writeln!(
            s,
            "<text x=\"{gx:.1}\" y=\"{:.1}\" fill=\"{}\" text-anchor=\"middle\">{v:.1}×</text>",
            axis_y + 16.0,
            theme.muted
        );
    }
    let _ = writeln!(
        s,
        "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" text-anchor=\"middle\">× faster (competitor ÷ oneyaml instructions; bars start at 0)</text>",
        LEFT + plot / 2.0,
        axis_y + 36.0,
        theme.muted
    );

    // Panels.
    let px = x(1.0);
    let mut y = top;
    for p in panels {
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"{:.1}\" fill=\"{}\" font-weight=\"600\">{}</text>",
            y + 17.0,
            theme.text,
            escape(&p.title)
        );
        y += PANEL_HEAD;
        // The grid and the parity line span this panel's rows only, under
        // its bars and their value labels.
        let rows_end = y + p.bars.len() as f64 * ROW;
        if !p.bars.is_empty() {
            for i in 0..=ticks {
                let gx = x(f64::from(i) * step);
                let _ = writeln!(
                    s,
                    "<line x1=\"{gx:.1}\" y1=\"{y:.1}\" x2=\"{gx:.1}\" y2=\"{rows_end:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
                    theme.grid
                );
            }
            let _ = writeln!(
                s,
                "<line x1=\"{px:.1}\" y1=\"{y:.1}\" x2=\"{px:.1}\" y2=\"{rows_end:.1}\" stroke=\"{}\" stroke-width=\"1.5\" stroke-dasharray=\"4 3\"/>",
                theme.text
            );
        }
        for b in &p.bars {
            let _ = writeln!(
                s,
                "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" text-anchor=\"end\">{}</text>",
                LEFT - 8.0,
                y + 15.0,
                theme.text,
                escape(&b.label)
            );
            let (end, fill) = match b.value {
                Some(v) => {
                    let fill = if v < 1.0 {
                        "url(#slower)"
                    } else {
                        theme.accent
                    };
                    let broken = v > axis_max;
                    let end = if broken { axis_end + OVERRUN } else { x(v) };
                    let _ = writeln!(
                        s,
                        "<rect x=\"{LEFT:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{BAR:.1}\" fill=\"{fill}\"/>",
                        y + 4.0,
                        end - LEFT
                    );
                    if broken {
                        // Two slanted gaps just past the axis end: the bar
                        // is cut there, not drawn to scale.
                        for dx in [6.0, 12.0] {
                            let _ = writeln!(
                                s,
                                "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"2.5\"/>",
                                axis_end + dx - 3.0,
                                y + 4.0 + BAR + 2.0,
                                axis_end + dx + 3.0,
                                y + 2.0,
                                theme.background
                            );
                        }
                    }
                    (end, theme.text)
                }
                None => (LEFT, theme.muted),
            };
            // An opaque box under the value label, so the parity and grid
            // lines never run through it (a slower bar's label always
            // crosses the parity line). The width is an estimate of the
            // text's; a little too wide only hides more grid.
            let chars = b.text.chars().count() as f64;
            let _ = writeln!(
                s,
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"{}\"/>",
                end + 3.0,
                y + 2.0,
                chars * LABEL_CHAR + 6.0,
                ROW - 4.0,
                theme.background
            );
            let _ = writeln!(
                s,
                "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{fill}\" stroke=\"{}\" stroke-width=\"4\" paint-order=\"stroke\">{}</text>",
                end + 6.0,
                y + 15.0,
                theme.background,
                escape(&b.text)
            );
            y += ROW;
        }
        // The zero axis of this panel, on top of its bars.
        if !p.bars.is_empty() {
            let _ = writeln!(
                s,
                "<line x1=\"{LEFT:.1}\" y1=\"{:.1}\" x2=\"{LEFT:.1}\" y2=\"{rows_end:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
                rows_end - p.bars.len() as f64 * ROW,
                theme.muted
            );
        }
        y += PANEL_GAP;
    }
    // The axis line under the last panel, where the ticks are.
    let _ = writeln!(
        s,
        "<line x1=\"{LEFT:.1}\" y1=\"{axis_y:.1}\" x2=\"{axis_end:.1}\" y2=\"{axis_y:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
        theme.muted
    );
    s.push_str("</svg>\n");
    s
}

/// How a note beside a compact group is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Main text (the range).
    Text,
    /// Secondary text.
    Muted,
    /// In the slower colour (a count of slower workloads).
    Slower,
}

/// One operation of the compact chart.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The operation.
    pub title: String,
    /// The line under it (the competitors).
    pub subtitle: String,
    /// One bar per workload, in the order the caption names; `None` draws
    /// no bar (not measured).
    pub bars: Vec<Option<f64>>,
    /// The lines beside the group (range, counts).
    pub notes: Vec<(String, Tone)>,
}

const C_LEFT: f64 = 224.0;
const C_RIGHT: f64 = 170.0;
const C_BAR: f64 = 6.0;
const C_PITCH: f64 = 8.0;
const C_NOTE: f64 = 14.0;
const C_LABEL: f64 = 32.0;
const C_GAP: f64 = 18.0;
const C_TOP: f64 = 96.0;
const C_OVERRUN: f64 = 16.0;

/// The height of a group's rows: its bars, its notes or its label, whichever
/// is tallest.
fn group_height(g: &Group) -> f64 {
    let bars = (g.bars.len() as f64 * C_PITCH - (C_PITCH - C_BAR)).max(0.0);
    bars.max(g.notes.len() as f64 * C_NOTE).max(C_LABEL)
}

/// The compact chart. `workloads` names the bars of every group, top to
/// bottom, in the caption.
#[must_use]
pub fn compact_svg(
    groups: &[Group],
    workloads: &[String],
    theme: &Theme,
    description: &str,
) -> String {
    let plot = WIDTH - C_LEFT - C_RIGHT;
    let values = || groups.iter().flat_map(|g| g.bars.iter().filter_map(|b| *b));
    let (axis_max, step) = scale(values());
    let cut = values().any(|v| v > axis_max);
    let x = |v: f64| C_LEFT + v.clamp(0.0, axis_max) / axis_max * plot;
    let axis_end = x(axis_max);
    let body: f64 = groups.iter().map(|g| group_height(g) + C_GAP).sum();
    let axis_y = C_TOP + body - C_GAP / 2.0;
    let height = axis_y + BOTTOM;
    let font = "font-family=\"system-ui, -apple-system, 'Segoe UI', Helvetica, Arial, sans-serif\"";
    let broken = if cut {
        format!("; bars longer than {axis_max:.1}× are drawn broken")
    } else {
        String::new()
    };
    let captions = [
        (
            43,
            "competitor's CPU instructions ÷ oneyaml's, not wall-clock time; dashed line = 1.0× (equal)"
                .to_owned(),
        ),
        (61, format!("orange, hatched bars = oneyaml slower{broken}")),
        (
            79,
            format!("one bar per workload, top to bottom: {}", workloads.join(", ")),
        ),
    ];
    // The notes start right of the plot; the chart widens past `WIDTH`
    // when a note or a caption line would not fit (estimated widths).
    let nx = axis_end + C_OVERRUN + 14.0;
    let chars = |t: &str| t.chars().count() as f64;
    let width = groups
        .iter()
        .flat_map(|g| &g.notes)
        .map(|(t, _)| nx + chars(t) * LABEL_CHAR + 16.0)
        .chain(
            captions
                .iter()
                .map(|(_, t)| 16.0 + chars(t) * LABEL_CHAR + 16.0),
        )
        .fold(WIDTH, f64::max)
        .ceil();

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" viewBox=\"0 0 {width:.0} {height:.0}\" role=\"img\" aria-labelledby=\"title desc\" {font} font-size=\"13\">"
    );
    let _ = writeln!(
        s,
        "<title id=\"title\">oneyaml: × faster than each competitor (CPU instructions)</title>"
    );
    let _ = writeln!(s, "<desc id=\"desc\">{}</desc>", escape(description));
    let _ = writeln!(
        s,
        "<defs><pattern id=\"slower\" width=\"4\" height=\"4\" patternUnits=\"userSpaceOnUse\" patternTransform=\"rotate(45)\"><rect width=\"4\" height=\"4\" fill=\"{0}\" fill-opacity=\"0.45\"/><rect width=\"2\" height=\"4\" fill=\"{0}\"/></pattern></defs>",
        theme.slower
    );
    let _ = writeln!(
        s,
        "<rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        theme.background
    );
    let _ = writeln!(
        s,
        "<text x=\"16\" y=\"24\" fill=\"{}\" font-size=\"15\" font-weight=\"600\">oneyaml: × faster than each competitor (higher is better)</text>",
        theme.text
    );
    for (y, line) in captions {
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"{y}\" fill=\"{}\">{}</text>",
            theme.muted,
            escape(&line)
        );
    }

    let ticks = (axis_max / step).round() as u32;
    let px = x(1.0);
    let mut y = C_TOP;
    for g in groups {
        let h = group_height(g);
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"{:.1}\" fill=\"{}\" font-weight=\"600\">{}</text>",
            y + 12.0,
            theme.text,
            escape(&g.title)
        );
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"{:.1}\" fill=\"{}\" font-size=\"12\">{}</text>",
            y + 28.0,
            theme.muted,
            escape(&g.subtitle)
        );
        let (top, bottom) = (y - 3.0, y + h + 3.0);
        for i in 0..=ticks {
            let gx = x(f64::from(i) * step);
            let _ = writeln!(
                s,
                "<line x1=\"{gx:.1}\" y1=\"{top:.1}\" x2=\"{gx:.1}\" y2=\"{bottom:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
                theme.grid
            );
        }
        for (i, b) in g.bars.iter().enumerate() {
            let Some(v) = *b else { continue };
            let by = y + i as f64 * C_PITCH;
            let fill = if v < 1.0 {
                "url(#slower)"
            } else {
                theme.accent
            };
            let long = v > axis_max;
            let end = if long { axis_end + C_OVERRUN } else { x(v) };
            let _ = writeln!(
                s,
                "<rect x=\"{C_LEFT:.1}\" y=\"{by:.1}\" width=\"{:.1}\" height=\"{C_BAR:.1}\" fill=\"{fill}\"/>",
                end - C_LEFT
            );
            if long {
                // A slanted gap just past the axis end: the bar is cut
                // there, not drawn to scale.
                let _ = writeln!(
                    s,
                    "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" stroke-width=\"2.5\"/>",
                    axis_end + 5.0,
                    by + C_BAR + 1.0,
                    axis_end + 9.0,
                    by - 1.0,
                    theme.background
                );
            }
        }
        // The parity line and the zero axis, over the bars.
        let _ = writeln!(
            s,
            "<line x1=\"{px:.1}\" y1=\"{top:.1}\" x2=\"{px:.1}\" y2=\"{bottom:.1}\" stroke=\"{}\" stroke-width=\"1.5\" stroke-dasharray=\"4 3\"/>",
            theme.text
        );
        let _ = writeln!(
            s,
            "<line x1=\"{C_LEFT:.1}\" y1=\"{top:.1}\" x2=\"{C_LEFT:.1}\" y2=\"{bottom:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
            theme.muted
        );
        for (i, (text, tone)) in g.notes.iter().enumerate() {
            let (fill, size) = match tone {
                Tone::Text => (theme.text, "13"),
                Tone::Muted => (theme.muted, "12"),
                Tone::Slower => (theme.slower, "12"),
            };
            let _ = writeln!(
                s,
                "<text x=\"{nx:.1}\" y=\"{:.1}\" fill=\"{fill}\" font-size=\"{size}\">{}</text>",
                y + 11.0 + i as f64 * C_NOTE,
                escape(text)
            );
        }
        y += h + C_GAP;
    }
    for i in 0..=ticks {
        let v = f64::from(i) * step;
        let _ = writeln!(
            s,
            "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" text-anchor=\"middle\">{v:.1}×</text>",
            x(v),
            axis_y + 16.0,
            theme.muted
        );
    }
    let _ = writeln!(
        s,
        "<text x=\"{:.1}\" y=\"{:.1}\" fill=\"{}\" text-anchor=\"middle\">× faster (competitor ÷ oneyaml instructions; bars start at 0)</text>",
        C_LEFT + plot / 2.0,
        axis_y + 36.0,
        theme.muted
    );
    let _ = writeln!(
        s,
        "<line x1=\"{C_LEFT:.1}\" y1=\"{axis_y:.1}\" x2=\"{axis_end:.1}\" y2=\"{axis_y:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
        theme.muted
    );
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn panel() -> Vec<Panel> {
        vec![Panel {
            title: "serde read".to_owned(),
            bars: vec![
                Bar {
                    label: "a vs b".to_owned(),
                    value: Some(1.2),
                    text: "1.2×".to_owned(),
                },
                Bar {
                    label: "c <&> d".to_owned(),
                    value: Some(0.28),
                    text: "0.28× slower".to_owned(),
                },
                Bar {
                    label: "e vs f".to_owned(),
                    value: None,
                    text: "not measured".to_owned(),
                },
            ],
        }]
    }

    #[test]
    fn axis_is_zero_based_and_contains_parity() {
        assert_eq!(axis(1.2), (1.5, 0.5));
        assert_eq!(axis(2.2), (2.5, 0.5));
        assert_eq!(axis(3.6), (4.0, 0.5));
        assert_eq!(axis(5.1), (6.0, 1.0));
        assert_eq!(axis(17.0), (20.0, 5.0));
        assert_eq!(axis(f64::NAN), (1.5, 0.5));
    }

    #[test]
    fn bars_are_linear_from_zero_and_slower_is_coloured() {
        let svg = svg(&panel(), &LIGHT, "desc & more");
        // Plot width 410 over an axis of 1.5×: 1.2× is 328 px, 0.28× 76.5 px.
        assert!(svg.contains("width=\"328.0\" height=\"14.0\" fill=\"#0969da\""));
        // The slower bar is hatched in the slower colour.
        assert!(svg.contains("width=\"76.5\" height=\"14.0\" fill=\"url(#slower)\""));
        assert!(svg.contains("<pattern id=\"slower\""));
        assert!(svg.contains("fill=\"#bc4c00\" fill-opacity"));
        assert!(svg.contains("c &lt;&amp;&gt; d"));
        // The unmeasured row keeps its label and says so, with no bar.
        assert!(svg.contains(">e vs f</text>"));
        assert!(svg.contains("fill=\"#59636e\" stroke=\"#ffffff\" stroke-width=\"4\" paint-order=\"stroke\">not measured</text>"));
        assert_eq!(svg.matches("height=\"14.0\"").count(), 2);
        assert!(svg.contains("<desc id=\"desc\">desc &amp; more</desc>"));
        assert!(svg.contains(">0.0×</text>"));
        assert!(svg.contains("stroke-dasharray"));
        assert!(svg.ends_with("</svg>\n"));
        assert_eq!(svg, super::svg(&panel(), &LIGHT, "desc & more"));
        assert_ne!(svg, super::svg(&panel(), &DARK, "desc & more"));
    }

    #[test]
    fn the_scale_ignores_bars_past_the_cut() {
        assert_eq!(scale([1.2, 0.28]), (1.5, 0.5));
        assert_eq!(scale([1.1, 0.79, 3.1, 7.3, 63.0]), (3.5, 0.5));
        // Up to the cut every bar fits.
        assert_eq!(scale([0.5, 4.0]), (4.0, 0.5));
        assert_eq!(scale([f64::NAN, f64::INFINITY]), (1.5, 0.5));
        assert_eq!(scale([]), (1.5, 0.5));
    }

    fn outliers() -> Vec<Panel> {
        let bar = |label: &str, v: f64| Bar {
            label: label.to_owned(),
            value: Some(v),
            text: format!("{v}×"),
        };
        vec![
            Panel {
                title: "first".to_owned(),
                bars: vec![bar("a", 1.1), bar("b", 0.79)],
            },
            Panel {
                title: "second, with a long heading".to_owned(),
                bars: vec![bar("c", 3.1), bar("d", 63.0)],
            },
        ]
    }

    #[test]
    fn an_outlier_is_cut_and_does_not_set_the_scale() {
        let svg = svg(&outliers(), &LIGHT, "");
        // Axis 0–3.5× over 410 px: wins and losses stay apart.
        assert!(svg.contains(">3.5×</text>"));
        assert!(!svg.contains(">4.0×</text>"));
        assert!(svg.contains("width=\"128.9\" height=\"14.0\" fill=\"#0969da\""));
        assert!(svg.contains("width=\"92.5\" height=\"14.0\" fill=\"url(#slower)\""));
        assert!(svg.contains("width=\"363.1\" height=\"14.0\" fill=\"#0969da\""));
        // The 63× bar ends 22 px past the axis end, broken by two gaps, and
        // keeps its value; the caption says bars are cut.
        assert!(svg.contains("width=\"432.0\" height=\"14.0\" fill=\"#0969da\""));
        assert_eq!(svg.matches("stroke-width=\"2.5\"").count(), 2);
        assert!(svg.contains(">63×</text>"));
        assert!(svg.contains("bars longer than 3.5× run past the axis end"));
        // Without an outlier there is no cut and no caption line.
        let plain = super::svg(&panel(), &LIGHT, "");
        assert!(!plain.contains("stroke-width=\"2.5\""));
        assert!(!plain.contains("run past the axis end"));
    }

    #[test]
    fn the_parity_line_spans_each_panel_rows_only() {
        let svg = svg(&outliers(), &LIGHT, "");
        // TOP 58 + caption 18; each panel: a 24 px heading, then its rows.
        let parity: Vec<&str> = svg
            .lines()
            .filter(|l| l.contains("stroke-dasharray"))
            .collect();
        assert_eq!(parity.len(), 2);
        assert!(
            parity[0].contains("y1=\"100.0\" x2=\"367.1\" y2=\"144.0\""),
            "{}",
            parity[0]
        );
        assert!(
            parity[1].contains("y1=\"178.0\" x2=\"367.1\" y2=\"222.0\""),
            "{}",
            parity[1]
        );
        // The second heading sits between them, clear of both.
        assert!(svg.contains("<text x=\"16\" y=\"171.0\""));
    }

    fn groups(values: &[&[Option<f64>]]) -> Vec<Group> {
        values
            .iter()
            .enumerate()
            .map(|(i, bars)| Group {
                title: format!("op {i}"),
                subtitle: "vs <x>".to_owned(),
                bars: bars.to_vec(),
                notes: vec![
                    ("0.28× to 1.2×".to_owned(), Tone::Text),
                    ("slower on 1 of 3".to_owned(), Tone::Slower),
                    ("faster on 1 of 3".to_owned(), Tone::Muted),
                ],
            })
            .collect()
    }

    fn names() -> Vec<String> {
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    }

    #[test]
    fn compact_bars_are_linear_and_keep_every_loss() {
        let g = groups(&[&[Some(1.2), Some(0.28), None]]);
        let svg = compact_svg(&g, &names(), &LIGHT, "d & e");
        // Plot width 366 over an axis of 1.5×: 1.2× is 292.8 px, 0.28× 68.3 px.
        assert!(svg.contains("width=\"292.8\" height=\"6.0\" fill=\"#0969da\""));
        assert!(svg.contains("width=\"68.3\" height=\"6.0\" fill=\"url(#slower)\""));
        // The unmeasured workload has no bar.
        assert_eq!(svg.matches("height=\"6.0\"").count(), 2);
        assert!(svg.contains("<pattern id=\"slower\""));
        assert!(svg.contains("stroke-dasharray"));
        assert!(svg.contains(">0.0×</text>") && svg.contains(">1.5×</text>"));
        assert!(svg.contains("<desc id=\"desc\">d &amp; e</desc>"));
        assert!(svg.contains(">vs &lt;x&gt;</text>"));
        assert!(svg.contains("not wall-clock time"));
        assert!(svg.contains(">one bar per workload, top to bottom: a, b, c</text>"));
        // The notes, the slower count in the slower colour.
        assert!(svg.contains("fill=\"#1f2328\" font-size=\"13\">0.28× to 1.2×</text>"));
        assert!(svg.contains("fill=\"#bc4c00\" font-size=\"12\">slower on 1 of 3</text>"));
        assert!(svg.contains("fill=\"#59636e\" font-size=\"12\">faster on 1 of 3</text>"));
        assert!(!svg.contains("drawn broken"));
        assert!(svg.ends_with("</svg>\n"));
        assert_eq!(svg, compact_svg(&g, &names(), &LIGHT, "d & e"));
        assert_ne!(svg, compact_svg(&g, &names(), &DARK, "d & e"));
        // About half the height of the detailed chart of the same rows.
        assert!(
            svg.starts_with(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"760\" height=\"193\""
            )
        );
    }

    #[test]
    fn compact_notes_always_fit() {
        let mut g = groups(&[&[None, None, None]]);
        g[0].notes = vec![
            ("not measured".to_owned(), Tone::Text),
            ("not measured on 5 of 5".to_owned(), Tone::Muted),
        ];
        let svg = compact_svg(&g, &names(), &LIGHT, "");
        // Notes start at 620 px; 22 characters at 7 px each, plus a 16 px
        // margin: the chart widens to 790 px instead of clipping.
        assert!(svg.contains("width=\"790\"") && svg.contains("viewBox=\"0 0 790 "));
        assert!(svg.contains("<text x=\"620.0\" y=\"121.0\" fill=\"#59636e\" font-size=\"12\">not measured on 5 of 5</text>"));
        // Short notes keep the usual width.
        let narrow = compact_svg(&groups(&[&[Some(1.0)]]), &names(), &LIGHT, "");
        assert!(narrow.contains("width=\"760\""));
        assert!(narrow.contains(
            "<title id=\"title\">oneyaml: × faster than each competitor (CPU instructions)</title>"
        ));
        assert!(
            narrow.contains(">oneyaml: × faster than each competitor (higher is better)</text>")
        );
        assert!(!narrow.contains("fastest"));
    }

    #[test]
    fn compact_outliers_are_cut_not_scaled() {
        let g = groups(&[&[Some(1.1), Some(0.79)], &[Some(3.1), Some(63.0)]]);
        let svg = compact_svg(&g, &names(), &DARK, "");
        // Axis 0–3.5× over 366 px; the 63× bar ends 16 px past it, cut once.
        assert!(svg.contains(">3.5×</text>") && !svg.contains(">4.0×</text>"));
        assert!(svg.contains("width=\"324.2\" height=\"6.0\" fill=\"#4493f8\""));
        assert!(svg.contains("width=\"382.0\" height=\"6.0\" fill=\"#4493f8\""));
        assert!(svg.contains("width=\"82.6\" height=\"6.0\" fill=\"url(#slower)\""));
        assert_eq!(svg.matches("stroke-width=\"2.5\"").count(), 1);
        assert!(svg.contains(
            "orange, hatched bars = oneyaml slower; bars longer than 3.5× are drawn broken"
        ));
        // One parity line per group.
        assert_eq!(svg.matches("stroke-dasharray").count(), 2);
    }

    #[test]
    fn empty_chart_is_well_formed() {
        let compact = compact_svg(&[], &[], &LIGHT, "");
        assert!(compact.starts_with("<svg") && compact.ends_with("</svg>\n"));
        let svg = svg(&[], &DARK, "");
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>\n"));
    }
}
