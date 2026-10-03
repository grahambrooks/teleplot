//! The MCP endpoint, exercised over HTTP the way agents use it.
//!
//! Tests that need Elasticsearch run when `TELEPLOT_TEST_ES` is set (as in
//! tests/elasticsearch.rs); the rest always run.

#![cfg(feature = "ssr")]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use teleplot::config::Config;
use teleplot::server::Teleplot;

struct Server {
    url: String,
    dir: PathBuf,
}

/// A Teleplot over a fresh dashboards directory holding the reference dashboard.
/// `advertised` is the address the router believes it serves; anything but
/// loopback makes the endpoint read-only.
async fn start(name: &str, es: Option<&str>, advertised: Option<SocketAddr>) -> Server {
    let dir = std::env::temp_dir().join(format!("teleplot-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        format!(
            "{}/dashboards/delivery-overview.yaml",
            env!("CARGO_MANIFEST_DIR")
        ),
        dir.join("delivery-overview.yaml"),
    )
    .unwrap();
    let config = match es {
        Some(url) => Config::parse(
            &format!("elasticsearch:\n  url: {url}\ntime_zone: UTC\n"),
            |_| None,
        ),
        None => Err("no Elasticsearch in this test".into()),
    };
    let app = Arc::new(Teleplot::new(config, Some(dir.clone())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = teleplot::server::router(advertised.unwrap_or(addr), app);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Server {
        url: format!("http://{addr}/mcp"),
        dir,
    }
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "teleplot-tests", "version": "1" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

/// A stateless (2026-07-28) JSON-RPC request.
async fn rpc(server: &Server, method: &str, name: Option<&str>, mut params: Value) -> Value {
    params["_meta"] = meta();
    let mut request = reqwest::Client::new()
        .post(&server.url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method);
    if let Some(name) = name {
        request = request.header("mcp-name", name);
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let response = request.json(&body).send().await.unwrap();
    assert_eq!(response.status(), 200, "{method}");
    let value: Value = response.json().await.unwrap();
    assert!(value.get("error").is_none(), "{method}: {value}");
    value["result"].clone()
}

/// Calls a tool and returns its structured result, checked against its output schema.
async fn call(server: &Server, tool: &str, arguments: Value) -> Value {
    let result = rpc(
        server,
        "tools/call",
        Some(tool),
        json!({ "name": tool, "arguments": arguments }),
    )
    .await;
    assert_eq!(result["isError"], json!(false), "{tool}: {result}");
    // Clients such as Claude Code reject results that don't match the
    // declared output schema, so every call checks it.
    let tools = rpc(server, "tools/list", None, json!({})).await;
    let schema = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == tool)
        .unwrap()["outputSchema"]
        .clone();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let content = result["structuredContent"].clone();
    let errors: Vec<String> = validator
        .iter_errors(&content)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{tool} result does not match its output schema: {errors:?}"
    );
    content
}

fn reference() -> Value {
    let path = format!(
        "{}/dashboards/delivery-overview.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::to_value(
        teleplot::dashboards::load_file(path.as_ref())
            .unwrap()
            .remove(0),
    )
    .unwrap()
}

/// A small dashboard an agent might write: change fail rate by team.
fn cfr_by_team(id: &str) -> Value {
    json!({
        "id": id,
        "title": "Change fail rate by team",
        "time": { "from": "2026-04-01T00:00:00Z", "to": "2026-10-02T00:00:00Z" },
        "panels": [{
            "id": "cfr",
            "title": "Change fail rate by team",
            "grid": { "x": 0, "y": 0, "w": 48, "h": 12 },
            "content": {
                "type": "chart", "kind": "table",
                "query": { "language": "kql", "text": "is_rework : false" },
                "layers": [{
                    "id": "l",
                    "data_view": { "index": "delivery-deployments", "time_field": "deployed_at" },
                    "columns": [
                        { "id": "team", "label": "Team", "op": "terms", "fields": ["team"], "size": 20, "order": { "by": "column", "column": "n" } },
                        { "id": "n", "label": "Deployments", "op": "count" },
                        { "id": "failed", "op": "count", "filter": { "language": "kql", "text": "result : failure" }, "helper": true },
                        { "id": "rate", "label": "Change fail rate", "op": "formula", "text": "failed / n",
                          "expr": { "call": "divide", "args": ["failed", "n"] }, "format": { "kind": "percent", "decimals": 1 },
                          "thresholds": [{ "label": "Elite", "to": 0.05 }, { "label": "High", "from": 0.05, "to": 0.10 },
                                         { "label": "Medium", "from": 0.10, "to": 0.15 }, { "label": "Low", "from": 0.15 }] }
                    ],
                    "encoding": { "as": "table", "columns": ["team", "n", "rate"] }
                }]
            }
        }]
    })
}

#[tokio::test]
async fn lists_seven_tools_with_schemas_and_hints() {
    let server = start("tools", None, None).await;
    let tools = rpc(&server, "tools/list", None, json!({})).await;
    let tools = tools["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 7, "{names:?}");
    for tool in tools {
        assert!(tool["outputSchema"].is_object(), "{}", tool["name"]);
        let read_only = tool["annotations"]["readOnlyHint"] == json!(true);
        assert_eq!(
            read_only,
            tool["name"] != "teleplot_save_dashboard",
            "{}",
            tool["name"]
        );
    }
    // The dashboard parameter carries the full dashboard schema, so agents see the format.
    let save = tools
        .iter()
        .find(|t| t["name"] == "teleplot_save_dashboard")
        .unwrap();
    assert!(
        save["inputSchema"].to_string().contains("panels"),
        "{}",
        save["inputSchema"]
    );
}

#[tokio::test]
async fn legacy_clients_still_work() {
    let server = start("legacy", None, None).await;
    let client = reqwest::Client::new();
    let init = client
        .post(&server.url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "old", "version": "1" } } }))
        .send()
        .await
        .unwrap();
    assert_eq!(init.status(), 200);
    assert!(
        init.headers().get("mcp-session-id").is_some(),
        "pre-2026 clients get a session"
    );
    let body = init.text().await.unwrap();
    assert!(body.contains("\"name\":\"teleplot\""), "{body}");
}

