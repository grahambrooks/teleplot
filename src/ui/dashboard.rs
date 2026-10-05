//! The dashboard list and the dashboard page.
//!
//! The page's state lives in the URL, so every view can be bookmarked and
//! shared: `?from=now-90d&to=now&q=team:checkout&ql=kql&var-<control>=a,b`.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos_router::NavigateOptions;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use leptos_router::params::ParamsMap;

use super::api::{self, encode};
use super::inspector::Inspector;
use super::panels::{PanelContext, PanelView};
use crate::model::api::{
    AdHocFilter, Catalog, ControlValues, DataRequest, DataResponse, FieldList,
};
use crate::model::{Control, Dashboard, Query, QueryLanguage};

#[component]
pub fn DashboardList() -> impl IntoView {
    let catalog = LocalResource::new(|| api::get::<Catalog>("/api/dashboards"));
    view! {
        <div class="page">
            <Suspense fallback=|| view! { <div class="loading"></div> }>
                {move || catalog.get().map(|result| match result {
                    Err(e) => view! { <div class="banner error">{e}</div> }.into_any(),
                    Ok(c) => view! {
                        {c.problem.map(|p| view! { <div class="banner warning">"Queries cannot run: " {p}</div> })}
                        {c.errors.into_iter().map(|e| view! { <div class="banner error">{e}</div> }).collect_view()}
                        <h1 class="page-title">"Dashboards"</h1>
                        {if c.dashboards.is_empty() {
                            view! { <p class="muted">"No dashboards found. Put Kibana exports (.ndjson) or Teleplot dashboards (.yaml) in the dashboards directory."</p> }.into_any()
                        } else {
                            view! {
                                <ul class="dashboard-list">
                                    {c.dashboards.into_iter().map(|d| view! {
                                        <li>
                                            <A href=format!("/d/{}", encode(&d.id))>
                                                <span class="dashboard-name">{d.title}</span>
                                                <span class="muted">{d.description.unwrap_or_default()}</span>
                                                <span class="muted small">{format!("{} panels", d.panels)}</span>
                                            </A>
                                        </li>
                                    }).collect_view()}
                                </ul>
                            }.into_any()
                        }}
                    }.into_any(),
                })}
            </Suspense>
        </div>
    }
}

/// Builds a URL for this dashboard with some query parameters changed
/// (`None` removes one).
fn url_with(id: &str, current: &ParamsMap, changes: &[(String, Option<String>)]) -> String {
    let mut params: Vec<(String, String)> = current
        .clone()
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .filter(|(k, _)| !changes.iter().any(|(c, _)| c == k))
        .collect();
    for (k, v) in changes {
        if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
            params.push((k.to_string(), v.clone()));
        }
    }
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    if query.is_empty() {
        format!("/d/{}", encode(id))
    } else {
        format!("/d/{}?{}", encode(id), query.join("&"))
    }
}

fn request_from(params: &ParamsMap, dashboard: &Dashboard) -> DataRequest {
    let query = params
        .get("q")
        .filter(|q| !q.trim().is_empty())
        .map(|text| Query {
            language: if params.get("ql").as_deref() == Some("lucene") {
                QueryLanguage::Lucene
            } else {
                QueryLanguage::Kql
            },
            text,
        });
    let selections: HashMap<String, Vec<String>> = dashboard
        .controls
        .iter()
        .filter_map(|c| {
            let values: Vec<String> = params
                .get(&format!("var-{}", c.id))?
                .split(',')
                .filter(|v| !v.is_empty())
                .map(String::from)
                .collect();
            Some((c.id.clone(), values))
        })
        .collect();
    let time = dashboard.time.clone();
    DataRequest {
        from: params
            .get("from")
            .or_else(|| time.as_ref().map(|t| t.from.clone()))
            .or_else(|| Some("now-90d".into())),
        to: params
            .get("to")
            .or_else(|| time.as_ref().map(|t| t.to.clone()))
            .or_else(|| Some("now".into())),
        query,
        selections,
        // Ad-hoc filter pills, as JSON: `f=[{"field":"team","values":["checkout"]}]`.
        filters: params
            .get("f")
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default(),
    }
}

