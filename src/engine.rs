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
use crate::model::{Control, Dashboard, PanelContent, PanelData, Query, TimeRange};
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
            Client::new(&config.elasticsearch)?,
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
    /// time range, queries, filters and the *other* controls' selections.
    pub async fn control_values(
        &self,
        dashboard: &Dashboard,
        control: &Control,
        request: &Request,
        now: Timestamp,
    ) -> Result<Vec<(String, u64)>, String> {
        let range = self.range(dashboard, request, now)?;
        let index = &control.data_view.index;
        let fields: HashMap<String, Vec<Field>> =
            HashMap::from([(index.clone(), (*self.fields(index).await?).clone())]);
        let others: Vec<(String, Vec<String>)> = request
            .selections
            .iter()
            .filter(|(f, _)| *f != control.field)
            .cloned()
            .collect();
        let queries: Vec<Query> = std::iter::once(dashboard.query.clone())
            .chain(request.query.clone())
            .collect();
        let stats = HashMap::new();
        let ctx = Context {
            range,
            time_zone: &self.time_zone_name,
            queries: &queries,
            filters: &dashboard.filters,
            selections: &others,
            fields: &fields,
            stats: &stats,
            settings: self.settings,
            annotations: &[],
        };
        let search = query::terms_search(&control.data_view, &control.field, 100, &ctx)?;
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
