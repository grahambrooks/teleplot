//! A small Elasticsearch REST client: just what Teleplot reads with.
//!
//! The official `elasticsearch` crate is still alpha, and Teleplot needs only
//! `_msearch`, `_query` (ES|QL) and `_field_caps`, so this talks to the REST
//! API directly.
//! TLS is rustls with the operating system's trust store (corporate CAs
//! work), plus an optional extra CA file.

use std::time::Duration;

use base64::Engine;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use crate::config::Elasticsearch;
use crate::kql::Field;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    /// insight's base URL, for [`Endpoint::Insight`] searches.
    insight: Option<String>,
}

impl Client {
    pub fn new(config: &Elasticsearch) -> Result<Self, String> {
        let base = match (&config.url, &config.cloud_id) {
            (Some(url), _) => url.trim_end_matches('/').to_string(),
            (None, Some(id)) => cloud_url(id)?,
            (None, None) => return Err("elasticsearch needs a url or a cloud_id".into()),
        };

        let mut headers = HeaderMap::new();
        if let Some(key) = &config.api_key {
            let value = HeaderValue::from_str(&format!("ApiKey {key}"))
                .map_err(|_| "api_key is not a valid header value")?;
            headers.insert(AUTHORIZATION, value);
        } else if let Some(user) = &config.username {
            let pair = format!("{user}:{}", config.password.as_deref().unwrap_or_default());
            let encoded = base64::engine::general_purpose::STANDARD.encode(pair);
            let mut value =
                HeaderValue::from_str(&format!("Basic {encoded}")).map_err(|_| "bad username")?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }

        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .danger_accept_invalid_certs(config.insecure);
        if let Some(path) = &config.ca_file {
            let pem = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            builder = builder.add_root_certificate(cert);
        }
        let http = builder.build().map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            base,
            insight: None,
        })
    }

    /// Send [`Endpoint::Insight`] searches to this insight server.
    pub fn with_insight(mut self, url: Option<&str>) -> Self {
        self.insight = url.map(|u| u.trim_end_matches('/').to_string());
        self
    }

    /// Whether an insight server is configured.
    pub fn has_insight(&self) -> bool {
        self.insight.is_some()
    }

    /// insight's series schema: every record type, its fields and their
    /// types (`GET /api/series/schema`).
    pub async fn insight_schema(&self) -> Result<Value, String> {
        let base = self
            .insight
            .as_deref()
            .ok_or("teleplot.yaml has no `insight: { url }`")?;
        let response = self
            .http
            .get(format!("{base}/api/series/schema"))
            .send()
            .await
            .map_err(|e| format!("insight unreachable: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("/api/series/schema failed: {}", response.status()));
        }
        response
            .json()
            .await
            .map_err(|e| format!("bad /api/series/schema response: {e}"))
    }

    /// A raw series request (`POST /api/series`), unreshaped.
    pub async fn insight_series_raw(&self, body: &Value) -> Result<Value, String> {
        let base = self
            .insight
            .as_deref()
            .ok_or("teleplot.yaml has no `insight: { url }`")?;
        let response = self
            .http
            .post(format!("{base}/api/series"))
            .json(body)
            .send()
            .await
            .map_err(|e| format!("insight unreachable: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("bad /api/series response: {e}"))?;
        if status.is_success() {
            Ok(value)
        } else {
            Err(value["error"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("/api/series failed: {status}")))
        }
    }

    /// Run a series request on insight and reshape the response like an
    /// ES|QL one: `{columns: [{name, type}], values: [[...]]}`.
    pub async fn insight_series(&self, body: &Value) -> Result<Value, String> {
        let base = self
            .insight
            .as_deref()
            .ok_or("this layer queries insight, but teleplot.yaml has no `insight: { url }`")?;
        let response = self
            .http
            .post(format!("{base}/api/series"))
            .json(body)
            .send()
            .await
            .map_err(|e| format!("insight unreachable: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("bad /api/series response: {e}"))?;
        if !status.is_success() {
            return Err(value["error"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("/api/series failed: {status}")));
        }
        Ok(series_as_esql(&value))
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Runs searches in one `_msearch` round trip, and ES|QL queries (one
    /// `_query` each) alongside it. Each succeeds or fails on its own; the
    /// outer error is for the `_msearch` as a whole.
    pub async fn msearch(&self, searches: &[Search]) -> Result<Vec<Result<Value, String>>, String> {
        self.msearch_with(
            searches,
            "responses.aggregations,responses.error,responses.status,responses.hits.total,responses.hits.hits.fields",
        )
        .await
    }

    /// Like [`Client::msearch`], but with whole responses (documents' `_source` included).
    pub async fn msearch_full(
        &self,
        searches: &[Search],
    ) -> Result<Vec<Result<Value, String>>, String> {
        self.msearch_with(searches, "").await
    }

    async fn msearch_with(
        &self,
        searches: &[Search],
        filter_path: &str,
    ) -> Result<Vec<Result<Value, String>>, String> {
        let (tabular, dsl): (Vec<usize>, Vec<usize>) =
            (0..searches.len()).partition(|&i| searches[i].endpoint != Endpoint::Search);
        if tabular.is_empty() {
            return self.msearch_only(searches, filter_path).await;
        }
        let mut queries = tokio::task::JoinSet::new();
        for &i in &tabular {
            let client = self.clone();
            let body = searches[i].body.clone();
            let endpoint = searches[i].endpoint;
            queries.spawn(async move {
                let response = match endpoint {
                    Endpoint::Insight => client.insight_series(&body).await,
                    _ => client.esql(&body).await,
                };
                (i, response)
            });
        }
        let only_dsl: Vec<Search> = dsl.iter().map(|&i| searches[i].clone()).collect();
        let dsl_responses = self.msearch_only(&only_dsl, filter_path).await?;
        let mut out: Vec<Option<Result<Value, String>>> = vec![None; searches.len()];
        for (i, response) in dsl.into_iter().zip(dsl_responses) {
            out[i] = Some(response);
        }
        while let Some(done) = queries.join_next().await {
            let (i, response) = done.map_err(|e| format!("ES|QL query failed: {e}"))?;
            out[i] = Some(response);
        }
        Ok(out
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("no response".into())))
            .collect())
    }

    /// One ES|QL query (`POST /_query`): columns and rows, or the reason it failed.
    pub async fn esql(&self, body: &Value) -> Result<Value, String> {
        let response = self
            .http
            .post(format!("{}/_query", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Elasticsearch unreachable: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("bad _query response: {e}"))?;
        match error_reason(&value) {
            Some(reason) => Err(reason),
            None if !status.is_success() => Err(format!("_query failed: {status}")),
            None => Ok(value),
        }
    }

    async fn msearch_only(
        &self,
        searches: &[Search],
        filter_path: &str,
    ) -> Result<Vec<Result<Value, String>>, String> {
        if searches.is_empty() {
            return Ok(Vec::new());
        }
        let mut body = String::new();
        for s in searches {
            let header = json!({ "index": s.index, "ignore_unavailable": true, "allow_no_indices": true, "request_cache": true });
            body.push_str(&header.to_string());
            body.push('\n');
            body.push_str(&s.body.to_string());
            body.push('\n');
        }
        let url = if filter_path.is_empty() {
            format!("{}/_msearch", self.base)
        } else {
            format!("{}/_msearch?filter_path={filter_path}", self.base)
        };
        let response = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/x-ndjson")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("Elasticsearch unreachable: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("bad _msearch response: {e}"))?;
        if !status.is_success() {
            return Err(
                error_reason(&value).unwrap_or_else(|| format!("_msearch failed: {status}"))
            );
        }
        let responses = value["responses"].as_array().cloned().unwrap_or_default();
        if responses.len() != searches.len() {
            return Err(format!(
                "_msearch returned {} responses for {} searches",
                responses.len(),
                searches.len()
            ));
        }
        Ok(responses
            .into_iter()
            .map(|r| match error_reason(&r) {
                Some(reason) => Err(reason),
                None => Ok(r),
            })
            .collect())
    }

    /// Indices and data streams (hidden ones excluded): name and document count.
    pub async fn indices(&self) -> Result<Vec<(String, u64)>, String> {
        let url = format!(
            "{}/_cat/indices?format=json&h=index,docs.count&s=index&expand_wildcards=open",
            self.base
        );
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Elasticsearch unreachable: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("bad _cat/indices response: {e}"))?;
        if !status.is_success() {
            return Err(
                error_reason(&value).unwrap_or_else(|| format!("_cat/indices failed: {status}"))
            );
        }
        Ok(value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| {
                let name = i["index"].as_str()?;
                let docs = i["docs.count"]
                    .as_str()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                (!name.starts_with('.')).then(|| (name.to_string(), docs))
            })
            .collect())
    }

    /// The fields of an index pattern and their Elasticsearch types.
    pub async fn field_caps(&self, index: &str) -> Result<Vec<Field>, String> {
        let url = format!(
            "{}/{}/_field_caps?fields=*&ignore_unavailable=true&allow_no_indices=true",
            self.base,
            encode_path(index)
        );
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Elasticsearch unreachable: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|e| format!("bad _field_caps response: {e}"))?;
        if !status.is_success() {
            return Err(
                error_reason(&value).unwrap_or_else(|| format!("_field_caps failed: {status}"))
            );
        }
        Ok(fields_from_caps(&value))
    }
}

/// One search: a query DSL search in an `_msearch`, or an ES|QL query.
#[derive(Debug, Clone, PartialEq)]
pub struct Search {
    /// The index pattern searched (for ES|QL, informational: `FROM` decides).
    pub index: String,
    pub body: Value,
    pub endpoint: Endpoint,
}

/// Where a [`Search`] is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endpoint {
    /// `_msearch`, with the body as query DSL.
    #[default]
    Search,
    /// `POST /_query`, with the body as an ES|QL request.
    Esql,
    /// insight's `POST /api/series`, with the body as a series request. The
    /// response is reshaped like an ES|QL one, so it is read the same way.
    Insight,
}

