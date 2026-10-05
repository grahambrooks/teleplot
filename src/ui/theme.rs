//! Colours. Series take a validated categorical palette in a fixed order
//! (stepped separately for each theme), a one-hue ramp when their order means
//! something, or a reserved status colour; axis and grid colours come from the
//! page's CSS variables, so charts follow the theme.
//!
//! The palettes pass the six checks of the dataviz method (lightness band,
//! chroma floor, CVD separation of neighbours, normal-vision floor, contrast)
//! against the panel surfaces `#ffffff` and `#181b1f`. Grafana's classic
//! palette, used before, failed three of them in the dark theme: violet and
//! blue collapse under protanopia (ΔE 5.0) and red sits 12.6 from orange.
//! Re-run the validator before changing a value or the order.

/// Categorical slots for the light theme, in their fixed order.
pub const CATEGORICAL_LIGHT: [&str; 8] = [
    "#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#4a3aa7", "#e34948",
];
/// The same hues stepped for the dark theme.
pub const CATEGORICAL_DARK: [&str; 8] = [
    "#3987e5", "#d95926", "#199e70", "#c98500", "#d55181", "#008300", "#9085e9", "#e66767",
];

/// The current theme's categorical palette, assigned to series by position.
pub fn palette() -> &'static [&'static str] {
    categorical(is_dark())
}

pub fn categorical(dark: bool) -> &'static [&'static str] {
    if dark {
        &CATEGORICAL_DARK
    } else {
        &CATEGORICAL_LIGHT
    }
}

/// Whether the page shows the dark theme (the server renders it dark).
pub fn is_dark() -> bool {
    #[cfg(feature = "hydrate")]
    {
        let theme = (|| {
            web_sys::window()?
                .document()?
                .document_element()?
                .get_attribute("data-theme")
        })();
        theme.as_deref() != Some("light")
    }
    #[cfg(not(feature = "hydrate"))]
    {
        true
    }
}

/// Status colours: reserved for state (a CI result, an alert's severity) and
/// never handed to an ordinary series. The same in both themes.
pub fn status(name: &str) -> Option<&'static str> {
    Some(match name {
        "good" => "#0ca30c",
        "warning" => "#fab219",
        "serious" => "#ec835a",
        "critical" => "#d03b3b",
        // Context rather than data: automation beside people, "other".
        "neutral" => "#898781",
        _ => return None,
    })
}

/// Ordinal steps of the blue ramp: ordered classes (size bands, age bands,
/// tiers). Light to dark; the end nearest the surface still clears 2:1.
const ORDINAL: [&str; 8] = [
    "#86b6ef", "#6da7ec", "#5598e7", "#3987e5", "#2a78d6", "#256abf", "#1c5cab", "#184f95",
];

/// The colour of class `i` of `n` ordered classes, spread across the ramp.
/// The first class recedes towards the panel: light on the light theme,
/// dark on the dark one.
pub fn ordinal(i: usize, n: usize, dark: bool) -> &'static str {
    let last = ORDINAL.len() - 1;
    let step = if n <= 1 {
        last
    } else {
        (i.min(n - 1) * last + (n - 1) / 2) / (n - 1)
    };
    ORDINAL[if dark { last - step } else { step }]
}

#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub struct ChartColors {
    pub text: String,
    pub grid: String,
    /// The panel background, behind labels drawn over data.
    pub panel: String,
    /// Annotations without a colour of their own.
    pub warning: String,
}

/// The current theme's chart colours.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub fn chart_colors() -> ChartColors {
    ChartColors {
        text: css_var("--text-muted", "rgba(204,204,220,0.65)"),
        grid: css_var("--border", "rgba(204,204,220,0.12)"),
        warning: css_var("--warning", "#ff9830"),
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
