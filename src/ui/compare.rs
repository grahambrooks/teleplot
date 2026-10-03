//! The Compare panel (BubbleUp): what sets a drag-selected box of documents
//! apart from the rest of the panel's documents.

use leptos::prelude::*;

use super::api::{self, encode};
use super::format;
use crate::model::Panel;
use crate::model::api::{CompareRequest, Comparison, DataRequest, FieldDiff, Selection};

/// Below this many selected documents, differences are noise more often than not.
const FEW: u64 = 10;

#[component]
pub fn ComparePanel(
    dashboard: String,
    panel: Panel,
    selection: Selection,
    request: Signal<DataRequest>,
    /// The dashboard's small-sample threshold, if it has one.
    min_samples: Option<u32>,
    on_close: Callback<()>,
) -> impl IntoView {
    let title = panel.title.clone().unwrap_or_else(|| panel.id.clone());
    let url = format!(
        "/api/dashboards/{}/panels/{}/compare",
        encode(&dashboard),
        encode(&panel.id)
    );
    let comparison = LocalResource::new(move || {
        let body = CompareRequest {
            request: request.get(),
            selection: selection.clone(),
        };
        let url = url.clone();
        async move { api::post::<_, Comparison>(&url, &body).await }
    });
    let few = min_samples.map_or(FEW, |m| FEW.max(m as u64));
    let body = move || match comparison.get() {
        None => view! { <div class="loading"></div> }.into_any(),
        Some(Err(e)) => view! { <div class="banner error">{e}</div> }.into_any(),
        Some(Ok(c)) => result(&c, few),
    };
    let heading = title.clone();
    view! {
        <div class="modal-backdrop" on:click=move |_| on_close.run(())>
            <div class="modal compare" role="dialog" aria-label=format!("Compare: {heading}")
                on:click=|e| e.stop_propagation()>
                <header class="modal-header">
                    <strong>{format!("Compare: {title}")}</strong>
                    <span class="spacer"></span>
                    <button class="close" title="Close" on:click=move |_| on_close.run(())>"×"</button>
                </header>
                <div class="modal-body">
                    <Suspense fallback=|| view! { <div class="loading"></div> }>{body}</Suspense>
                </div>
            </div>
        </div>
    }
}

fn percent(share: f64) -> String {
    format!("{:.0}%", share * 100.0)
}

fn result(c: &Comparison, few: u64) -> AnyView {
    if c.selected == 0 {
        return view! { <p class="muted">"Nothing in the selection. Drag a box over some points."</p> }.into_any();
    }
    let summary = format!(
        "{} selected of {} ({})",
        format::plain(c.selected as f64),
        format::plain(c.baseline as f64),
        percent(c.selected as f64 / c.baseline.max(1) as f64)
    );
    let caution = (c.selected < few).then(|| view! {
        <div class="banner warning">{format!("Only {} documents selected: differences this small are often chance.", c.selected)}</div>
    });
    // Fields whose spread barely changes say nothing; show the rest.
    let fields: Vec<_> = c
        .fields
        .iter()
        .filter(|f| f.score >= 0.05)
        .take(8)
        .map(field_view)
        .collect();
    let measures: Vec<_> = c
        .measures
        .iter()
        .take(8)
        .map(|m| {
            let ratio = m.selected_median / m.baseline_median;
            let change = if ratio >= 1.0 {
                format!("{ratio:.1}× higher")
            } else {
                format!("{:.1}× lower", 1.0 / ratio.max(1e-9))
            };
            view! {
                <tr>
                    <td>{m.field.clone()}</td>
                    <td class="num">{format::grouped(m.selected_median, 1)}</td>
                    <td class="num">{format::grouped(m.baseline_median, 1)}</td>
                    <td class="num">{change}</td>
                </tr>
            }
        })
        .collect();
    let skipped = (!c.skipped.is_empty()).then(|| {
        view! { <p class="muted small">"Not compared: " {c.skipped.join("; ")}</p> }
    });
    view! {
        <p><strong>{summary}</strong>" against everything else in the panel, under the same filters."</p>
        {caution}
        <h3 class="compare-heading">"What sets them apart"</h3>
        {if fields.is_empty() {
            view! { <p class="muted">"No field's values are spread very differently in the selection."</p> }.into_any()
        } else {
            view! { <div class="compare-fields">{fields}</div> }.into_any()
        }}
        {(!measures.is_empty()).then(|| view! {
            <h3 class="compare-heading">"Medians"</h3>
            <table class="data-table">
                <thead><tr><th>"Field"</th><th class="num">"Selected"</th><th class="num">"Overall"</th><th class="num">"Change"</th></tr></thead>
                <tbody>{measures}</tbody>
            </table>
        })}
        {skipped}
        <p class="muted small">"Fields are ranked by how differently their values are spread: half the summed differences in share, from 0% (the same) to 100% (nothing in common)."</p>
    }
    .into_any()
}

fn field_view(f: &FieldDiff) -> impl IntoView {
    let values: Vec<_> = f
        .values
        .iter()
        .map(|v| {
            let selected = format!("width:{:.1}%", v.selected_share * 100.0);
            let baseline = format!("width:{:.1}%", v.baseline_share * 100.0);
            let text = format!(
                "{} vs {}",
                percent(v.selected_share),
                percent(v.baseline_share)
            );
            let title = format!(
                "{} of the selection ({}), {} overall ({})",
                percent(v.selected_share),
                v.selected,
                percent(v.baseline_share),
                v.baseline
            );
            view! {
                <div class="compare-value" title=title>
                    <span class="compare-label">{v.value.clone()}</span>
                    <span class="compare-bars">
                        <span class="bar selected" style=selected></span>
                        <span class="bar baseline" style=baseline></span>
                    </span>
                    <span class="muted small">{text}</span>
                </div>
            }
        })
        .collect();
    view! {
        <div class="compare-field">
            <div class="compare-field-name">
                <strong>{f.field.clone()}</strong>
                <span class="muted small">{format!("{} different", percent(f.score))}</span>
            </div>
            {values}
        </div>
    }
}
