use serde_json::json;

use super::*;

fn fields() -> Vec<Field> {
    vec![
        Field::new("team", "keyword"),
        Field::new("result", "keyword"),
        Field::new("message", "text"),
        Field::new("lines_changed", "float"),
        Field::new("deployed_at", "date"),
        Field::new("ai_assisted", "boolean"),
        Field::new("user.name", "keyword"),
        Field::new("user.email", "keyword"),
    ]
}

fn dsl(query: &str) -> serde_json::Value {
    translate(query, &fields(), &Options::default()).unwrap_or_else(|e| panic!("{query:?}: {e}"))
}

fn should(q: serde_json::Value) -> serde_json::Value {
    json!({ "bool": { "should": [q], "minimum_should_match": 1 } })
}

#[test]
fn empty_query_matches_everything() {
    assert_eq!(dsl(""), json!({ "match_all": {} }));
    assert_eq!(dsl("   "), json!({ "match_all": {} }));
}

#[test]
fn field_types_choose_the_query() {
    assert_eq!(
        dsl("team: checkout"),
        should(json!({ "term": { "team": { "value": "checkout" } } }))
    );
    assert_eq!(
        dsl("message: deploy failed"),
        should(json!({ "match": { "message": "deploy failed" } }))
    );
    assert_eq!(
        dsl("message: \"deploy failed\""),
        should(json!({ "match_phrase": { "message": "deploy failed" } }))
    );
    assert_eq!(
        dsl("deployed_at: 2026-09-01"),
        should(json!({ "range": { "deployed_at": { "gte": "2026-09-01", "lte": "2026-09-01" } } }))
    );
    assert_eq!(
        dsl("ai_assisted: true"),
        should(json!({ "match": { "ai_assisted": true } }))
    );
}

#[test]
fn quoting_the_value_does_not_change_keyword_matching() {
    assert_eq!(dsl("result : \"failure\""), dsl("result: failure"));
}

#[test]
fn wildcards() {
    assert_eq!(
        dsl("team: *"),
        should(json!({ "exists": { "field": "team" } }))
    );
    assert_eq!(
        dsl("team: check*"),
        should(json!({ "wildcard": { "team": { "value": "check*" } } }))
    );
    assert_eq!(
        dsl("message: dep-loy*"),
        should(json!({ "query_string": { "fields": ["message"], "query": "dep\\-loy*" } }))
    );
    // A wildcard field name expands to every matching field.
    assert_eq!(
        dsl("user.*: alice"),
        json!({ "bool": { "should": [
            { "term": { "user.name": { "value": "alice" } } },
            { "term": { "user.email": { "value": "alice" } } }
        ], "minimum_should_match": 1 } })
    );
    assert_eq!(dsl("*: *"), json!({ "match_all": {} }));
}

#[test]
fn ranges_keep_the_value_as_written() {
    assert_eq!(
        dsl("lines_changed < 100"),
        should(json!({ "range": { "lines_changed": { "lt": "100" } } }))
    );
    assert_eq!(
        dsl("lines_changed>=500"),
        should(json!({ "range": { "lines_changed": { "gte": "500" } } }))
    );
}

#[test]
fn boolean_operators_and_precedence() {
    let a = should(json!({ "term": { "team": { "value": "a" } } }));
    let b = should(json!({ "term": { "team": { "value": "b" } } }));
    let c = should(json!({ "term": { "result": { "value": "c" } } }));
    // AND binds tighter than OR.
    assert_eq!(
        dsl("team: a or team: b and result: c"),
        json!({ "bool": { "should": [a, { "bool": { "filter": [b, c] } }], "minimum_should_match": 1 } })
    );
    assert_eq!(dsl("NOT team: a"), json!({ "bool": { "must_not": a } }));
    assert_eq!(
        dsl("not (team: a)"),
        dsl("NOT team: a"),
        "keywords are case-insensitive"
    );
}

#[test]
fn value_lists_apply_the_field_to_each_value() {
    assert_eq!(
        dsl("team: (a or b)"),
        json!({ "bool": { "should": [
            should(json!({ "term": { "team": { "value": "a" } } })),
            should(json!({ "term": { "team": { "value": "b" } } }))
        ], "minimum_should_match": 1 } })
    );
    assert_eq!(
        dsl("team: (not a)"),
        json!({ "bool": { "must_not": should(json!({ "term": { "team": { "value": "a" } } })) } })
    );
}

#[test]
fn values_without_a_field_search_everything() {
    assert_eq!(
        dsl("timeout"),
        json!({ "multi_match": { "type": "best_fields", "query": "timeout", "lenient": true } })
    );
    assert_eq!(
        dsl("\"time out\""),
        json!({ "multi_match": { "type": "phrase", "query": "time out", "lenient": true } })
    );
    assert_eq!(
        dsl("time*"),
        json!({ "query_string": { "query": "time*" } })
    );
}

#[test]
fn unquoted_values_may_contain_spaces_but_stop_at_keywords() {
    assert_eq!(
        dsl("message: in progress"),
        should(json!({ "match": { "message": "in progress" } }))
    );
    assert!(matches!(parse("message: a or b").unwrap(), Node::Or(_)));
}

#[test]
fn escapes() {
    assert_eq!(
        dsl("message: a\\:b"),
        should(json!({ "match": { "message": "a:b" } }))
    );
    assert_eq!(
        dsl("message: \\*"),
        should(json!({ "match": { "message": "*" } })),
        "escaped * is literal"
    );
    assert_eq!(
        dsl("message: \"say \\\"hi\\\"\""),
        should(json!({ "match_phrase": { "message": "say \"hi\"" } }))
    );
    assert_eq!(
        dsl("message: \\u0041"),
        should(json!({ "match": { "message": "A" } }))
    );
    assert_eq!(
        dsl("message: \\or"),
        should(json!({ "match": { "message": "or" } })),
        "escaped keyword"
    );
}

#[test]
fn nested_queries_prefix_their_fields() {
    let mut fields = fields();
    fields.push(Field::new("items.sku", "keyword"));
    let q = translate("items: { sku: abc }", &fields, &Options::default()).unwrap();
    assert_eq!(
        q,
        json!({ "nested": {
            "path": "items",
            "query": should(json!({ "term": { "items.sku": { "value": "abc" } } })),
            "score_mode": "none"
        } })
    );
}

#[test]
fn unknown_fields_are_used_as_written() {
    assert_eq!(
        translate("nope: x", &[], &Options::default()).unwrap(),
        should(json!({ "match": { "nope": "x" } }))
    );
}

#[test]
fn date_matches_carry_the_time_zone() {
    let options = Options {
        time_zone: Some("Europe/London".into()),
    };
    assert_eq!(
        translate("deployed_at >= 2026-09-01", &fields(), &options).unwrap(),
        should(
            json!({ "range": { "deployed_at": { "gte": "2026-09-01", "time_zone": "Europe/London" } } })
        )
    );
}

#[test]
fn syntax_errors_say_where() {
    let err = parse("team: (a or").unwrap_err();
    assert_eq!(err.position, 11, "{err}");
    assert!(parse("team:").is_err());
    assert!(parse("(team: a").is_err());
    assert!(parse("team: a)").is_err());
}

#[test]
fn fixture_queries_parse() {
    for q in [
        "is_rework : false",
        "result : \"failure\"",
        "caused_by_deployment_id : *",
        "NOT status : \"todo\"",
        "lines_changed >= 100 and lines_changed < 500",
        "ai_assisted : true",
        "state : \"merged\"",
        "products.product_name:*trouser*",
    ] {
        parse(q).unwrap_or_else(|e| panic!("{q:?}: {e}"));
    }
}
