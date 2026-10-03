//! The agent skill's cookbook (`skills/teleplot/references/cookbook.md`)
//! must stay correct: every `yaml` block is a panel that parses and lints
//! cleanly against the delivery fixtures' fields.

#![cfg(feature = "ssr")]

use std::collections::HashMap;

use serde_json::{Value, json};
use teleplot::kql::Field;
use teleplot::lint::{self, Severity};

fn root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

fn field_caps() -> HashMap<String, Vec<Field>> {
    [
        "delivery-prs",
        "delivery-deployments",
        "delivery-incidents",
        "delivery-workitems",
    ]
    .into_iter()
    .map(|index| {
        let text = std::fs::read_to_string(format!("{}/fixtures/field-caps/{index}.json", root()))
            .unwrap();
        let caps: Value = serde_json::from_str(&text).unwrap();
        (index.to_string(), teleplot::es::fields_from_caps(&caps))
    })
    .collect()
}

/// The `yaml` code blocks of a Markdown file.
fn yaml_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match (&mut current, line.trim_start()) {
            (None, "```yaml") => current = Some(String::new()),
            (Some(block), "```") => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            (Some(block), _) => {
                block.push_str(line);
                block.push('\n');
            }
            _ => {}
        }
    }
    blocks
}

#[test]
fn every_cookbook_panel_lints_cleanly() {
    let text =
        std::fs::read_to_string(format!("{}/skills/teleplot/references/cookbook.md", root()))
            .unwrap();
    let blocks = yaml_blocks(&text);
    assert!(blocks.len() >= 12, "found {} recipes", blocks.len());
    let fields = field_caps();
    for block in blocks {
        let panel: Value =
            serde_yaml_ng::from_str(&block).unwrap_or_else(|e| panic!("not YAML: {e}\n{block}"));
        let id = panel["id"].as_str().unwrap_or("?").to_string();
        let dashboard = json!({ "id": "cookbook", "title": "Cookbook", "panels": [panel] });
        let parsed = lint::parse(dashboard).unwrap_or_else(|p| panic!("{id}: {p:?}"));
        let errors: Vec<_> = lint::check(&parsed, Some(&fields))
            .into_iter()
            .filter(|p| p.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{id}: {errors:#?}");
    }
}
