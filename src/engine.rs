//! Runs a dashboard's queries against Elasticsearch.
//!
//! One `_msearch` covers every panel (after a min/max pre-pass when a
//! histogram needs an automatic interval). Field capabilities and recent
//! search responses are cached in memory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moka::future::Cache;
use serde_json::Value;

use crate::config::Config;
use crate::es::{Client, Search};
use crate::kql::Field;
use crate::model::api::{InspectedRequest, Inspection};
use crate::model::{
    Control, Dashboard, DataView, Filter, PanelContent, PanelData, Query, TimeRange,
};
use crate::query::{self, Context, Settings};
use crate::time;

#[derive(Clone)]
pub struct Engine {
    client: Client,
    time_zone: TimeZone,
    time_zone_name: String,
    settings: Settings,
    field_caps: Cache<String, Arc<Vec<Field>>>,
    responses: Cache<String, Arc<Value>>,
}

/// Planned panels, ready to search.
struct Prepared {
    plans: Vec<(String, query::Plan)>,
    /// The min/max pre-pass: what each search was for, and its response.
    stats: Vec<(String, Search, Result<Value, String>)>,
}

/// What the viewer asked for.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Overrides the dashboard's default time range.
    pub time: Option<TimeRange>,
    /// The viewer's query bar, ANDed with the dashboard's query.
    pub query: Option<Query>,
    /// Control selections: field → chosen values.
    pub selections: Vec<(String, Vec<String>)>,
    /// The viewer's ad-hoc filter pills; each applies only to indexes that
    /// have its field.
    pub filters: Vec<crate::model::Filter>,
}

impl Engine {
    pub fn new(config: &Config) -> Result<Self, String> {
        let time_zone = config.time_zone();
        let time_zone_name = config
            .time_zone
            .clone()
            .or_else(|| time_zone.iana_name().map(String::from))
            .unwrap_or_else(|| "UTC".into());
        Ok(Self::with_client(
            Client::new(&config.elasticsearch)?
                .with_insight(config.insight.as_ref().map(|i| i.url.as_str())),
            time_zone,
            time_zone_name,
        ))
    }

    pub fn with_client(client: Client, time_zone: TimeZone, time_zone_name: String) -> Self {
        Engine {
            client,
            time_zone,
            time_zone_name,
            settings: Settings::default(),
            field_caps: Cache::builder()
                .time_to_live(Duration::from_secs(300))
                .max_capacity(256)
                .build(),
            responses: Cache::builder()
                .time_to_live(Duration::from_secs(30))
                .max_capacity(4096)
                .build(),
        }
    }

    /// Data for every chart panel of a dashboard, keyed by panel id.
    pub async fn dashboard_data(
        &self,
        dashboard: &Dashboard,
        request: &Request,
        now: Timestamp,
    ) -> Result<HashMap<String, PanelData>, String> {
        let prepared = self.prepare(dashboard, request, now, None).await?;
        let searches: Vec<Search> = prepared
            .plans
            .iter()
            .flat_map(|(_, p)| p.searches())
            .collect();
        let mut responses = self.search(&searches).await?.into_iter();

        let mut out = HashMap::new();
        for (id, plan) in prepared.plans {
            let mine: Vec<_> = responses.by_ref().take(plan.searches().len()).collect();
            out.insert(id, plan.evaluate(&mine));
        }
        Ok(out)
    }