#[component]
pub fn DashboardPage() -> impl IntoView {
    let params = use_params_map();
    let id = Memo::new(move |_| params.with(|p| p.get("id")).unwrap_or_default());
    let dashboard = LocalResource::new(move || {
        let id = id.get();
        async move { api::get::<Dashboard>(&format!("/api/dashboards/{}", encode(&id))).await }
    });
    view! {
        <Suspense fallback=|| view! { <div class="loading"></div> }>
            {move || dashboard.get().map(|result| match result {
                Err(e) => view! { <div class="page"><div class="banner error">{e}</div></div> }.into_any(),
                Ok(d) => view! { <DashboardView dashboard=d/> }.into_any(),
            })}
        </Suspense>
    }
}

#[component]
fn DashboardView(dashboard: Dashboard) -> impl IntoView {
    let query = use_query_map();
    let navigate = use_navigate();
    let dashboard = StoredValue::new(dashboard);
    let id = dashboard.with_value(|d| d.id.clone());
    let request = Memo::new(move |_| query.with(|q| dashboard.with_value(|d| request_from(q, d))));
    let refresh = RwSignal::new(0u32);

    let data = LocalResource::new(move || {
        let body = request.get();
        let _ = refresh.get();
        let id = dashboard.with_value(|d| d.id.clone());
        async move {
            api::post::<_, DataResponse>(&format!("/api/dashboards/{}/data", encode(&id)), &body)
                .await
        }
    });

    let go = {
        let id = id.clone();
        let navigate = navigate.clone();
        move |changes: Vec<(String, Option<String>)>| {
            let url = query.with_untracked(|q| url_with(&id, q, &changes));
            navigate(&url, NavigateOptions::default());
        }
    };
    let set_time = {
        let go = go.clone();
        Callback::new(move |(from, to): (String, String)| {
            go(vec![("from".into(), Some(from)), ("to".into(), Some(to))])
        })
    };
    let set_query = {
        let go = go.clone();
        Callback::new(move |(text, lucene): (String, bool)| {
            go(vec![
                ("q".into(), Some(text)),
                ("ql".into(), lucene.then(|| "lucene".to_string())),
            ])
        })
    };
    // With chained controls, a change clears the controls after it: their
    // choices were made under the old selection (team → repo → service).
    let set_control = {
        let go = go.clone();
        let (ids, chained) = dashboard.with_value(|d| {
            (
                d.controls.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
                d.chain_controls,
            )
        });
        Callback::new(move |(control, values): (String, Vec<String>)| {
            let mut changes = vec![(format!("var-{control}"), Some(values.join(",")))];
            if chained && let Some(i) = ids.iter().position(|id| *id == control) {
                changes.extend(ids[i + 1..].iter().map(|id| (format!("var-{id}"), None)));
            }
            go(changes)
        })
    };
    let set_filters = {
        let go = go.clone();
        Callback::new(move |filters: Vec<AdHocFilter>| {
            let encoded =
                (!filters.is_empty()).then(|| serde_json::to_string(&filters).unwrap_or_default());
            go(vec![("f".into(), encoded)])
        })
    };
    let on_zoom = Callback::new(move |(from, to): (f64, f64)| {
        set_time.run((super::clock::iso(from), super::clock::iso(to)));
    });

    let time_zone = Signal::derive(move || {
        data.get()
            .and_then(|r| r.ok())
            .map(|r| r.time_zone)
            .unwrap_or_else(|| "UTC".into())
    });
    let dark = use_context::<RwSignal<bool>>()
        .map(Signal::from)
        .unwrap_or_else(|| Signal::stored(true));
    // Clicking a team (or any value of a field with a control) selects it.
    let controls_for_drill = dashboard.with_value(|d| d.controls.clone());
    let drill_fields: Vec<String> = controls_for_drill.iter().map(|c| c.field.clone()).collect();
    let drill = Callback::new(move |(field, value): (String, String)| {
        if let Some(c) = controls_for_drill.iter().find(|c| c.field == field) {
            set_control.run((c.id.clone(), vec![value]));
        }
    });
    let min_samples = dashboard.with_value(|d| d.min_samples);
    let inspecting = RwSignal::new(None::<String>);
    let comparing = RwSignal::new(None::<(String, crate::model::api::Selection)>);
    let ctx = PanelContext {
        inspect: Callback::new(move |panel: String| inspecting.set(Some(panel))),
        compare: Callback::new(move |selected| comparing.set(Some(selected))),
        comparing: None,
        time_zone,
        sync_key: id.clone(),
        on_zoom,
        dark,
        min_samples,
        drill_fields,
        drill,
    };

    let panels = dashboard.with_value(|d| d.panels.clone());
    // The query bar and filter pills are Elasticsearch queries: on a
    // dashboard whose charts all read insight they would filter nothing.
    let es_filters = panels.iter().any(|p| match &p.content {
        crate::model::PanelContent::Chart(c) => c.layers.iter().any(|l| l.insight.is_none()),
        _ => false,
    });
    let title = dashboard.with_value(|d| d.title.clone());
    let controls = dashboard.with_value(|d| d.controls.clone());
    let base_query = dashboard.with_value(|d| d.query.clone());
    let dashboard_filters =
        dashboard.with_value(|d| d.filters.iter().filter(|f| !f.disabled).count());

    // The header printed above the panels: what, when, and with which filters.
    let print_title = dashboard.with_value(|d| d.title.clone());
    let print_range = move || {
        let label = request.with(|r| {
            time_label(
                r.from.as_deref().unwrap_or(""),
                r.to.as_deref().unwrap_or(""),
            )
        });
        match data.get().and_then(|r| r.ok()) {
            Some(r) => {
                let zone = if r.time_zone == "UTC" {
                    String::new()
                } else {
                    format!(", buckets in {}", r.time_zone)
                };
                let range = format!(
                    "{} to {}{zone}",
                    iso_minutes(r.from_ms),
                    iso_minutes(r.to_ms)
                );
                // Quick ranges ("Last 90 days") say what they mean; custom ones are the range.
                if QUICK_RANGES.iter().any(|(l, _, _)| *l == label) {
                    format!("{label}: {range}")
                } else {
                    range
                }
            }
            None => label,
        }
    };
    let print_filters = move || {
        request.with(|r| {
            let mut parts = Vec::new();
            if let Some(q) = &r.query {
                parts.push(format!("Query: {}", q.text));
            }
            for (control, values) in &r.selections {
                if !values.is_empty() {
                    parts.push(format!("{control}: {}", values.join(", ")));
                }
            }
            parts.join(" · ")
        })
    };

    view! {
        <div class="dashboard">
            <div class="toolbar">
                <h1 class="dashboard-title">{title}</h1>
                <span class="spacer"></span>
                <TimePicker request=request.into() on_change=set_time/>
                <button class="btn" title="Refresh" on:click=move |_| refresh.update(|n| *n += 1)>"↻"</button>
                <button class="btn" title="Print, or save as PDF from the print dialog" on:click=move |_| {
                    #[cfg(feature = "hydrate")]
                    if let Some(dark) = use_context::<RwSignal<bool>>() {
                        super::export::print(dark);
                    }
                }>"Export PDF"</button>
            </div>
            <div class="print-only print-header">
                <strong>{print_title}</strong>
                <span>{print_range}</span>
                <span>{print_filters}</span>
            </div>
            <div class="toolbar">
                {es_filters.then(|| view! { <QueryBar request=request.into() on_submit=set_query/> })}
                {(!base_query.is_empty() || dashboard_filters > 0).then(|| {
                    let text = format!(
                        "Dashboard: {}{}",
                        if base_query.is_empty() { String::new() } else { base_query.text.clone() },
                        if dashboard_filters > 0 { format!(" + {dashboard_filters} filter(s)") } else { String::new() }
                    );
                    view! { <span class="chip muted" title="Applied to every panel">{text}</span> }
                })}
                {controls.into_iter().map(|c| view! {
                    <ControlSelect control=c dashboard=id.clone() request=request.into() on_change=set_control/>
                }).collect_view()}
                {es_filters.then(|| view! { <AdHocFilters dashboard=id.clone() request=request.into() on_change=set_filters/> })}
            </div>
            {move || data.get().and_then(|r| r.err()).map(|e| view! { <div class="banner error">{e}</div> })}
            <div class="grid">
                {panels.into_iter().map(|panel| {
                    let pid = panel.id.clone();
                    let panel_data = Signal::derive(move || {
                        data.get().and_then(|r| r.ok()).and_then(|r| r.panels.get(&pid).cloned())
                    });
                    view! { <PanelView panel data=panel_data ctx=ctx.clone()/> }
                }).collect_view()}
            </div>
            {move || {
                let panel = inspecting
                    .get()
                    .and_then(|pid| dashboard.with_value(|d| d.panels.iter().find(|p| p.id == pid).cloned()))?;
                let id = dashboard.with_value(|d| d.id.clone());
                Some(view! {
                    <Inspector dashboard=id panel request=request.into()
                        on_close=Callback::new(move |_| inspecting.set(None))/>
                })
            }}
            {move || {
                let (pid, selection) = comparing.get()?;
                let panel = dashboard.with_value(|d| d.panels.iter().find(|p| p.id == pid).cloned())?;
                let id = dashboard.with_value(|d| d.id.clone());
                Some(view! {
                    <super::compare::ComparePanel dashboard=id panel selection request=request.into()
                        min_samples=min_samples
                        on_close=Callback::new(move |_| comparing.set(None))/>
                })
            }}
        </div>
    }
}

