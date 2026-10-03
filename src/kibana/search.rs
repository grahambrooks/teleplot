//! Queries and filter pills, as stored in `searchSourceJSON` and Lens state.

use serde_json::{Map, Value};

use super::objects::{bool_field, str_field};
use crate::model::{Filter, Query, QueryLanguage};

/// `{ "query": "...", "language": "kuery" | "lucene" }`. Very old exports
/// may hold a DSL object as the query; that is treated as empty.
pub fn query(value: &Value) -> Query {
    let language = match value.get("language").and_then(Value::as_str) {
        Some("lucene") => QueryLanguage::Lucene,
        _ => QueryLanguage::Kql,
    };
    let text = value
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Query {
        language,
        text: text.to_string(),
    }
}

/// Filter pills. The DSL is in `query` (7.x+); older filters put it at the
/// top level next to `meta`.
pub fn filters(value: &Value) -> Vec<Filter> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items.iter().filter_map(filter).collect()
}

fn filter(value: &Value) -> Option<Filter> {
    let meta = value.get("meta").cloned().unwrap_or(Value::Null);
    let query = match value.get("query") {
        Some(q) if q.is_object() => q.clone(),
        _ => {
            let rest: Map<String, Value> = value
                .as_object()?
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "meta" | "$state" | "query"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if rest.is_empty() {
                return None;
            }
            Value::Object(rest)
        }
    };
    Some(Filter {
        label: str_field(&meta, "alias").map(String::from),
        field: str_field(&meta, "key").map(String::from),
        negate: bool_field(&meta, "negate"),
        disabled: bool_field(&meta, "disabled"),
        query,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_language_and_text() {
        let q = query(&json!({ "query": "a:1", "language": "lucene" }));
        assert_eq!(q.language, QueryLanguage::Lucene);
        assert_eq!(q.text, "a:1");
        assert_eq!(
            query(&json!({ "query": "", "language": "kuery" })),
            Query::default()
        );
        assert!(query(&json!({ "query": { "match_all": {} } })).is_empty());
    }

    #[test]
    fn reads_modern_and_legacy_filter_shapes() {
        let filters = filters(&json!([
            { "meta": { "key": "team", "negate": true, "alias": null },
              "query": { "match_phrase": { "team": "data" } }, "$state": { "store": "appState" } },
            { "meta": { "key": "x", "disabled": true }, "range": { "x": { "lt": 5 } } },
            { "meta": {} }
        ]));
        assert_eq!(filters.len(), 2, "a filter with no DSL is dropped");
        assert!(filters[0].negate && !filters[0].disabled);
        assert_eq!(
            filters[0].query,
            json!({ "match_phrase": { "team": "data" } })
        );
        assert_eq!(filters[0].label, None);
        assert!(filters[1].disabled);
        assert_eq!(filters[1].query, json!({ "range": { "x": { "lt": 5 } } }));
    }
}
