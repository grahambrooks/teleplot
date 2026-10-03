//! A layer's data as a plain table: labelled columns and rows. Shared by the
//! MCP preview and the browser's CSV download, so both describe a panel's
//! numbers the same way.

use super::{Cell, Layer, LayerData, Operation};

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: Vec<TableColumn>,
    /// Row-major: one cell per column.
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableColumn {
    pub id: String,
    /// The column's label, or its id when it has none.
    pub label: String,
    /// Values are epoch milliseconds.
    pub date: bool,
}

impl TableColumn {
    /// A cell as text: dates through `date`, numbers in full precision,
    /// empty cells as nothing.
    pub fn text(&self, cell: &Cell, date: &dyn Fn(f64) -> String) -> String {
        match cell {
            Cell::Number(n) if self.date => date(*n),
            Cell::Number(n) => n.to_string(),
            Cell::Text(t) => t.clone(),
            Cell::Null => String::new(),
        }
    }
}

/// The drawn columns of a layer (helpers are not in its data), in order.
pub fn layer_table(layer: &Layer, data: &LayerData) -> Table {
    let columns = data
        .columns
        .iter()
        .map(|s| {
            let column = layer.column(&s.id);
            TableColumn {
                id: s.id.clone(),
                label: column
                    .and_then(|c| c.label.clone())
                    .unwrap_or_else(|| s.id.clone()),
                date: data.dates.contains(&s.id)
                    || match column.map(|c| &c.op) {
                        Some(Operation::DateHistogram { .. }) => true,
                        Some(Operation::Value { field }) => {
                            layer.data_view.time_field.as_ref() == Some(field)
                        }
                        _ => false,
                    },
            }
        })
        .collect();
    let rows = (0..data.rows)
        .map(|r| {
            data.columns
                .iter()
                .map(|s| s.values.get(r).cloned().unwrap_or(Cell::Null))
                .collect()
        })
        .collect();
    Table { columns, rows }
}

/// RFC 4180 CSV, with a header row of column labels.
pub fn csv(table: &Table, date: &dyn Fn(f64) -> String) -> String {
    let mut out = String::new();
    let line = |out: &mut String, fields: Vec<String>| {
        let quoted: Vec<String> = fields.iter().map(|f| quote(f)).collect();
        out.push_str(&quoted.join(","));
        out.push_str("\r\n");
    };
    line(
        &mut out,
        table.columns.iter().map(|c| c.label.clone()).collect(),
    );
    for row in &table.rows {
        line(
            &mut out,
            table
                .columns
                .iter()
                .zip(row)
                .map(|(c, cell)| c.text(cell, date))
                .collect(),
        );
    }
    out
}

fn quote(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, DataView, Encoding, Interval, Series};

    fn col(id: &str, label: Option<&str>, op: Operation) -> Column {
        Column {
            id: id.into(),
            label: label.map(String::from),
            op,
            filter: None,
            time_shift: None,
            per: None,
            format: None,
            helper: false,
            thresholds: Vec::new(),
        }
    }

    #[test]
    fn csv_labels_columns_formats_dates_and_quotes_text() {
        let layer = Layer {
            id: "l".into(),
            data_view: DataView {
                index: "i".into(),
                time_field: Some("t".into()),
            },
            columns: vec![
                col(
                    "x",
                    Some("Week"),
                    Operation::DateHistogram {
                        field: "t".into(),
                        interval: Interval::Auto,
                        include_empty: false,
                    },
                ),
                col("team", None, Operation::Count { field: None }),
                col(
                    "n",
                    Some("Deploys, total"),
                    Operation::Count { field: None },
                ),
            ],
            encoding: Encoding::Table {
                columns: vec!["x".into(), "team".into(), "n".into()],
                styles: Vec::new(),
                sparkline: None,
            },
            documents: None,
            esql: None,
        };
        let data = LayerData {
            layer: "l".into(),
            rows: 2,
            columns: vec![
                Series {
                    id: "x".into(),
                    values: vec![Cell::Number(0.0), Cell::Number(1.0)],
                },
                Series {
                    id: "team".into(),
                    values: vec![Cell::Text("a \"b\"".into()), Cell::Text("c".into())],
                },
                Series {
                    id: "n".into(),
                    values: vec![Cell::Number(2.5), Cell::Null],
                },
            ],
            counts: Vec::new(),
            error: None,
            dates: Vec::new(),
        };
        let table = layer_table(&layer, &data);
        assert_eq!(
            csv(&table, &|ms| format!("day{ms}")),
            "Week,team,\"Deploys, total\"\r\nday0,\"a \"\"b\"\"\",2.5\r\nday1,c,\r\n"
        );
    }
}