    /// How one panel's numbers are made: every search it sends (the
    /// automatic-interval pre-pass included), with Elasticsearch's whole
    /// responses. Runs past the response cache, so timings are current.
    pub async fn inspect_panel(
        &self,
        dashboard: &Dashboard,
        panel: &str,
        request: &Request,
        now: Timestamp,
    ) -> Result<Inspection, String> {
        let prepared = self.prepare(dashboard, request, now, Some(panel)).await?;
        let Some((_, plan)) = prepared.plans.first() else {
            return Err(format!("no chart panel {panel}"));
        };
        let described = plan.described_searches();
        let searches: Vec<Search> = described.iter().map(|(_, s)| s.clone()).collect();
        let responses = self.client.msearch_full(&searches).await?;
        let data = plan.evaluate(&responses);

        let inspected = |description: String, search: &Search, response: Result<Value, String>| {
            let (response, error) = match response {
                Ok(r) => (Some(r), None),
                Err(e) => (None, Some(e)),
            };
            let hits = response.as_ref().and_then(|r| {
                r["aggregations"]["root"]["doc_count"]
                    .as_u64()
                    .or_else(|| r["hits"]["total"]["value"].as_u64())
                    .or_else(|| r["hits"]["hits"].as_array().map(|h| h.len() as u64))
                    // ES|QL: the rows returned.
                    .or_else(|| r["values"].as_array().map(|v| v.len() as u64))
            });
            InspectedRequest {
                description,
                index: search.index.clone(),
                request: search.body.clone(),
                took_ms: response.as_ref().and_then(|r| r["took"].as_u64()),
                hits,
                response,
                error,
            }
        };
        let mut requests: Vec<InspectedRequest> = prepared
            .stats
            .into_iter()
            .map(|(description, search, response)| inspected(description, &search, response))
            .collect();
        requests.extend(
            described
                .into_iter()
                .zip(responses)
                .map(|((description, search), response)| inspected(description, &search, response)),
        );
        Ok(Inspection { requests, data })
    }

    /// How a selection on one panel (a box on its scatter or heatmap)
    /// differs from all of the panel's documents. See [`crate::compare`].
    pub async fn compare(
        &self,
        dashboard: &Dashboard,
        panel: &str,
        request: &Request,
        selection: &crate::model::api::Selection,
        now: Timestamp,
    ) -> Result<crate::model::api::Comparison, String> {
        let p = dashboard
            .panels
            .iter()
            .find(|p| p.id == panel)
            .ok_or_else(|| format!("no panel {panel}"))?;
        let PanelContent::Chart(chart) = &p.content else {
            return Err(format!("panel {panel} is not a chart"));
        };
        let range = match &p.time {
            Some(t) => time::resolve(&t.from, &t.to, now, &self.time_zone)?,
            None => self.range(dashboard, request, now)?,
        };
        let indexes: Vec<String> = chart
            .layers
            .iter()
            .map(|l| l.data_view.index.clone())
            .collect();
        let fields = self.fields_for(&indexes).await;
        let queries: Vec<Query> = std::iter::once(dashboard.query.clone())
            .chain(request.query.clone())
            .collect();
        let stats = HashMap::new();
        let ctx = Context {
            range,
            time_zone: &self.time_zone_name,
            queries: &queries,
            filters: &dashboard.filters,
            selections: &request.selections,
            adhoc: &request.filters,
            fields: &fields,
            stats: &stats,
            settings: self.settings,
            annotations: &[],
        };
        let plan = crate::compare::plan(chart, selection, &ctx)?;
        let mut responses = self.search(&plan.searches).await?.into_iter();
        let selected = responses.next().ok_or("no response")??;
        let baseline = responses.next().ok_or("no response")??;
        Ok(crate::compare::rank(&plan, &selected, &baseline))
    }

