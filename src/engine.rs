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
        let default_time = TimeRange {
            from: "now-15m".into(),
            to: "now".into(),
        };
        let time = request
            .time
            .as_ref()
            .or(dashboard.time.as_ref())
            .unwrap_or(&default_time);
        let dashboard_range = time::resolve(&time.from, &time.to, now, &self.time_zone)?;

        let charts: Vec<_> = dashboard
            .panels
            .iter()
            .filter_map(|p| match &p.content {
                PanelContent::Chart(c) => Some((p, c)),
                _ => None,
            })
            .collect();

        let mut fields = HashMap::new();
        for (_, chart) in &charts {
            for layer in &chart.layers {
                let index = &layer.data_view.index;
                if !fields.contains_key(index) {
                    let found = self
                        .fields(index)
                        .await
                        .map(|f| (*f).clone())
                        .unwrap_or_default();
                    fields.insert(index.clone(), found);
                }
            }
        }

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
        let mut stats = HashMap::new();
        for (key, response) in stat_keys
            .into_iter()
            .zip(self.search(&stat_searches).await?)
        {
            if let Some(minmax) = response.ok().as_ref().and_then(query::read_stats) {
                stats.insert(key, minmax);
            }
        }

        let plans: Vec<_> = charts
            .iter()
            .zip(&ranges)
            .map(|((panel, chart), range)| {
                (
                    panel.id.clone(),
                    query::plan(chart, &context(*range, &stats)),
                )
            })
            .collect();
        let searches: Vec<Search> = plans.iter().flat_map(|(_, p)| p.searches()).collect();
        let mut responses = self.search(&searches).await?.into_iter();

        let mut out = HashMap::new();
        for (id, plan) in plans {
            let mine: Vec<_> = responses.by_ref().take(plan.searches().len()).collect();
            out.insert(id, plan.evaluate(&mine));
        }
        Ok(out)
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

    /// `_msearch` with a short-lived cache in front: only misses go to Elasticsearch.
    async fn search(&self, searches: &[Search]) -> Result<Vec<Result<Value, String>>, String> {
        let keys: Vec<String> = searches
            .iter()
            .map(|s| format!("{}\n{}", s.index, s.body))
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
            let batch: Vec<Search> = misses.iter().map(|&i| searches[i].clone()).collect();
            for (i, response) in misses.into_iter().zip(self.client.msearch(&batch).await?) {
                if let Ok(value) = &response {
                    self.responses
                        .insert(keys[i].clone(), Arc::new(value.clone()))
                        .await;
                }
                results[i] = Some(response);
            }
        }
        Ok(results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("no response".into())))
            .collect())
    }
}