#[tokio::test]
async fn foreign_origins_are_refused() {
    let server = start("origin", None, None).await;
    let response = reqwest::Client::new()
        .post(&server.url)
        .header("origin", "http://evil.example")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

#[tokio::test]
async fn resources_carry_the_format() {
    let server = start("resources", None, None).await;
    let list = rpc(&server, "resources/list", None, json!({})).await;
    assert_eq!(list["resources"].as_array().unwrap().len(), 2);
    let schema = rpc(
        &server,
        "resources/read",
        // 2026-07-28 requires Mcp-Name on resources/read too: the URI.
        Some("teleplot://schema/dashboard"),
        json!({ "uri": "teleplot://schema/dashboard" }),
    )
    .await;
    let text = schema["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"panels\""), "{text}");
    // 2026-07-28 clients (e.g. Claude Code) require cache hints on resource results.
    assert!(
        schema["ttlMs"].is_number() && list["ttlMs"].is_number(),
        "{schema}"
    );
    let docs = rpc(
        &server,
        "resources/read",
        // 2026-07-28 requires Mcp-Name on resources/read too: the URI.
        Some("teleplot://docs/dashboards"),
        json!({ "uri": "teleplot://docs/dashboards" }),
    )
    .await;
    assert!(
        docs["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("# Teleplot dashboards")
    );
}

#[tokio::test]
async fn validation_names_the_exact_problem() {
    let server = start("validate", None, None).await;
    let ok = call(
        &server,
        "teleplot_validate_dashboard",
        json!({ "dashboard": reference() }),
    )
    .await;
    assert_eq!(ok["valid"], json!(true), "{ok}");
    assert!(
        ok["note"].as_str().unwrap().contains("fields not checked"),
        "offline: {ok}"
    );

    let mut broken = reference();
    broken["panels"][2]["content"]["layers"][0]["columns"][2]["expr"]["args"][0] = json!("nope");
    let bad = call(
        &server,
        "teleplot_validate_dashboard",
        json!({ "dashboard": broken }),
    )
    .await;
    assert_eq!(bad["valid"], json!(false));
    assert_eq!(
        bad["problems"][0]["path"],
        json!("panels[2].content.layers[0].columns[2]")
    );
    assert!(
        bad["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("\"nope\"")
    );
}

#[tokio::test]
async fn saves_new_dashboards_and_protects_existing_ones() {
    let server = start("save", None, None).await;

    // People's dashboards are not the agent's to replace.
    let mut theirs = reference();
    theirs["title"] = json!("Changed");
    let refused = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": theirs }),
    )
    .await;
    assert_eq!(refused["saved"], json!(false));
    assert!(
        refused["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("delivery-overview-copy"),
        "{refused}"
    );

    // A new dashboard lands in agents/ and is served at once.
    let saved = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": cfr_by_team("cfr-by-team") }),
    )
    .await;
    assert_eq!(saved["saved"], json!(true), "{saved}");
    assert_eq!(saved["created"], json!(true));
    assert!(server.dir.join("agents/cfr-by-team.yaml").exists());
    let list = call(&server, "teleplot_list_dashboards", json!({})).await;
    let entry = list["dashboards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "cfr-by-team")
        .cloned()
        .unwrap();
    assert_eq!(entry["source"], json!("agent"));
    let api = reqwest::get(server.url.replace("/mcp", "/api/dashboards/cfr-by-team"))
        .await
        .unwrap();
    assert_eq!(api.status(), 200, "the web UI sees it");

    // Replacing needs the hash of what was read; a stale one is refused.
    let mut v2 = cfr_by_team("cfr-by-team");
    v2["title"] = json!("CFR by team, v2");
    let blind = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": v2 }),
    )
    .await;
    assert!(
        blind["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("expected_hash"),
        "{blind}"
    );
    let read = call(
        &server,
        "teleplot_get_dashboard",
        json!({ "id": "cfr-by-team" }),
    )
    .await;
    assert_eq!(
        read["panels"][0]["shows"],
        json!(["Team", "Deployments", "Change fail rate"])
    );
    let replaced = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": v2, "expected_hash": read["hash"] }),
    )
    .await;
    assert_eq!(replaced["saved"], json!(true), "{replaced}");
    assert_eq!(replaced["created"], json!(false));
    let stale = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": v2, "expected_hash": read["hash"] }),
    )
    .await;
    assert!(
        stale["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("changed since you read it"),
        "{stale}"
    );
    assert_eq!(
        std::fs::read_dir(server.dir.join("agents/.history/cfr-by-team"))
            .unwrap()
            .count(),
        1
    );

    // Invalid dashboards are never written.
    let mut invalid = cfr_by_team("broken");
    invalid["panels"][0]["grid"]["x"] = json!(30);
    let no = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": invalid }),
    )
    .await;
    assert_eq!(no["saved"], json!(false));
    assert!(!server.dir.join("agents/broken.yaml").exists());
}

