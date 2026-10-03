//! The panel inspector: the table behind a panel, and the Elasticsearch
//! searches that made it (Grafana's panel inspector, Kibana's Inspect).

use leptos::prelude::*;

use super::api::{self, encode};
use crate::model::api::{DataRequest, InspectedRequest, Inspection};
use crate::model::table::{Table, layer_table};
use crate::model::{Chart, Encoding, Panel, PanelContent, PanelData};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Data,
    Requests,
}

/// A date cell as ISO 8601 (UTC): unambiguous in a table and in CSV.
pub fn iso(ms: f64) -> String {
    #[cfg(feature = "hydrate")]
    {
        js_sys::Date::new(&ms.into()).to_iso_string().into()
    }
    #[cfg(not(feature = "hydrate"))]
    {
        jiff::Timestamp::from_millisecond(ms as i64)
            .map(|t| t.to_string())
            .unwrap_or_default()
    }
}

/// Each layer's table, with its file name. One layer: `<title>.csv`.
pub fn tables(chart: &Chart, data: &PanelData, title: &str) -> Vec<(String, Table)> {
    let several = data.layers.len() > 1;
    chart
        .layers
        .iter()
        .zip(&data.layers)
        .filter(|(_, d)| d.error.is_none())
        .map(|(layer, d)| {
            let name = if several {
                format!(
                    "{}-{}.csv",
                    super::export::slug(title),
                    super::export::slug(&layer.id)
                )
            } else {
                format!("{}.csv", super::export::slug(title))
            };
            (name, layer_table(layer, d))
        })
        .collect()
}

/// Downloads a panel's data as CSV, one file per data layer. Reference
/// lines are left out (the inspector has them): one file is the usual case.
pub fn download_csv(chart: &Chart, data: &PanelData, title: &str) {
    let mut chart = chart.clone();
    let mut data = data.clone();
    let keep: Vec<bool> = chart
        .layers
        .iter()
        .map(|l| !matches!(l.encoding, Encoding::ReferenceLines { .. }))
        .collect();
    let mut flags = keep.iter();
    chart.layers.retain(|_| *flags.next().unwrap_or(&true));
    let mut flags = keep.iter();
    data.layers.retain(|_| *flags.next().unwrap_or(&true));
    for (name, table) in tables(&chart, &data, title) {
        let text = crate::model::table::csv(&table, &iso);
        #[cfg(feature = "hydrate")]
        if let Err(e) = super::export::download(&name, "text/csv", &text) {
            leptos::logging::warn!("CSV export: {e}");
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = (name, text);
    }
}

#[component]
pub fn Inspector(
    dashboard: String,
    panel: Panel,
    request: Signal<DataRequest>,
    on_close: Callback<()>,
) -> impl IntoView {
    let PanelContent::Chart(chart) = panel.content.clone() else {
        return ().into_any();
    };
    let title = panel.title.clone().unwrap_or_else(|| panel.id.clone());
    let url = format!(
        "/api/dashboards/{}/panels/{}/inspect",
        encode(&dashboard),
        encode(&panel.id)
    );
    let inspection = LocalResource::new(move || {
        let body = request.get();
        let url = url.clone();
        async move { api::post::<_, Inspection>(&url, &body).await }
    });
    let tab = RwSignal::new(Tab::Data);
    let chart = StoredValue::new(chart);
    let heading = title.clone();
    let file_title = StoredValue::new(title);

    let body = move || match inspection.get() {
        None => view! { <div class="loading"></div> }.into_any(),
        Some(Err(e)) => view! { <div class="banner error">{e}</div> }.into_any(),
        Some(Ok(i)) => match tab.get() {
            Tab::Data => data_view(&chart.get_value(), &i.data, &file_title.get_value()),
            Tab::Requests => requests_view(&i.requests),
        },
    };
    let tab_button = move |which: Tab, label: &'static str| {
        view! {
            <button class="tab" class:active=move || tab.get() == which
                on:click=move |_| tab.set(which)>{label}</button>
        }
    };
    view! {
        <div class="modal-backdrop" on:click=move |_| on_close.run(())>
            <div class="modal inspector" role="dialog" aria-label=format!("Inspect {heading}")
                on:click=|e| e.stop_propagation()>
                <header class="modal-header">
                    <strong>{format!("Inspect: {heading}")}</strong>
                    <nav class="tabs">{tab_button(Tab::Data, "Data")}{tab_button(Tab::Requests, "Requests")}</nav>
                    <span class="spacer"></span>
                    <button class="close" title="Close" on:click=move |_| on_close.run(())>"×"</button>
                </header>
                // Its own boundary: loading here must not suspend the dashboard behind it.
                <div class="modal-body">
                    <Suspense fallback=|| view! { <div class="loading"></div> }>{body}</Suspense>
                </div>
            </div>
        </div>
    }
    .into_any()
}