impl Search {
    pub fn dsl(index: impl Into<String>, body: Value) -> Self {
        Search {
            index: index.into(),
            body,
            endpoint: Endpoint::Search,
        }
    }
}

/// `_field_caps` → fields, skipping metadata (`_id`, ...) and object fields.
pub fn fields_from_caps(value: &Value) -> Vec<Field> {
    let mut fields: Vec<Field> = value["fields"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, _)| !name.starts_with('_'))
        .filter_map(|(name, types)| {
            let mut es_types: Vec<String> = types
                .as_object()?
                .keys()
                .filter(|t| !matches!(t.as_str(), "object" | "nested" | "unmapped"))
                .cloned()
                .collect();
            es_types.sort();
            (!es_types.is_empty()).then(|| Field {
                name: name.clone(),
                es_types,
            })
        })
        .collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

/// An insight series response (`{columns: [{id, type}], rows}`) in the
/// shape of an ES|QL response, which the ES|QL layer evaluation reads.
pub fn series_as_esql(series: &Value) -> Value {
    let columns: Vec<Value> = series["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| json!({ "name": c["id"], "type": c["type"] }))
        .collect();
    let mut esql = json!({ "columns": columns, "values": series["rows"].clone() });
    if let Some(buckets) = series.get("buckets") {
        esql["buckets"] = buckets.clone();
    }
    esql
}

fn error_reason(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    let root = &error["root_cause"][0];
    let reason = root["reason"]
        .as_str()
        .or_else(|| error["reason"].as_str())
        .unwrap_or("unknown error");
    let kind = root["type"]
        .as_str()
        .or_else(|| error["type"].as_str())
        .unwrap_or("error");
    Some(format!("{kind}: {reason}"))
}

/// Index patterns may contain `*` and `,`, which are fine in a path; other
/// reserved characters are not expected in index names.
fn encode_path(index: &str) -> String {
    index.replace(' ', "%20")
}

/// `name:base64(host$es_uuid$kibana_uuid)` → `https://es_uuid.host[:port]`.
fn cloud_url(cloud_id: &str) -> Result<String, String> {
    let encoded = cloud_id.rsplit_once(':').map_or(cloud_id, |(_, e)| e);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "cloud_id is not valid base64")?;
    let decoded = String::from_utf8(decoded).map_err(|_| "cloud_id is not UTF-8")?;
    let mut parts = decoded.split('$');
    let host = parts
        .next()
        .filter(|h| !h.is_empty())
        .ok_or("cloud_id has no host")?;
    let es = parts
        .next()
        .filter(|h| !h.is_empty())
        .ok_or("cloud_id has no Elasticsearch id")?;
    Ok(match host.rsplit_once(':') {
        Some((host, port)) => format!("https://{es}.{host}:{port}"),
        None => format!("https://{es}.{host}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_cloud_ids() {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode("us-east-1.aws.found.io:443$abc123$kib456");
        assert_eq!(
            cloud_url(&format!("my-deployment:{encoded}")).unwrap(),
            "https://abc123.us-east-1.aws.found.io:443"
        );
        let no_port = base64::engine::general_purpose::STANDARD.encode("eu.example.io$def$ghi");
        assert_eq!(cloud_url(&no_port).unwrap(), "https://def.eu.example.io");
        assert!(cloud_url("x:not base64!").is_err());
    }

    #[test]
    fn reads_field_caps() {
        let caps = json!({ "indices": ["a"], "fields": {
            "team": { "keyword": { "type": "keyword" } },
            "_id": { "_id": { "type": "_id" } },
            "user": { "object": { "type": "object" } },
            "count": { "long": {}, "integer": {} }
        } });
        assert_eq!(
            fields_from_caps(&caps),
            [
                Field {
                    name: "count".into(),
                    es_types: vec!["integer".into(), "long".into()]
                },
                Field::new("team", "keyword")
            ]
        );
    }

    #[test]
    fn extracts_error_reasons() {
        let err = json!({ "error": { "root_cause": [{ "type": "index_not_found_exception", "reason": "no such index [x]" }] }, "status": 404 });
        assert_eq!(
            error_reason(&err).unwrap(),
            "index_not_found_exception: no such index [x]"
        );
        assert_eq!(error_reason(&json!({ "took": 1 })), None);
    }
}
