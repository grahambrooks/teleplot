//! Reads Kibana saved-object exports (NDJSON, Kibana 8.x and 9.x) into the
//! dashboard [model](crate::model).
//!
//! Exports vary by version and by how each panel was saved, so conversion is
//! defensive: a panel that cannot be converted becomes a
//! [`Placeholder`] saying why, and never fails the dashboard.

mod legacy;
mod lens;
mod objects;
mod search;

use serde_json::Value;

pub use objects::Export;
use objects::{Reference, as_object, bool_field, find_ref, json_attr, number, str_field};

use crate::model::{
    Control, Dashboard, Grid, Panel, PanelContent, Placeholder, PlaceholderReason, TimeRange,
};

/// Every dashboard in an export.
pub fn read(ndjson: &str) -> Result<Vec<Dashboard>, String> {
    let export = Export::parse(ndjson)?;
    Ok(export
        .of_kind("dashboard")
        .map(|d| dashboard(&export, d))
        .collect())
}

fn dashboard(export: &Export, object: &objects::SavedObject) -> Dashboard {
    let a = &object.attributes;
    let source = json_attr(a["kibanaSavedObjectMeta"].get("searchSourceJSON"));

    let time = bool_field(a, "timeRestore")
        .then(|| {
            Some(TimeRange {
                from: str_field(a, "timeFrom")?.into(),
                to: str_field(a, "timeTo")?.into(),
            })
        })
        .flatten();
    let refresh = &a["refreshInterval"];
    let refresh_ms = (!bool_field(refresh, "pause"))
        .then(|| number(&refresh["value"]))
        .flatten()
        .filter(|ms| *ms > 0.0)
        .map(|ms| ms as u64);

    let panels = json_attr(a.get("panelsJSON"))
        .as_array()
        .map(|ps| {
            ps.iter()
                .enumerate()
                .map(|(n, p)| panel(export, object, n, p))
                .collect()
        })
        .unwrap_or_default();

    Dashboard {
        id: object.id.clone(),
        title: str_field(a, "title").unwrap_or(&object.id).to_string(),
        description: str_field(a, "description").map(String::from),
        time,
        refresh_ms,
        query: search::query(&source["query"]),
        filters: search::filters(&source["filter"]),
        controls: controls(export, object),
        min_samples: None,
        annotations: Vec::new(),
        panels,
    }
}

fn panel(export: &Export, dashboard: &objects::SavedObject, n: usize, p: &Value) -> Panel {
    let config = &p["embeddableConfig"];
    let id = str_field(p, "panelIndex")
        .or_else(|| str_field(&p["gridData"], "i"))
        .map(String::from)
        .unwrap_or_else(|| format!("panel-{n}"));
    let grid = &p["gridData"];
    let cell = |k: &str| number(&grid[k]).unwrap_or(0.0).max(0.0) as u32;
    let kind = str_field(p, "type")
        .or_else(|| str_field(config, "type"))
        .unwrap_or("unknown");

    // References a by-value panel sees: the dashboard's, scoped to the panel
    // ("<panelIndex>:<name>"), with the prefix removed.
    let prefix = format!("{id}:");
    let mut refs: Vec<Reference> = dashboard
        .references
        .iter()
        .filter_map(|r| {
            r.name.strip_prefix(&prefix).map(|name| Reference {
                name: name.into(),
                ..r.clone()
            })
        })
        .collect();

    let (content, own_title) = match by_reference(export, dashboard, &id, p, kind) {
        Some(Ok(object)) => {
            let content = match object.kind.as_str() {
                "lens" => lens::convert(export, &object.attributes, &object.references)
                    .map(PanelContent::Chart),
                "visualization" => {
                    legacy::from_saved_object(export, &object.attributes, &object.references)
                }
                other => Err(out_of_scope(
                    other,
                    format!("{other} panels are not in the MVP"),
                )),
            };
            (
                content,
                str_field(&object.attributes, "title").map(String::from),
            )
        }
        Some(Err(placeholder)) => (Err(placeholder), None),
        None => match kind {
            "lens" if config["attributes"].is_object() => {
                let attrs = &config["attributes"];
                if let Ok(own) =
                    serde_json::from_value::<Vec<Reference>>(attrs["references"].clone())
                {
                    refs.extend(own);
                }
                (
                    lens::convert(export, attrs, &refs).map(PanelContent::Chart),
                    str_field(attrs, "title").map(String::from),
                )
            }
            "visualization" if config["savedVis"].is_object() => {
                let vis = &config["savedVis"];
                (
                    legacy::from_saved_vis(export, vis, &refs),
                    str_field(vis, "title").map(String::from),
                )
            }
            "lens" | "visualization" => (
                Err(unparseable(
                    kind,
                    "panel has neither a reference nor an embedded visualization",
                )),
                None,
            ),
            other => (
                Err(out_of_scope(
                    other,
                    format!("{other} panels are not in the MVP"),
                )),
                None,
            ),
        },
    };

    // Panel-level title wins, including "" (deliberately blank).
    let title = if bool_field(config, "hidePanelTitles") {
        None
    } else {
        p.get("title")
            .or_else(|| config.get("title"))
            .and_then(Value::as_str)
            .map(String::from)
            .or(own_title)
            .filter(|t| !t.is_empty())
    };

    let time = config["timeRange"]
        .is_object()
        .then(|| {
            Some(TimeRange {
                from: str_field(&config["timeRange"], "from")?.into(),
                to: str_field(&config["timeRange"], "to")?.into(),
            })
        })
        .flatten();

    Panel {
        id,
        title,
        grid: Grid {
            x: cell("x"),
            y: cell("y"),
            w: cell("w").max(1),
            h: cell("h").max(1),
        },
        time,
        content: content.unwrap_or_else(PanelContent::Placeholder),
    }
}