fn data_view(chart: &Chart, data: &PanelData, title: &str) -> AnyView {
    let errors: Vec<_> = chart
        .layers
        .iter()
        .zip(&data.layers)
        .filter_map(|(l, d)| {
            let e = d.error.clone()?;
            Some(view! { <div class="banner error">{format!("Layer {}: {e}", l.id)}</div> })
        })
        .collect();
    let warnings: Vec<_> = data
        .warnings
        .iter()
        .map(|w| view! { <div class="banner warning">{w.clone()}</div> })
        .collect();
    let several = data.layers.len() > 1;
    let layers: Vec<_> = tables(chart, data, title)
        .into_iter()
        .zip(chart.layers.iter())
        .map(|((name, table), layer)| {
            let csv = crate::model::table::csv(&table, &iso);
            let head: Vec<_> = table
                .columns
                .iter()
                .map(|c| view! { <th>{c.label.clone()}</th> })
                .collect();
            let rows: Vec<_> = table
                .rows
                .iter()
                .map(|row| {
                    let cells: Vec<_> = table
                        .columns
                        .iter()
                        .zip(row)
                        .map(|(c, cell)| view! { <td>{c.text(cell, &iso)}</td> })
                        .collect();
                    view! { <tr>{cells}</tr> }
                })
                .collect();
            let count = format!(
                "{} row{}",
                table.rows.len(),
                if table.rows.len() == 1 { "" } else { "s" }
            );
            view! {
                <section class="inspect-layer">
                    <div class="inspect-layer-head">
                        {several.then(|| view! { <strong>{format!("Layer {}", layer.id)}</strong> })}
                        <span class="muted">{count}</span>
                        <span class="spacer"></span>
                        <button class="panel-action visible" title="Download as CSV" on:click=move |_| {
                            #[cfg(feature = "hydrate")]
                            if let Err(e) = super::export::download(&name, "text/csv", &csv) {
                                leptos::logging::warn!("CSV export: {e}");
                            }
                            #[cfg(not(feature = "hydrate"))]
                            let _ = (&name, &csv);
                        }>"Download CSV"</button>
                    </div>
                    <div class="inspect-table">
                        <table class="data-table"><thead><tr>{head}</tr></thead><tbody>{rows}</tbody></table>
                    </div>
                </section>
            }
        })
        .collect();
    view! { {errors}{warnings}{layers} }.into_any()
}

fn requests_view(requests: &[InspectedRequest]) -> AnyView {
    let took: u64 = requests.iter().filter_map(|r| r.took_ms).sum();
    let summary = format!(
        "{} search{} · {took} ms in Elasticsearch",
        requests.len(),
        if requests.len() == 1 { "" } else { "es" }
    );
    let items: Vec<_> = requests
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut facts = vec![r.index.clone()];
            if let Some(ms) = r.took_ms {
                facts.push(format!("{ms} ms"));
            }
            if let Some(n) = r.hits {
                facts.push(format!("{n} documents matched"));
            }
            let pretty = |v: &serde_json::Value| serde_json::to_string_pretty(v).unwrap_or_default();
            let request = format!("POST /{}/_search\n{}", r.index, pretty(&r.request));
            let response = r.response.as_ref().map(|v| {
                let text = pretty(v);
                view! { <details><summary>"Response"</summary><pre class="json">{text}</pre></details> }
            });
            let error = r
                .error
                .clone()
                .map(|e| view! { <div class="banner error">{e}</div> });
            view! {
                <details class="inspect-request" open=i == 0>
                    <summary><strong>{r.description.clone()}</strong>" "<span class="muted">{facts.join(" · ")}</span></summary>
                    {error}
                    <div class="muted small">"Request (paste into Kibana Dev Tools)"</div>
                    <pre class="json">{request}</pre>
                    {response}
                </details>
            }
        })
        .collect();
    view! { <div class="muted inspect-summary">{summary}</div>{items} }.into_any()
}