    /// Plans the chart panels (all, or `only` one): field caps, time ranges,
    /// and the min/max pre-pass that automatic histogram intervals need.
    async fn prepare(
        &self,
        dashboard: &Dashboard,
        request: &Request,
        now: Timestamp,
        only: Option<&str>,
    ) -> Result<Prepared, String> {
        let dashboard_range = self.range(dashboard, request, now)?;

        let charts: Vec<_> = dashboard
            .panels
            .iter()
            .filter(|p| only.is_none_or(|id| p.id == id))
            .filter_map(|p| match &p.content {
                PanelContent::Chart(c) => Some((p, c)),
                _ => None,
            })
            .collect();

        // Field types for every index searched: chart layers and annotations.
        let indexes: Vec<String> = charts
            .iter()
            .flat_map(|(_, chart)| {
                let layers = chart.layers.iter().map(|l| &l.data_view.index);
                let annotations = chart.annotations.iter().chain(&dashboard.annotations);
                layers.chain(annotations.filter_map(|a| a.data_view.as_ref().map(|v| &v.index)))
            })
            .cloned()
            .collect();
        let fields = self.fields_for(&indexes).await;

        let mut ranges = Vec::new();
        for (panel, _) in &charts {
            ranges.push(match &panel.time {
                Some(t) => time::resolve(&t.from, &t.to, now, &self.time_zone)?,
                None => dashboard_range,
            });
        }

        let empty_stats = HashMap::new();
        let queries: Vec<Query> = std::iter::once(dashboard.query.clone())
            .chain(request.query.clone())
            .collect();
        let context = |range, stats| Context {
            range,
            time_zone: &self.time_zone_name,
            queries: &queries,
            filters: &dashboard.filters,
            selections: &request.selections,
            adhoc: &request.filters,
            fields: &fields,
            stats,
            settings: self.settings,
            annotations: &dashboard.annotations,
        };

        // Pre-pass: field ranges for automatic histogram intervals.
        let mut stat_keys = Vec::new();
        let mut stat_searches = Vec::new();
        for ((_, chart), range) in charts.iter().zip(&ranges) {
            for (key, search) in query::stats_searches(chart, &context(*range, &empty_stats)) {
                stat_keys.push(key);
                stat_searches.push(search);
            }
        }
        let stat_responses = self.search(&stat_searches).await?;
        let mut stats = HashMap::new();
        let mut described = Vec::new();
        for (((layer, column), search), response) in
            stat_keys.into_iter().zip(stat_searches).zip(stat_responses)
        {
            if let Some(minmax) = response.as_ref().ok().and_then(query::read_stats) {
                stats.insert((layer.clone(), column.clone()), minmax);
            }
            described.push((
                format!("Layer {layer}, min and max for column {column}'s automatic interval"),
                search,
                response,
            ));
        }

        let plans = charts
            .iter()
            .zip(&ranges)
            .map(|((panel, chart), range)| {
                (
                    panel.id.clone(),
                    query::plan(chart, &context(*range, &stats)),
                )
            })
            .collect();
        Ok(Prepared {
            plans,
            stats: described,
        })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Fields of each index pattern (cached), for checking dashboards.
    pub async fn fields_for(&self, indexes: &[String]) -> HashMap<String, Vec<Field>> {
        let mut out = HashMap::new();
        for index in indexes {
            if !out.contains_key(index) {
                let found = self
                    .fields(index)
                    .await
                    .map(|f| (*f).clone())
                    .unwrap_or_default();
                out.insert(index.clone(), found);
            }
        }
        out
    }

    pub fn time_zone_name(&self) -> &str {
        &self.time_zone_name
    }

    /// The resolved time range for a request, epoch milliseconds.
    pub fn range(
        &self,
        dashboard: &Dashboard,
        request: &Request,
        now: Timestamp,
    ) -> Result<time::Range, String> {
        let default_time = TimeRange {
            from: "now-15m".into(),
            to: "now".into(),
        };
        let time = request
            .time
            .as_ref()
            .or(dashboard.time.as_ref())
            .unwrap_or(&default_time);
        time::resolve(&time.from, &time.to, now, &self.time_zone)
    }

    /// The most common values of a control's field, under the dashboard's
    /// time range, queries and filters, and the selections of the controls
    /// before it (none when the dashboard's controls aren't chained).
    pub async fn control_values(
        &self,
        dashboard: &Dashboard,
        control: &Control,
        request: &Request,
        now: Timestamp,
    ) -> Result<Vec<(String, u64)>, String> {
        let position = dashboard
            .controls
            .iter()
            .position(|c| c.id == control.id)
            .unwrap_or(dashboard.controls.len());
        let before: Vec<&str> = dashboard.controls[..position]
            .iter()
            .filter(|_| dashboard.chain_controls)
            .map(|c| c.field.as_str())
            .collect();
        let others: Vec<(String, Vec<String>)> = request
            .selections
            .iter()
            .filter(|(f, _)| *f != control.field && before.contains(&f.as_str()))
            .cloned()
            .collect();
        self.values(
            dashboard,
            &control.data_view,
            &control.field,
            &others,
            &request.filters,
            request,
            now,
        )
        .await
    }

    /// The fields an ad-hoc filter can use: those of every index the
    /// dashboard reads that can be filtered by value (keywords, booleans,
    /// numbers, IPs), with their types. Fields about people are left out:
    /// Teleplot has no per-engineer views.
    pub async fn filterable_fields(&self, dashboard: &Dashboard) -> Vec<(String, Vec<String>)> {
        const FILTERABLE: &[&str] = &[
            "keyword",
            "constant_keyword",
            "boolean",
            "ip",
            "version",
            "long",
            "integer",
            "short",
            "byte",
            "double",
            "float",
            "half_float",
            "scaled_float",
            "unsigned_long",
        ];
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for view in data_views(dashboard) {
            let Ok(fields) = self.fields(&view.index).await else {
                continue;
            };
            for f in fields.iter() {
                // No per-engineer views: people's fields are not offered.
                if !f.es_types.iter().all(|t| FILTERABLE.contains(&t.as_str()))
                    || crate::compare::about_people(&f.name)
                {
                    continue;
                }
                match out.iter_mut().find(|(name, _)| *name == f.name) {
                    Some((_, types)) => {
                        for t in &f.es_types {
                            if !types.contains(t) {
                                types.push(t.clone());
                            }
                        }
                    }
                    None => out.push((f.name.clone(), f.es_types.clone())),
                }
            }
        }
        out.sort();
        out
    }

    /// Values to suggest for an ad-hoc filter on `field`: from the first of
    /// the dashboard's data views that has it, under every control
    /// selection and the other ad-hoc filters.
    pub async fn field_values(
        &self,
        dashboard: &Dashboard,
        field: &str,
        request: &Request,
        now: Timestamp,
    ) -> Result<Vec<(String, u64)>, String> {
        if crate::compare::about_people(field) {
            return Err(format!(
                "{field} is about people; Teleplot does not filter or rank by person"
            ));
        }
        let mut view = None;
        for v in data_views(dashboard) {
            if self
                .fields(&v.index)
                .await
                .is_ok_and(|f| f.iter().any(|f| f.name == field))
            {
                view = Some(v);
                break;
            }
        }
        let view =
            view.ok_or_else(|| format!("no data view of this dashboard has a field {field}"))?;
        let others: Vec<Filter> = request
            .filters
            .iter()
            .filter(|f| f.field.as_deref() != Some(field))
            .cloned()
            .collect();
        self.values(
            dashboard,
            &view,
            field,
            &request.selections,
            &others,
            request,
            now,
        )
        .await
    }

    /// The most common values of a field under the dashboard's range,
    /// queries and filters, and the given selections and ad-hoc filters.
    #[allow(clippy::too_many_arguments)]
    async fn values(
        &self,
        dashboard: &Dashboard,
        view: &DataView,
        field: &str,
        selections: &[(String, Vec<String>)],
        adhoc: &[Filter],
        request: &Request,
        now: Timestamp,
    ) -> Result<Vec<(String, u64)>, String> {
        let range = self.range(dashboard, request, now)?;
        let index = &view.index;
        let fields: HashMap<String, Vec<Field>> =
            HashMap::from([(index.clone(), (*self.fields(index).await?).clone())]);
        let queries: Vec<Query> = std::iter::once(dashboard.query.clone())
            .chain(request.query.clone())
            .collect();
        let stats = HashMap::new();
        let ctx = Context {
            range,
            time_zone: &self.time_zone_name,
            queries: &queries,
            filters: &dashboard.filters,
            selections,
            adhoc,
            fields: &fields,
            stats: &stats,
            settings: self.settings,
            annotations: &[],
        };
        let search = query::terms_search(view, field, 100, &ctx)?;
        let response = self.search(&[search]).await?.remove(0)?;
        Ok(response["aggregations"]["values"]["buckets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| {
                let key =
                    b["key_as_string"]
                        .as_str()
                        .map(String::from)
                        .or_else(|| match &b["key"] {
                            Value::String(s) => Some(s.clone()),
                            Value::Null => None,
                            other => Some(other.to_string()),
                        })?;
                Some((key, b["doc_count"].as_u64().unwrap_or(0)))
            })
            .collect())
    }