const QUICK_RANGES: [(&str, &str, &str); 12] = [
    ("Last 24 hours", "now-24h", "now"),
    ("Last 7 days", "now-7d", "now"),
    ("Last 30 days", "now-30d", "now"),
    ("Last 90 days", "now-90d", "now"),
    ("Last 6 months", "now-6M", "now"),
    ("Last 1 year", "now-1y", "now"),
    ("This week", "now/w", "now/w"),
    ("This month", "now/M", "now/M"),
    ("This quarter", "now/M-2M/M", "now/M"),
    ("This year", "now/y", "now/y"),
    ("Last full month", "now-1M/M", "now-1M/M"),
    ("Last full quarter", "now-3M/M", "now-1M/M"),
];

fn time_label(from: &str, to: &str) -> String {
    QUICK_RANGES
        .iter()
        .find(|(_, f, t)| *f == from && *t == to)
        .map(|(label, _, _)| label.to_string())
        .unwrap_or_else(|| format!("{} → {}", short(from), short(to)))
}

fn iso_minutes(ms: i64) -> String {
    format!("{} UTC", super::clock::minutes(ms as f64))
}

/// ISO timestamps shown as dates and times; date math as typed.
fn short(s: &str) -> String {
    super::clock::parse_iso(s).map_or_else(|| s.to_string(), super::clock::minutes)
}