/// The saved object a by-reference panel points at. `None` for by-value panels.
fn by_reference<'a>(
    export: &'a Export,
    dashboard: &objects::SavedObject,
    id: &str,
    p: &Value,
    kind: &str,
) -> Option<Result<&'a objects::SavedObject, Placeholder>> {
    let name = str_field(p, "panelRefName")?;
    // 8.x scopes the name to the panel; 7.x exports do not.
    let reference = find_ref(&dashboard.references, &format!("{id}:{name}"))
        .or_else(|| find_ref(&dashboard.references, name));
    Some(match reference {
        None => Err(broken(
            kind,
            format!("panel reference {name} is not in the dashboard's references"),
        )),
        Some(r) => export.get(&r.kind, &r.id).ok_or_else(|| {
            broken(
                &r.kind,
                format!("{} {} is not in this export", r.kind, r.id),
            )
        }),
    })
}

/// Options-list controls become [`Control`]s. Other control types (range
/// sliders, time sliders) are not in the MVP and are skipped.
fn controls(export: &Export, dashboard: &objects::SavedObject) -> Vec<Control> {
    let panels = as_object(&json_attr(
        dashboard.attributes["controlGroupInput"].get("panelsJSON"),
    ));
    let mut ordered: Vec<(f64, Control)> = panels
        .iter()
        .filter(|(_, c)| str_field(c, "type") == Some("optionsListControl"))
        .filter_map(|(id, c)| {
            let input = &c["explicitInput"];
            let field = str_field(input, "fieldName")?;
            let view_id = str_field(input, "dataViewId")
                .map(String::from)
                .or_else(|| {
                    find_ref(
                        &dashboard.references,
                        &format!("controlGroup_{id}:optionsListDataView"),
                    )
                    .map(|r| r.id.clone())
                })?;
            let control = Control {
                id: id.clone(),
                title: str_field(input, "title").unwrap_or(field).to_string(),
                field: field.to_string(),
                data_view: export.data_view(&view_id)?,
            };
            Some((number(&c["order"]).unwrap_or(0.0), control))
        })
        .collect();
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
    ordered.into_iter().map(|(_, c)| c).collect()
}

fn placeholder(kind: &str, reason: PlaceholderReason, detail: impl Into<String>) -> Placeholder {
    Placeholder {
        kind: kind.to_string(),
        reason,
        detail: detail.into(),
    }
}

fn out_of_scope(kind: &str, detail: impl Into<String>) -> Placeholder {
    placeholder(kind, PlaceholderReason::OutOfScope, detail)
}

fn broken(kind: &str, detail: impl Into<String>) -> Placeholder {
    placeholder(kind, PlaceholderReason::BrokenReference, detail)
}

fn unparseable(kind: &str, detail: impl Into<String>) -> Placeholder {
    placeholder(kind, PlaceholderReason::Unparseable, detail)
}