    async fn fields(&self, index: &str) -> Result<Arc<Vec<Field>>, String> {
        if let Some(hit) = self.field_caps.get(index).await {
            return Ok(hit);
        }
        let fields = Arc::new(self.client.field_caps(index).await?);
        self.field_caps
            .insert(index.to_string(), fields.clone())
            .await;
        Ok(fields)
    }

    /// `_msearch` with a short-lived cache in front: only misses go to
    /// Elasticsearch, and a search repeated in one batch (the same
    /// annotation on several charts) is sent once.
    async fn search(&self, searches: &[Search]) -> Result<Vec<Result<Value, String>>, String> {
        let keys: Vec<String> = searches
            .iter()
            .map(|s| format!("{:?}\n{}\n{}", s.endpoint, s.index, s.body))
            .collect();
        let mut results: Vec<Option<Result<Value, String>>> = Vec::with_capacity(searches.len());
        let mut misses = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            match self.responses.get(key).await {
                Some(hit) => results.push(Some(Ok((*hit).clone()))),
                None => {
                    results.push(None);
                    misses.push(i);
                }
            }
        }
        if !misses.is_empty() {
            // The first of each distinct missed search, and who else wants it.
            let mut unique: Vec<usize> = Vec::new();
            let mut first_of: HashMap<&str, usize> = HashMap::new();
            for &i in &misses {
                first_of.entry(keys[i].as_str()).or_insert_with(|| {
                    unique.push(i);
                    i
                });
            }
            let batch: Vec<Search> = unique.iter().map(|&i| searches[i].clone()).collect();
            let mut answered: HashMap<usize, Result<Value, String>> = HashMap::new();
            for (i, response) in unique.into_iter().zip(self.client.msearch(&batch).await?) {
                if let Ok(value) = &response {
                    self.responses
                        .insert(keys[i].clone(), Arc::new(value.clone()))
                        .await;
                }
                answered.insert(i, response);
            }
            for i in misses {
                results[i] = answered.get(&first_of[keys[i].as_str()]).cloned();
            }
        }
        Ok(results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("no response".into())))
            .collect())
    }
}

/// Every data view a dashboard reads (chart layers, controls, annotations),
/// once each, in order.
fn data_views(dashboard: &Dashboard) -> Vec<DataView> {
    let mut out: Vec<DataView> = Vec::new();
    let layers = dashboard.panels.iter().flat_map(|p| match &p.content {
        PanelContent::Chart(c) => c.layers.iter().map(|l| l.data_view.clone()).collect(),
        _ => Vec::new(),
    });
    let controls = dashboard.controls.iter().map(|c| c.data_view.clone());
    let annotations = dashboard
        .annotations
        .iter()
        .filter_map(|a| a.data_view.clone());
    for view in layers.chain(controls).chain(annotations) {
        if !out.iter().any(|v| v.index == view.index) {
            out.push(view);
        }
    }
    out
}
