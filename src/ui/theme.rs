//! Colours. Series use Grafana's classic palette, in order; axis and grid
//! colours come from the page's CSS variables, so charts follow the theme.

/// Grafana's "Classic palette", assigned to series by position.
pub const PALETTE: [&str; 12] = [
    "#73BF69", "#F2CC0C", "#8AB8FF", "#FF780A", "#F2495C", "#5794F2", "#B877D9", "#705DA0",
    "#37872D", "#FADE2A", "#447EBC", "#C15C17",
];

pub fn palette() -> &'static [&'static str] {
    &PALETTE
}

#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub struct ChartColors {
    pub text: String,
    pub grid: String,
    /// The panel background, behind labels drawn over data.
    pub panel: String,
}

/// The current theme's chart colours.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub fn chart_colors() -> ChartColors {
    ChartColors {
        text: css_var("--text-muted", "rgba(204,204,220,0.65)"),
        grid: css_var("--border", "rgba(204,204,220,0.12)"),
        panel: css_var("--panel", "#181b1f"),
    }
}

#[cfg(feature = "hydrate")]
fn css_var(name: &str, fallback: &str) -> String {
    let value = (|| {
        let window = web_sys::window()?;
        let root = window.document()?.document_element()?;
        let style = window.get_computed_style(&root).ok()??;
        let v = style.get_property_value(name).ok()?;
        let v = v.trim();
        (!v.is_empty()).then(|| v.to_string())
    })();
    value.unwrap_or_else(|| fallback.to_string())
}

#[cfg(not(feature = "hydrate"))]
#[allow(dead_code)]
fn css_var(_name: &str, fallback: &str) -> String {
    fallback.to_string()
}

/// A benchmark band's colour: its own, or green → blue → orange → red by position.
pub fn threshold_color(t: &crate::model::Threshold, index: usize) -> String {
    const DEFAULT: [&str; 4] = ["#73BF69", "#5794F2", "#FF9830", "#F2495C"];
    t.color
        .clone()
        .unwrap_or_else(|| DEFAULT[index.min(DEFAULT.len() - 1)].to_string())
}

/// One-hue sequential ramp (blue, light → dark) for magnitudes: heatmap cells.
const SEQUENTIAL: [&str; 13] = [
    "#cde2fb", "#b7d3f6", "#9ec5f4", "#86b6ef", "#6da7ec", "#5598e7", "#3987e5", "#2a78d6",
    "#256abf", "#1c5cab", "#184f95", "#104281", "#0d366b",
];

/// The ramp's colour for `t` in 0–1. Small values recede towards the panel:
/// light on the light theme, dark on the dark one.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub fn sequential(t: f64, dark: bool) -> &'static str {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let t = if dark { 1.0 - t } else { t };
    SEQUENTIAL[(t * (SEQUENTIAL.len() - 1) as f64).round() as usize]
}
