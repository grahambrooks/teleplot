//! KQL (Kibana Query Language) → Elasticsearch query DSL.
//!
//! Elasticsearch only understands KQL natively from 9.1, and Teleplot also
//! targets 8.x, so KQL is translated here for every version. The parser
//! ports Kibana's grammar and the translation follows Kibana's `kuery`
//! functions (`is`, `range`, `and`, `or`, `not`, `nested`), including their
//! use of field types: keyword fields get `term`, date fields a `range`.

mod parse;

use serde_json::{Value as Json, json};

pub use parse::{ParseError, parse};

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Or(Vec<Node>),
    And(Vec<Node>),
    Not(Box<Node>),
    /// `field: value`, or a bare `value` (field `None`) searched in all fields.
    Is {
        field: Option<Literal>,
        value: Literal,
    },
    Range {
        field: Literal,
        op: RangeOp,
        value: Literal,
    },
    Nested {
        path: Literal,
        query: Box<Node>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOp {
    Lt,
    Lte,
    Gt,
    Gte,
}

impl RangeOp {
    fn key(self) -> &'static str {
        match self {
            RangeOp::Lt => "lt",
            RangeOp::Lte => "lte",
            RangeOp::Gt => "gt",
            RangeOp::Gte => "gte",
        }
    }
}

/// A value or field name. Unquoted literals may contain wildcards.
#[derive(Debug, Clone, PartialEq)]
pub struct Literal {
    pub pieces: Vec<Piece>,
    pub quoted: bool,
    /// Unquoted `true`, `false` and `null` are typed literals.
    pub value: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    Wildcard,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Null,
}

impl Literal {
    fn wildcard() -> Self {
        Literal {
            pieces: vec![Piece::Wildcard],
            quoted: false,
            value: None,
        }
    }

    fn is_wildcard(&self) -> bool {
        self.pieces.iter().any(|p| matches!(p, Piece::Wildcard))
    }

    /// Exactly one `*` and nothing else.
    fn is_lone_wildcard(&self) -> bool {
        matches!(self.pieces.as_slice(), [Piece::Wildcard])
    }

    fn text(&self) -> String {
        self.pieces
            .iter()
            .map(|p| match p {
                Piece::Text(t) => t.as_str(),
                Piece::Wildcard => "*",
            })
            .collect()
    }

    /// The value as JSON: typed literals stay typed, everything else is a string.
    fn json(&self) -> Json {
        match self.value {
            Some(Value::Bool(b)) => Json::Bool(b),
            Some(Value::Null) => Json::Null,
            None => Json::String(self.text()),
        }
    }

    /// For `query_string` and `wildcard` queries: text escaped, `*` kept.
    fn query_string(&self) -> String {
        self.pieces
            .iter()
            .map(|p| match p {
                Piece::Text(t) => escape_query_string(t),
                Piece::Wildcard => "*".into(),
            })
            .collect()
    }

    /// Whether a field name matches this (possibly wildcard) pattern.
    fn matches(&self, name: &str) -> bool {
        glob(&self.pieces, name)
    }
}

fn glob(pieces: &[Piece], s: &str) -> bool {
    match pieces.split_first() {
        None => s.is_empty(),
        Some((Piece::Text(t), rest)) => s.strip_prefix(t.as_str()).is_some_and(|r| glob(rest, r)),
        Some((Piece::Wildcard, rest)) => (0..=s.len())
            .filter(|&i| s.is_char_boundary(i))
            .any(|i| glob(rest, &s[i..])),
    }
}

fn escape_query_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "+-=&|><!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A field of the searched indexes, from `_field_caps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    /// Elasticsearch types, e.g. `["keyword"]`; several when indexes disagree.
    pub es_types: Vec<String>,
}

impl Field {
    pub fn new(name: impl Into<String>, es_type: &str) -> Self {
        Field {
            name: name.into(),
            es_types: vec![es_type.into()],
        }
    }

    fn is_keyword(&self) -> bool {
        self.es_types == ["keyword"]
    }

    fn is_date(&self) -> bool {
        !self.es_types.is_empty()
            && self
                .es_types
                .iter()
                .all(|t| t == "date" || t == "date_nanos")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Time zone for exact-date matches on date fields, e.g. `Europe/London`.
    pub time_zone: Option<String>,
}

/// Translates a KQL query to DSL. `fields` drives type-specific queries and
/// field-name wildcards; without it, field names are used as written.
pub fn to_dsl(node: &Node, fields: &[Field], options: &Options) -> Json {
    Translator { fields, options }.node(node, None)
}

/// Parses and translates in one step.
pub fn translate(query: &str, fields: &[Field], options: &Options) -> Result<Json, ParseError> {
    Ok(to_dsl(&parse(query)?, fields, options))
}

struct Translator<'a> {
    fields: &'a [Field],
    options: &'a Options,
}

