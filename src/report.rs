//! `teleplot check`: what Teleplot can and cannot render in a set of dashboards.

use std::fmt::Write;

use crate::model::{Dashboard, PanelContent, PlaceholderReason};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub panels: usize,
    pub rendered: usize,
    pub out_of_scope: usize,
    pub broken: usize,
    pub unparseable: usize,
}

impl Counts {
    pub fn of(dashboard: &Dashboard) -> Self {
        let mut c = Counts::default();
        for panel in &dashboard.panels {
            c.panels += 1;
            match &panel.content {
                PanelContent::Chart(_) | PanelContent::Markdown { .. } => c.rendered += 1,
                PanelContent::Placeholder(p) => match p.reason {
                    PlaceholderReason::OutOfScope => c.out_of_scope += 1,
                    PlaceholderReason::BrokenReference => c.broken += 1,
                    PlaceholderReason::Unparseable => c.unparseable += 1,
                },
            }
        }
        c
    }

    fn add(&mut self, other: Counts) {
        self.panels += other.panels;
        self.rendered += other.rendered;
        self.out_of_scope += other.out_of_scope;
        self.broken += other.broken;
        self.unparseable += other.unparseable;
    }
}

/// A human-readable report, and the totals across all dashboards.
/// Panels Teleplot should render but could not (`unparseable`) are the
/// ones that matter: they are Teleplot bugs or unknown export variants.
pub fn check(dashboards: &[Dashboard]) -> (String, Counts) {
    let mut out = String::new();
    let mut total = Counts::default();
    for d in dashboards {
        let c = Counts::of(d);
        total.add(c);
        let _ = writeln!(out, "  {} ({} panels)", d.title, c.panels);
        let _ = writeln!(out, "    {}", summary(c));
        for panel in &d.panels {
            if let PanelContent::Placeholder(p) = &panel.content {
                let reason = match p.reason {
                    PlaceholderReason::OutOfScope => "not supported",
                    PlaceholderReason::BrokenReference => "broken reference",
                    PlaceholderReason::Unparseable => "COULD NOT CONVERT",
                };
                let title = panel.title.as_deref().unwrap_or("(untitled)");
                let _ = writeln!(out, "    - {title} [{}] {reason}: {}", p.kind, p.detail);
            }
        }
    }
    (out, total)
}

pub fn summary(c: Counts) -> String {
    let mut parts = vec![format!("{} of {} panels render", c.rendered, c.panels)];
    if c.out_of_scope > 0 {
        parts.push(format!("{} not supported", c.out_of_scope));
    }
    if c.broken > 0 {
        parts.push(format!("{} broken references", c.broken));
    }
    if c.unparseable > 0 {
        parts.push(format!("{} could not be converted", c.unparseable));
    }
    parts.join(", ")
}