#[tokio::test]
async fn writes_are_refused_unless_teleplot_is_local() {
    let server = start("remote", None, Some("192.0.2.10:3000".parse().unwrap())).await;
    let result = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": cfr_by_team("x") }),
    )
    .await;
    assert_eq!(result["saved"], json!(false));
    assert!(
        result["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("localhost"),
        "{result}"
    );
}

// ---------------------------------------------------------------- with Elasticsearch

fn cluster() -> Option<String> {
    let spec = std::env::var("TELEPLOT_TEST_ES").ok()?;
    let url = spec
        .split(',')
        .next()?
        .split_once('=')?
        .1
        .trim()
        .to_string();
    Some(url)
}

#[tokio::test]
async fn an_agent_can_discover_check_preview_and_save() {
    let Some(es) = cluster() else {
        eprintln!("TELEPLOT_TEST_ES not set: skipping");
        return;
    };
    let server = start("agent", Some(&es), None).await;

    let indices = call(&server, "teleplot_list_indices", json!({})).await;
    assert!(
        indices["indices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["name"] == "delivery-deployments")
    );

    let described = call(
        &server,
        "teleplot_describe_index",
        json!({ "index": "delivery-deployments" }),
    )
    .await;
    assert_eq!(described["time_fields"], json!(["deployed_at"]));
    let result = described["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "result")
        .unwrap();
    let values: Vec<&str> = result["top_values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v[0].as_str().unwrap())
        .collect();
    assert!(values.contains(&"failure"), "{values:?}");
    assert!(described["sample"].is_object());

    // Online validation knows the fields.
    let mut typo = cfr_by_team("typo");
    typo["panels"][0]["content"]["layers"][0]["columns"][0]["fields"] = json!(["teams"]);
    let checked = call(
        &server,
        "teleplot_validate_dashboard",
        json!({ "dashboard": typo }),
    )
    .await;
    assert!(
        checked["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("did you mean team"),
        "{checked}"
    );

    // The preview shows the same numbers Kibana does (tests/elasticsearch.rs).
    let preview = call(
        &server,
        "teleplot_preview",
        json!({ "dashboard": cfr_by_team("cfr") }),
    )
    .await;
    let layer = &preview["panels"][0]["layers"][0];
    assert_eq!(
        layer["columns"],
        json!(["Team", "Deployments", "Change fail rate"])
    );
    assert_eq!(layer["rows"][0][0], json!("checkout"));
    assert_eq!(layer["rows"][0][1], json!(395.0));
    let rate = layer["rows"][0][2].as_f64().unwrap();
    assert_eq!((rate * 1000.0).round() / 10.0, 4.3);

    let saved = call(
        &server,
        "teleplot_save_dashboard",
        json!({ "dashboard": cfr_by_team("cfr") }),
    )
    .await;
    assert_eq!(saved["saved"], json!(true), "{saved}");
    let data: Value = reqwest::Client::new()
        .post(server.url.replace("/mcp", "/api/dashboards/cfr/data"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        data["panels"]["cfr"]["layers"][0]["rows"],
        json!(8),
        "the saved dashboard serves real data"
    );
}