#[component]
fn TimePicker(
    request: Signal<DataRequest>,
    on_change: Callback<(String, String)>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let from = RwSignal::new(String::new());
    let to = RwSignal::new(String::new());
    let label = move || {
        request.with(|r| {
            time_label(
                r.from.as_deref().unwrap_or(""),
                r.to.as_deref().unwrap_or(""),
            )
        })
    };
    let toggle = move |_| {
        request.with_untracked(|r| {
            from.set(r.from.clone().unwrap_or_default());
            to.set(r.to.clone().unwrap_or_default());
        });
        open.update(|o| *o = !*o);
    };
    let apply = move |from: String, to: String| {
        open.set(false);
        on_change.run((from, to));
    };
    view! {
        <div class="time-picker">
            <button class="btn" on:click=toggle>"🕒 " {label}</button>
            <Show when=move || open.get()>
                <div class="popover">
                    <div class="quick-ranges">
                        {QUICK_RANGES.iter().map(|(label, f, t)| {
                            let (f, t) = (f.to_string(), t.to_string());
                            view! { <button class="link" on:click=move |_| apply(f.clone(), t.clone())>{*label}</button> }
                        }).collect_view()}
                    </div>
                    <div class="custom-range">
                        <label>"From"<input prop:value=move || from.get() on:input=move |ev| from.set(event_target_value(&ev))/></label>
                        <label>"To"<input prop:value=move || to.get() on:input=move |ev| to.set(event_target_value(&ev))/></label>
                        <button class="btn primary" on:click=move |_| apply(from.get_untracked(), to.get_untracked())>"Apply"</button>
                        <span class="muted small">"Date math like now-30d/d, or 2026-04-01"</span>
                    </div>
                </div>
            </Show>
        </div>
    }
}

