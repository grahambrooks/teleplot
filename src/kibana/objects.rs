//! The saved objects in a Kibana export, and lookups across them.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::DataView;

#[derive(Debug, Clone, Deserialize)]
pub struct SavedObject {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub attributes: Value,
    #[serde(default)]
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Reference {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

/// Every saved object in one NDJSON export.
#[derive(Debug, Default)]
pub struct Export {
    objects: Vec<SavedObject>,
}

impl Export {
    /// Parses a Saved Objects export. The trailing summary line
    /// (`exportedCount`, `missingReferences`, ...) is skipped.
    pub fn parse(ndjson: &str) -> Result<Self, String> {
        let mut objects = Vec::new();
        for (n, line) in ndjson.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value =
                serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 1))?;
            if value.get("type").is_none() {
                continue;
            }
            let object = serde_json::from_value(value)
                .map_err(|e| format!("line {}: not a saved object: {e}", n + 1))?;
            objects.push(object);
        }
        Ok(Self { objects })
    }

    pub fn get(&self, kind: &str, id: &str) -> Option<&SavedObject> {
        self.objects.iter().find(|o| o.kind == kind && o.id == id)
    }

    pub fn of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a SavedObject> {
        self.objects.iter().filter(move |o| o.kind == kind)
    }

    /// The data view (index pattern) saved object with this id.
    pub fn data_view(&self, id: &str) -> Option<DataView> {
        let attrs = &self.get("index-pattern", id)?.attributes;
        data_view_from(attrs)
    }
}

/// A data view from an `index-pattern` object's attributes or an ad-hoc
/// data view spec; both use `title` and `timeFieldName`.
pub fn data_view_from(attrs: &Value) -> Option<DataView> {
    Some(DataView {
        index: attrs.get("title")?.as_str()?.to_string(),
        time_field: str_field(attrs, "timeFieldName").map(String::from),
    })
}

/// Kibana stores much nested JSON as strings (`panelsJSON`, `visState`,
/// `searchSourceJSON`). Accepts either form; unparseable strings become null.
pub fn json_attr(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

/// A non-empty string field.
pub fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|s| !s.is_empty())
}

pub fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub fn as_object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// Number fields sometimes arrive as strings (`"300"`).
pub fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn find_ref<'a>(refs: &'a [Reference], name: &str) -> Option<&'a Reference> {
    refs.iter().find(|r| r.name == name)
}
