//! Panel-level edits to a hand-written YAML dashboard, for agents working
//! through MCP on a dashboards directory under version control.
//!
//! Rewriting a whole dashboard from its model would drop every comment and
//! reformat every panel, so a one-panel change would show as a whole-file
//! diff. Instead one panel's lines are replaced (or a panel is appended), and
//! every other line, comments included, stays byte for byte. The result is
//! parsed again and must equal the intended dashboard, or the edit is refused.

use serde::Deserialize;

use crate::model::{Dashboard, Panel};

/// An edited file.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub text: String,
    /// The changed lines, `-` before and `+` after, with their line numbers.
    pub diff: String,
    /// The panel was new, and appended.
    pub added: bool,
}

/// Where a panel's lines are in the file, as line indexes `[start, end)`.
struct Span {
    start: usize,
    end: usize,
}

/// Replace panel `panel.id` in `text`, or append it to the panels when no
/// panel has that id. With the `old` panel, only the fields that differ are
/// rewritten.
pub fn replace_panel(text: &str, panel: &Panel, old: Option<&Panel>) -> Result<Edit, String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.iter().any(|l| l.trim_end() == "---") {
        return Err("the file holds more than one YAML document; edit it by hand".into());
    }
    let panels_at = lines
        .iter()
        .position(|l| l.trim_end() == "panels:")
        .ok_or("no top-level `panels:` list in the file")?;

    // The panels list ends at the next top-level key.
    let list_end = (panels_at + 1..lines.len())
        .find(|&i| {
            let l = lines[i];
            !l.is_empty() && !l.starts_with(' ') && !l.starts_with('#') && !l.starts_with('-')
        })
        .unwrap_or(lines.len());

    // Items: lines that open a list entry at the list's indentation.
    let indent = (panels_at + 1..list_end)
        .find_map(|i| {
            let l = lines[i];
            let trimmed = l.trim_start();
            trimmed.starts_with("- ").then(|| l.len() - trimmed.len())
        })
        .unwrap_or(2);
    let opener = format!("{}- ", " ".repeat(indent));
    let starts: Vec<usize> = (panels_at + 1..list_end)
        .filter(|&i| lines[i].starts_with(&opener))
        .collect();

    let item_id = |start: usize, end: usize| -> Option<String> {
        let own = format!("{}  id:", " ".repeat(indent));
        (start..end).find_map(|i| {
            let l = lines[i];
            let value = if i == start {
                l.strip_prefix(&opener)?.strip_prefix("id:")
            } else {
                l.strip_prefix(&own)
            }?;
            Some(
                value
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string(),
            )
        })
    };

    let mut spans = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let next = starts.get(n + 1).copied().unwrap_or(list_end);
        // Blank lines and comments before the next panel belong to it.
        let mut end = next;
        while end > start + 1 && {
            let t = lines[end - 1].trim();
            t.is_empty() || t.starts_with('#')
        } {
            end -= 1;
        }
        if let Some(id) = item_id(start, end) {
            spans.push((id, Span { start, end }));
        }
    }

    let found = spans.iter().find(|(id, _)| id == &panel.id);
    if let Some((_, span)) = found
        && lines[span.start..span.end]
            .iter()
            .any(|l| defines_anchor(l))
    {
        return Err(format!(
            "panel {} defines a YAML anchor (&name) that other panels may refer to; edit it by hand",
            panel.id
        ));
    }

    // Each change: replace lines [start, end) with these.
    let mut changes: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let added = found.is_none();
    match (found, old) {
        (Some((_, span)), Some(old)) => {
            changes = key_changes(&lines, span, indent, old, panel)?;
        }
        (Some((_, span)), None) => {
            changes.push((span.start, span.end, panel_lines(panel, indent)?));
        }
        (None, _) => {
            // After the last panel, ahead of trailing comments.
            let at = spans.last().map_or(panels_at + 1, |(_, s)| s.end);
            let mut new_lines = panel_lines(panel, indent)?;
            if at > panels_at + 1 {
                new_lines.insert(0, String::new());
            }
            changes.push((at, at, new_lines));
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut diff = String::new();
    let mut at = 0;
    for (start, end, new_lines) in &changes {
        out.extend(lines[at..*start].iter().map(|l| l.to_string()));
        out.extend(new_lines.iter().cloned());
        diff.push_str(&format!("@@ line {} @@\n", start + 1));
        for l in &lines[*start..*end] {
            diff.push_str(&format!("-{l}\n"));
        }
        for l in new_lines {
            diff.push_str(&format!("+{l}\n"));
        }
        at = *end;
    }
    out.extend(lines[at..].iter().map(|l| l.to_string()));
    let mut text_out = out.join("\n");
    if text.ends_with('\n') {
        text_out.push('\n');
    }
    Ok(Edit {
        text: text_out,
        diff,
        added,
    })
}

/// The changes that turn `old` into `new` in a panel's lines, one field at a
/// time: an unchanged field keeps its text exactly, and only changed,
/// added or removed fields are rewritten.
fn key_changes(
    lines: &[&str],
    span: &Span,
    indent: usize,
    old: &Panel,
    new: &Panel,
) -> Result<Vec<(usize, usize, Vec<String>)>, String> {
    let as_map = |p: &Panel| match serde_yaml_ng::to_value(p) {
        Ok(serde_yaml_ng::Value::Mapping(m)) => Ok(m),
        Ok(_) => Err("a panel serialised to something other than a mapping".to_string()),
        Err(e) => Err(e.to_string()),
    };
    let (old_map, new_map) = (as_map(old)?, as_map(new)?);

    // Where each field's lines are: from its key to the next field's key.
    let pad = " ".repeat(indent + 2);
    let key_of = |i: usize| -> Option<String> {
        let rest = if i == span.start {
            lines[i].trim_start().strip_prefix("- ")?
        } else {
            lines[i].strip_prefix(&pad)?
        };
        let key: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        (!key.is_empty() && rest[key.len()..].starts_with(':')).then_some(key)
    };
    let starts: Vec<(usize, String)> = (span.start..span.end)
        .filter_map(|i| key_of(i).map(|k| (i, k)))
        .collect();
    let field_span = |key: &str| -> Option<(usize, usize)> {
        let n = starts.iter().position(|(_, k)| k == key)?;
        let start = starts[n].0;
        let mut end = starts.get(n + 1).map_or(span.end, |(i, _)| *i);
        // A field's trailing blank lines stay with the panel's layout.
        while end > start + 1 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        Some((start, end))
    };
    let field_lines = |key: &serde_yaml_ng::Value, value: &serde_yaml_ng::Value| {
        let mut one = serde_yaml_ng::Mapping::new();
        one.insert(key.clone(), value.clone());
        serde_yaml_ng::to_string(&one).map(|yaml| {
            yaml.lines()
                .map(|l| {
                    if l.is_empty() {
                        String::new()
                    } else {
                        format!("{pad}{l}")
                    }
                })
                .collect::<Vec<_>>()
        })
    };

    let mut changes = Vec::new();
    for (key, value) in &new_map {
        let name = key.as_str().unwrap_or_default();
        if old_map.get(key) == Some(value) {
            continue;
        }
        let new_lines = field_lines(key, value).map_err(|e| e.to_string())?;
        match field_span(name) {
            // `id` opens the list item; it can't differ (it's how the panel was found).
            Some((start, _)) if start == span.start => {
                return Err(format!(
                    "can't rewrite the `{name}` that opens panel {}",
                    new.id
                ));
            }
            Some((start, end)) => changes.push((start, end, new_lines)),
            None => {
                let at = field_span(starts.last().map(|(_, k)| k.as_str()).unwrap_or(""))
                    .map_or(span.end, |(_, end)| end);
                changes.push((at, at, new_lines));
            }
        }
    }
    for key in old_map.keys() {
        if !new_map.contains_key(key)
            && let Some((start, end)) = field_span(key.as_str().unwrap_or_default())
        {
            changes.push((start, end, Vec::new()));
        }
    }
    changes.sort_by_key(|(start, end, _)| (*start, *end));
    Ok(changes)
}

/// Whether a line defines a YAML anchor (`&name`) outside a quoted string.
fn defines_anchor(line: &str) -> bool {
    let mut quote = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '#') if prev == ' ' => return false,
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '&')
                if matches!(prev, ' ' | ':' | '[' | '{' | ',' | '-')
                    && line[i + 1..]
                        .chars()
                        .next()
                        .is_some_and(|n| n.is_ascii_alphanumeric() || n == '_') =>
            {
                return true;
            }
            _ => {}
        }
        prev = c;
    }
    false
}