#[component]
fn QueryBar(request: Signal<DataRequest>, on_submit: Callback<(String, bool)>) -> impl IntoView {
    let text = RwSignal::new(String::new());
    let lucene = RwSignal::new(false);
    Effect::new(move |_| {
        let q = request.with(|r| r.query.clone());
        text.set(q.as_ref().map(|q| q.text.clone()).unwrap_or_default());
        lucene.set(q.is_some_and(|q| q.language == QueryLanguage::Lucene));
    });
    let submit = move || on_submit.run((text.get_untracked(), lucene.get_untracked()));
    view! {
        <div class="query-bar">
            <input
                class="query-input"
                placeholder=move || if lucene.get() { "Lucene query" } else { "Filter with KQL, e.g. team : checkout and not result : failure" }
                prop:value=move || text.get()
                on:input=move |ev| text.set(event_target_value(&ev))
                on:keydown=move |ev| if ev.key() == "Enter" { submit() }
            />
            <button class="btn small" title="Query language" on:click=move |_| { lucene.update(|l| *l = !*l); submit(); }>
                {move || if lucene.get() { "Lucene" } else { "KQL" }}
            </button>
        </div>
    }
}

#[component]
fn ControlSelect(
    control: Control,
    dashboard: String,
    request: Signal<DataRequest>,
    on_change: Callback<(String, Vec<String>)>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let control = StoredValue::new(control);
    let selected = Memo::new(move |_| {
        let id = control.with_value(|c| c.id.clone());
        request.with(|r| r.selections.get(&id).cloned().unwrap_or_default())
    });
    let values = LocalResource::new(move || {
        let body = request.get();
        let url = control.with_value(|c| {
            format!(
                "/api/dashboards/{}/controls/{}",
                encode(&dashboard),
                encode(&c.id)
            )
        });
        let wanted = open.get();
        async move {
            if !wanted {
                return Ok(ControlValues::default());
            }
            api::post::<_, ControlValues>(&url, &body).await
        }
    });
    let toggle_value = move |value: String| {
        let mut chosen = selected.get_untracked();
        if let Some(i) = chosen.iter().position(|v| *v == value) {
            chosen.remove(i);
        } else {
            chosen.push(value);
        }
        on_change.run((control.with_value(|c| c.id.clone()), chosen));
    };
    let summary = move || match selected.get().as_slice() {
        [] => "All".to_string(),
        [one] => one.clone(),
        many => format!("{} selected", many.len()),
    };
    view! {
        <div class="control">
            <button class="btn" on:click=move |_| open.update(|o| *o = !*o)>
                <span class="muted">{control.with_value(|c| c.title.clone())}": "</span>{summary}
            </button>
            <Show when=move || open.get()>
                <div class="popover options">
                    <button class="link" on:click=move |_| on_change.run((control.with_value(|c| c.id.clone()), Vec::new()))>"Clear"</button>
                    <Suspense fallback=|| view! { <div class="loading small"></div> }>
                        {move || values.get().map(|r| match r {
                            Err(e) => view! { <div class="banner error">{e}</div> }.into_any(),
                            Ok(v) => v.values.into_iter().map(|(value, count)| {
                                let checked = { let value = value.clone(); move || selected.get().contains(&value) };
                                let pick = value.clone();
                                view! {
                                    <label class="option">
                                        <input type="checkbox" prop:checked=checked on:change=move |_| toggle_value(pick.clone())/>
                                        <span>{value}</span>
                                        <span class="muted small">{count}</span>
                                    </label>
                                }
                            }).collect_view().into_any(),
                        })}
                    </Suspense>
                </div>
            </Show>
        </div>
    }
}