impl Translator<'_> {
    fn node(&self, node: &Node, nested: Option<&str>) -> Json {
        match node {
            Node::Or(children) => json!({ "bool": {
                "should": children.iter().map(|c| self.node(c, nested)).collect::<Vec<_>>(),
                "minimum_should_match": 1
            } }),
            Node::And(children) => json!({ "bool": {
                "filter": children.iter().map(|c| self.node(c, nested)).collect::<Vec<_>>()
            } }),
            Node::Not(child) => json!({ "bool": { "must_not": self.node(child, nested) } }),
            Node::Nested { path, query } => {
                let path = full_name(&path.text(), nested);
                json!({ "nested": { "path": path, "query": self.node(query, Some(&path)), "score_mode": "none" } })
            }
            Node::Range { field, op, value } => {
                let queries: Vec<Json> = self
                    .fields_for(field, nested)
                    .iter()
                    .map(|f| {
                        let mut params = json!({ op.key(): value.json() });
                        if f.is_date() {
                            self.add_time_zone(&mut params);
                        }
                        json!({ "range": { &f.name: params } })
                    })
                    .collect();
                should(queries)
            }
            Node::Is { field, value } => self.is(field.as_ref(), value, nested),
        }
    }

    fn is(&self, field: Option<&Literal>, value: &Literal, nested: Option<&str>) -> Json {
        let exists = value.is_lone_wildcard();
        if exists && field.is_some_and(Literal::is_lone_wildcard) {
            return json!({ "match_all": {} });
        }
        let Some(field) = field else {
            if value.is_wildcard() {
                return json!({ "query_string": { "query": value.query_string() } });
            }
            let kind = if value.quoted {
                "phrase"
            } else {
                "best_fields"
            };
            return json!({ "multi_match": { "type": kind, "query": value.json(), "lenient": true } });
        };

        let queries = self
            .fields_for(field, nested)
            .iter()
            .map(|f| {
                if exists {
                    json!({ "exists": { "field": f.name } })
                } else if value.is_wildcard() {
                    if f.is_keyword() {
                        json!({ "wildcard": { &f.name: { "value": value.query_string() } } })
                    } else {
                        json!({ "query_string": { "fields": [f.name], "query": value.query_string() } })
                    }
                } else if f.is_date() {
                    let mut params = json!({ "gte": value.json(), "lte": value.json() });
                    self.add_time_zone(&mut params);
                    json!({ "range": { &f.name: params } })
                } else if f.is_keyword() {
                    json!({ "term": { &f.name: { "value": value.json() } } })
                } else if value.quoted {
                    json!({ "match_phrase": { &f.name: value.json() } })
                } else {
                    json!({ "match": { &f.name: value.json() } })
                }
            })
            .collect();
        should(queries)
    }

    /// Fields a (possibly wildcard) name refers to. Unknown names are used
    /// as written, as Kibana does, so they match nothing in indexes without them.
    fn fields_for(&self, field: &Literal, nested: Option<&str>) -> Vec<Field> {
        let pattern = match nested {
            Some(path) => {
                let mut pieces = vec![Piece::Text(format!("{path}."))];
                pieces.extend(field.pieces.iter().cloned());
                Literal {
                    pieces,
                    ..field.clone()
                }
            }
            None => field.clone(),
        };
        let found: Vec<Field> = self
            .fields
            .iter()
            .filter(|f| pattern.matches(&f.name))
            .cloned()
            .collect();
        if found.is_empty() {
            vec![Field {
                name: pattern.text(),
                es_types: vec![],
            }]
        } else {
            found
        }
    }

    fn add_time_zone(&self, params: &mut Json) {
        if let Some(tz) = &self.options.time_zone {
            params["time_zone"] = json!(tz);
        }
    }
}

fn full_name(name: &str, nested: Option<&str>) -> String {
    match nested {
        Some(path) => format!("{path}.{name}"),
        None => name.to_string(),
    }
}

fn should(queries: Vec<Json>) -> Json {
    json!({ "bool": { "should": queries, "minimum_should_match": 1 } })
}

#[cfg(test)]
mod tests;
