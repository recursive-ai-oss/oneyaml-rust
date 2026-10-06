//! A dependency-free horizontal bar chart in SVG.
//!
//! `bench` is unpublished, but a plotting stack (plotters and
//! its font/raster dependencies) would be the heaviest dependency in the
//! workspace for one fixed chart shape. This writes the SVG text directly:
//! deterministic (byte-stable output, fixed-precision coordinates), and
//! honest by construction (§3 of the documentation concept): linear,
//! zero-based bars, a labelled unit and direction, a marked parity line,
//! slower bars in their own colour and labelled "slower", no log scale.

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

/// The chart.
#[must_use]
pub fn svg(panels: &[Panel], theme: &Theme, description: &str) -> String {
    let rows: usize = panels.iter().map(|p| p.bars.len()).sum();
    let plot = WIDTH - LEFT - RIGHT;
    let body = panels.len() as f64 * (PANEL_HEAD + PANEL_GAP) + rows as f64 * ROW;
    let height = TOP + body + BOTTOM;
    let max = panels
        .iter()
        .flat_map(|p| p.bars.iter().filter_map(|b| b.value))
        .fold(0.0_f64, f64::max);
    let (axis_max, step) = axis(max);
    let x = |v: f64| LEFT + v.max(0.0) / axis_max * plot;
    let axis_y = TOP + body;
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
        "<text x=\"16\" y=\"43\" fill=\"{}\">competitor CPU instructions ÷ oneyaml's · dashed line = 1.0× (equal) · orange = oneyaml slower</text>",
        theme.muted
    );

    // Grid and tick labels.
    let ticks = (axis_max / step).round() as u32;
    for i in 0..=ticks {
        let v = f64::from(i) * step;
        let gx = x(v);
        let _ = writeln!(
            s,
            "<line x1=\"{gx:.1}\" y1=\"{TOP:.1}\" x2=\"{gx:.1}\" y2=\"{axis_y:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
            theme.grid
        );
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

    // The parity line, under the bars and their value labels.
    let px = x(1.0);
    let _ = writeln!(
        s,
        "<line x1=\"{px:.1}\" y1=\"{TOP:.1}\" x2=\"{px:.1}\" y2=\"{axis_y:.1}\" stroke=\"{}\" stroke-width=\"1.5\" stroke-dasharray=\"4 3\"/>",
        theme.text
    );

    // Panels.
    let mut y = TOP;
    for p in panels {
        let _ = writeln!(
            s,
            "<text x=\"16\" y=\"{:.1}\" fill=\"{}\" font-weight=\"600\">{}</text>",
            y + 17.0,
            theme.text,
            escape(&p.title)
        );
        y += PANEL_HEAD;
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
                    let colour = if v < 1.0 { theme.slower } else { theme.accent };
                    let _ = writeln!(
                        s,
                        "<rect x=\"{LEFT:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{BAR:.1}\" fill=\"{colour}\"/>",
                        y + 4.0,
                        x(v) - LEFT
                    );
                    (x(v), theme.text)
                }
                None => (LEFT, theme.muted),
            };
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
        y += PANEL_GAP;
    }

    // The zero axis, on top of the bars.
    let _ = writeln!(
        s,
        "<line x1=\"{LEFT:.1}\" y1=\"{TOP:.1}\" x2=\"{LEFT:.1}\" y2=\"{axis_y:.1}\" stroke=\"{}\" stroke-width=\"1\"/>",
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
        assert!(svg.contains("width=\"76.5\" height=\"14.0\" fill=\"#bc4c00\""));
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
    fn empty_chart_is_well_formed() {
        let svg = svg(&[], &DARK, "");
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>\n"));
    }
}