/// The viewer's own filter pills: `field: value, …` chips (click to invert,
/// × to remove) and a picker to add one, with values suggested under the
/// other filters. A pill narrows only the panels whose data has its field.
#[component]
fn AdHocFilters(
    dashboard: String,
    request: Signal<DataRequest>,
    on_change: Callback<Vec<AdHocFilter>>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let field = RwSignal::new(String::new());
    let negate = RwSignal::new(false);
    let chosen = RwSignal::new(Vec::<String>::new());
    let dashboard = StoredValue::new(dashboard);
    let current = Memo::new(move |_| request.with(|r| r.filters.clone()));

    let fields = LocalResource::new(move || {
        let wanted = open.get();
        let url = dashboard.with_value(|d| format!("/api/dashboards/{}/fields", encode(d)));
        async move {
            if !wanted {
                return Ok(FieldList::default());
            }
            api::get::<FieldList>(&url).await
        }
    });
    let values = LocalResource::new(move || {
        let name = field.get();
        let body = request.get();
        let url = dashboard.with_value(|d| {
            format!(
                "/api/dashboards/{}/fields/{}/values",
                encode(d),
                encode(&name)
            )
        });
        async move {
            if name.is_empty() {
                return Ok(ControlValues::default());
            }
            api::post::<_, ControlValues>(&url, &body).await
        }
    });

    let remove = move |i: usize| {
        let mut all = current.get_untracked();
        all.remove(i);
        on_change.run(all);
    };
    let invert = move |i: usize| {
        let mut all = current.get_untracked();
        all[i].negate = !all[i].negate;
        on_change.run(all);
    };
    let add = move || {
        let values = chosen.get_untracked();
        let name = field.get_untracked();
        if name.is_empty() || values.is_empty() {
            return;
        }
        let mut all = current.get_untracked();
        all.push(AdHocFilter {
            field: name,
            values,
            negate: negate.get_untracked(),
        });
        open.set(false);
        field.set(String::new());
        chosen.set(Vec::new());
        negate.set(false);
        on_change.run(all);
    };
    let toggle_value = move |value: String| {
        chosen.update(|c| {
            if let Some(i) = c.iter().position(|v| *v == value) {
                c.remove(i);
            } else {
                c.push(value);
            }
        })
    };

    view! {
        <div class="adhoc">
            {move || current.get().into_iter().enumerate().map(|(i, f)| {
                let text = format!(
                    "{}{}: {}",
                    if f.negate { "NOT " } else { "" },
                    f.field,
                    f.values.join(", ")
                );
                view! {
                    <span class="pill" class:negated=f.negate>
                        <button class="link" title="Click to invert (is / is not). Applies to panels whose data has this field."
                            on:click=move |_| invert(i)>{text}</button>
                        <button class="link" title="Remove" on:click=move |_| remove(i)>"×"</button>
                    </span>
                }
            }).collect_view()}
            <div class="control">
                <button class="btn" on:click=move |_| open.update(|o| *o = !*o)>"+ Filter"</button>
                <Show when=move || open.get()>
                    <div class="popover options adhoc-picker">
                        <Suspense fallback=|| view! { <div class="loading small"></div> }>
                            {move || fields.get().map(|r| match r {
                                Err(e) => view! { <div class="banner error">{e}</div> }.into_any(),
                                Ok(list) => view! {
                                    <select prop:value=move || field.get()
                                        on:change=move |ev| { chosen.set(Vec::new()); field.set(event_target_value(&ev)); }>
                                        <option value="">"Field…"</option>
                                        {list.fields.into_iter().map(|(name, types)| {
                                            let label = format!("{name} ({})", types.join(", "));
                                            view! { <option value=name.clone()>{label}</option> }
                                        }).collect_view()}
                                    </select>
                                }.into_any(),
                            })}
                        </Suspense>
                        <label class="option">
                            <input type="checkbox" prop:checked=move || negate.get() on:change=move |_| negate.update(|n| *n = !*n)/>
                            <span>"is not"</span>
                        </label>
                        <Suspense fallback=|| view! { <div class="loading small"></div> }>
                            {move || values.get().map(|r| match r {
                                Err(e) => view! { <div class="banner error">{e}</div> }.into_any(),
                                Ok(v) => v.values.into_iter().map(|(value, count)| {
                                    let checked = { let value = value.clone(); move || chosen.get().contains(&value) };
                                    let pick = value.clone();
                                    view! {
                                        <label class="option">
                                            <input type="checkbox" prop:checked=checked on:change=move |_| toggle_value(pick.clone())/>
                                            <span>{value}</span>
                                            <span class="muted small">{count}</span>
                                        </label>
                                    }
                                }).collect_view().into_any(),
                            })}
                        </Suspense>
                        <button class="btn primary" prop:disabled=move || chosen.get().is_empty() on:click=move |_| add()>"Add filter"</button>
                    </div>
                </Show>
            </div>
        </div>
    }
}
