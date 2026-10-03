//! Exporting what's on screen: a chart panel as PNG, the dashboard as PDF
//! (through the browser's print dialog, so no headless browser is needed).

/// File-name-safe version of a title.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub fn slug(title: &str) -> String {
    let lowered: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let joined = lowered
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if joined.is_empty() {
        "panel".into()
    } else {
        joined
    }
}

#[cfg(feature = "hydrate")]
pub use browser::{png, print};

#[cfg(feature = "hydrate")]
mod browser {
    use leptos::prelude::*;
    use wasm_bindgen::JsCast;
    use web_sys::{CanvasRenderingContext2d, HtmlAnchorElement, HtmlCanvasElement, HtmlElement};

    fn css(element: &web_sys::Element, property: &str) -> String {
        web_sys::window()
            .and_then(|w| w.get_computed_style(element).ok().flatten())
            .and_then(|s| s.get_property_value(property).ok())
            .unwrap_or_default()
    }

    /// Saves a chart panel as PNG: its title, the chart, and the legend's labels.
    pub fn png(panel: &HtmlElement, title: &str) -> Result<(), String> {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or("no document")?;
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        let chart: HtmlCanvasElement = panel
            .query_selector(".uplot canvas")
            .ok()
            .flatten()
            .ok_or("this panel has no chart to export")?
            .dyn_into()
            .map_err(|_| "not a canvas")?;

        let legend: Vec<(String, String)> = panel
            .query_selector_all(".u-legend .u-series")
            .map(|list| {
                (0..list.length())
                    .filter_map(|i| list.item(i)?.dyn_into::<web_sys::Element>().ok())
                    .filter_map(|row| {
                        let label = row
                            .query_selector(".u-label")
                            .ok()
                            .flatten()?
                            .text_content()?;
                        let marker = row.query_selector(".u-marker").ok().flatten()?;
                        let color = css(&marker, "border-color");
                        (!label.trim().is_empty() && label.trim() != "Time")
                            .then_some((label, color))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let pad = 12.0 * dpr;
        let header = 28.0 * dpr;
        let footer = if legend.is_empty() { 0.0 } else { 24.0 * dpr };
        let (w, h) = (
            chart.width() as f64 + 2.0 * pad,
            chart.height() as f64 + header + footer + pad,
        );
        let out: HtmlCanvasElement = document
            .create_element("canvas")
            .map_err(|_| "canvas")?
            .dyn_into()
            .map_err(|_| "canvas")?;
        out.set_width(w as u32);
        out.set_height(h as u32);
        let ctx: CanvasRenderingContext2d = out
            .get_context("2d")
            .ok()
            .flatten()
            .ok_or("no 2d context")?
            .dyn_into()
            .map_err(|_| "no 2d context")?;

        let root = document.document_element().ok_or("no root")?;
        ctx.set_fill_style_str(&css(&root, "--panel"));
        ctx.fill_rect(0.0, 0.0, w, h);
        ctx.set_fill_style_str(&css(&root, "--text"));
        ctx.set_font(&format!("500 {}px sans-serif", (14.0 * dpr).round()));
        let _ = ctx.fill_text(title, pad, pad + 10.0 * dpr);
        ctx.draw_image_with_html_canvas_element(&chart, pad, header)
            .map_err(|_| "could not copy the chart")?;

        let mut x = pad;
        let y = header + chart.height() as f64 + 16.0 * dpr;
        ctx.set_font(&format!("{}px sans-serif", (12.0 * dpr).round()));
        for (label, color) in &legend {
            ctx.set_fill_style_str(color);
            ctx.fill_rect(x, y - 9.0 * dpr, 10.0 * dpr, 10.0 * dpr);
            ctx.set_fill_style_str(&css(&root, "--text-muted"));
            let _ = ctx.fill_text(label, x + 14.0 * dpr, y);
            x += ctx.measure_text(label).map_or(80.0, |m| m.width()) + 30.0 * dpr;
        }

        let url = out
            .to_data_url_with_type("image/png")
            .map_err(|_| "could not encode PNG")?;
        let a: HtmlAnchorElement = document
            .create_element("a")
            .map_err(|_| "anchor")?
            .dyn_into()
            .map_err(|_| "anchor")?;
        a.set_href(&url);
        a.set_download(&format!("{}.png", super::slug(title)));
        a.click();
        Ok(())
    }

    /// Prints the dashboard: light theme, page-width layout, then the
    /// browser's print dialog (where "Save as PDF" lives), then back.
    pub fn print(dark: RwSignal<bool>) {
        let Some(root) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        else {
            return;
        };
        let was_dark = dark.get_untracked();
        let _ = root.class_list().add_1("printing");
        dark.set(false);
        // Charts redraw for the theme and re-fit to the page width over the
        // next frames; print once they have.
        request_animation_frame(move || {
            request_animation_frame(move || {
                set_timeout(
                    move || {
                        if let Some(w) = web_sys::window() {
                            let _ = w.print();
                        }
                        let _ = root.class_list().remove_1("printing");
                        dark.set(was_dark);
                    },
                    std::time::Duration::from_millis(150),
                );
            });
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs() {
        assert_eq!(
            super::slug("Change lead time, weekly median"),
            "change-lead-time-weekly-median"
        );
        assert_eq!(super::slug("!!!"), "panel");
    }
}