/// The panel as YAML list-item lines at `indent`.
fn panel_lines(panel: &Panel, indent: usize) -> Result<Vec<String>, String> {
    let yaml = serde_yaml_ng::to_string(panel).map_err(|e| e.to_string())?;
    let pad = " ".repeat(indent);
    Ok(yaml
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                format!("{pad}- {l}")
            } else if l.is_empty() {
                String::new()
            } else {
                format!("{pad}  {l}")
            }
        })
        .collect())
}

/// Parse a single-document YAML dashboard.
pub fn parse(text: &str) -> Result<Dashboard, String> {
    Dashboard::deserialize(serde_yaml_ng::Deserializer::from_str(text)).map_err(|e| e.to_string())
}

/// Apply `panel` to the dashboard in `text` and check the result: the file
/// must parse to `before` with that one panel replaced or appended.
pub fn edit_dashboard(
    text: &str,
    before: &Dashboard,
    panel: &Panel,
) -> Result<(Edit, Dashboard), String> {
    let old = before.panels.iter().find(|p| p.id == panel.id);
    let edit = replace_panel(text, panel, old)?;
    let mut expected = before.clone();
    match expected.panels.iter_mut().find(|p| p.id == panel.id) {
        Some(p) => *p = panel.clone(),
        None => expected.panels.push(panel.clone()),
    }
    let after = parse(&edit.text).map_err(|e| format!("the edited file doesn't parse: {e}"))?;
    if after != expected {
        return Err(
            "the edited file doesn't read back as the intended dashboard; edit it by hand".into(),
        );
    }
    Ok((edit, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# A dashboard with comments.
id: demo
title: Demo
time:
  from: now-90d/d
  to: now
panels:
  # ── First section ──
  - id: one
    title: One
    grid: { x: 0, y: 0, w: 24, h: 8 }
    content:
      type: markdown
      text: \"first\"

  # ── Second section ──
  - id: two
    title: Two
    grid: { x: 24, y: 0, w: 24, h: 8 }
    content:
      type: markdown
      text: \"second\"
";

    fn markdown(id: &str, text: &str) -> Panel {
        serde_yaml_ng::from_str(&format!(
            "id: {id}\ntitle: {id}\ngrid: {{ x: 0, y: 8, w: 48, h: 4 }}\ncontent:\n  type: markdown\n  text: {text}\n"
        ))
        .unwrap()
    }

    #[test]
    fn replacing_a_panel_keeps_every_other_line() {
        let before = parse(FILE).unwrap();
        let (edit, after) = edit_dashboard(FILE, &before, &markdown("one", "changed")).unwrap();
        assert!(!edit.added);
        assert_eq!(after.panels[0].title.as_deref(), Some("one"));
        assert_eq!(
            after.panels[1], before.panels[1],
            "the other panel is untouched"
        );
        // Comments, including the one heading the next panel, survive.
        assert!(edit.text.starts_with("# A dashboard with comments.\n"));
        assert!(edit.text.contains("  # ── First section ──\n  - id: one"));
        assert!(
            edit.text
                .contains("\n\n  # ── Second section ──\n  - id: two\n")
        );
        let tail = FILE.split("  # ── Second section ──").nth(1).unwrap();
        assert!(
            edit.text.ends_with(tail),
            "the second panel's lines are byte for byte the same"
        );
        assert!(edit.diff.contains("-    title: One") && edit.diff.contains("+    title: one"));
    }

    #[test]
    fn only_changed_fields_are_rewritten() {
        let before = parse(FILE).unwrap();
        let mut panel = before.panels[0].clone();
        panel.title = Some("One, renamed".into());
        let (edit, after) = edit_dashboard(FILE, &before, &panel).unwrap();
        assert_eq!(after.panels[0].title.as_deref(), Some("One, renamed"));
        let changed: Vec<&str> = edit
            .diff
            .lines()
            .filter(|l| l.starts_with(['-', '+']))
            .collect();
        assert_eq!(changed, ["-    title: One", "+    title: One, renamed"]);
        assert_eq!(
            edit.text,
            FILE.replace("    title: One\n", "    title: One, renamed\n"),
            "everything else byte for byte"
        );
    }

    #[test]
    fn a_new_panel_is_appended() {
        let before = parse(FILE).unwrap();
        let (edit, after) = edit_dashboard(FILE, &before, &markdown("three", "new")).unwrap();
        assert!(edit.added);
        assert_eq!(after.panels.len(), 3);
        assert_eq!(after.panels[2].id, "three");
        assert!(
            edit.text.starts_with(&FILE[..FILE.len() - 1]),
            "earlier lines unchanged"
        );
    }

    /// Every panel of the dashboards that ship with Teleplot survives being
    /// written back: serialisation round-trips, and the edit verifies.
    #[test]
    fn every_shipped_panel_round_trips() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dashboards");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "yaml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let before = parse(&text).unwrap();
            for panel in &before.panels {
                match edit_dashboard(&text, &before, panel) {
                    Ok(_) => checked += 1,
                    // Refused up front, rather than written and found broken.
                    Err(e) if e.contains("YAML anchor") => {}
                    Err(e) => panic!("{}: panel {}: {e}", path.display(), panel.id),
                }
            }
        }
        assert!(checked > 10, "only {checked} panels checked");
    }

    #[test]
    fn anchors_are_found_outside_strings_only() {
        assert!(defines_anchor("    columns: &lead-cols"));
        assert!(defines_anchor("  - &base { id: x }"));
        assert!(!defines_anchor("    text: \"a & b\""));
        assert!(!defines_anchor("    label: R&D # &not"));
    }

    #[test]
    fn several_documents_are_refused() {
        let two = format!("{FILE}---\n{FILE}");
        assert!(replace_panel(&two, &markdown("one", "x"), None).is_err());
    }
}
