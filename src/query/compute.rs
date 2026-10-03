//! Columns computed from other columns, evaluated over the flattened table.
//!
//! Pipeline functions partition rows by every bucket column except the date
//! histogram (Kibana's `by`) and walk them in table order, matching Kibana's
//! `moving_average_fn`, `cumulative_sum_fn`, `derivative_fn` and
//! `overall_metric_fn`, including how each treats empty cells.

use std::collections::HashMap;

use crate::model::{Cell, Expr, Layer, Operation, OverallFunction};

/// Fills in every computed column of `layer`. `buckets` are the bucket
/// columns with whether each is a date histogram.
pub fn computed_columns(
    layer: &Layer,
    buckets: &[(String, bool)],
    rows: usize,
    columns: &mut HashMap<String, Vec<Cell>>,
) -> Result<(), String> {
    let by: Vec<&str> = buckets
        .iter()
        .filter(|(_, date)| !date)
        .map(|(c, _)| c.as_str())
        .collect();
    let partition: Vec<String> = (0..rows)
        .map(|r| {
            by.iter()
                .map(|c| match columns.get(*c).map(|v| &v[r]) {
                    Some(Cell::Text(t)) => t.clone(),
                    Some(Cell::Number(n)) => n.to_string(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\u{1f}")
        })
        .collect();

    let mut pending: Vec<&crate::model::Column> = layer
        .columns
        .iter()
        .filter(|c| !c.op.inputs().is_empty())
        .collect();
    while !pending.is_empty() {
        let ready = pending
            .iter()
            .position(|c| c.op.inputs().iter().all(|i| columns.contains_key(*i)));
        let Some(index) = ready else {
            let ids: Vec<&str> = pending.iter().map(|c| c.id.as_str()).collect();
            return Err(format!(
                "columns {ids:?} depend on columns that do not exist or on each other"
            ));
        };
        let column = pending.remove(index);
        let values = match &column.op {
            Operation::Formula { expr, .. } => {
                (0..rows).map(|r| eval(expr, columns, r).into()).collect()
            }
            Operation::MovingAverage { of, window } => {
                moving_average(&numbers(&columns[of]), &partition, *window as usize)
            }
            Operation::CumulativeSum { of } => cumulative_sum(&numbers(&columns[of]), &partition),
            Operation::Differences { of } => differences(&numbers(&columns[of]), &partition),
            Operation::CounterRate { of } => counter_rate(&numbers(&columns[of]), &partition),
            Operation::Overall { of, function } => {
                overall(&numbers(&columns[of]), &partition, *function)
            }
            other => return Err(format!("{other:?} is not computed")),
        };
        columns.insert(column.id.clone(), values);
    }
    Ok(())
}

fn numbers(cells: &[Cell]) -> Vec<Option<f64>> {
    cells.iter().map(Cell::number).collect()
}

fn moving_average(values: &[Option<f64>], partition: &[String], window: usize) -> Vec<Cell> {
    let mut last: HashMap<&str, Vec<f64>> = HashMap::new();
    values
        .iter()
        .zip(partition)
        .map(|(value, key)| {
            let previous = last.get(key.as_str());
            let out = match (previous, value) {
                (Some(prev), Some(_)) if !prev.is_empty() => {
                    Some(prev.iter().sum::<f64>() / prev.len() as f64)
                }
                _ => None,
            };
            if let Some(v) = value {
                let entry = last.entry(key.as_str()).or_default();
                entry.push(*v);
                if entry.len() > window {
                    entry.remove(0);
                }
            }
            out.into()
        })
        .collect()
}

fn cumulative_sum(values: &[Option<f64>], partition: &[String]) -> Vec<Cell> {
    let mut sums: HashMap<&str, f64> = HashMap::new();
    values
        .iter()
        .zip(partition)
        .map(|(value, key)| {
            let sum = sums.entry(key.as_str()).or_insert(0.0);
            if let Some(v) = value {
                *sum += v;
            }
            Cell::Number(*sum)
        })
        .collect()
}

fn differences(values: &[Option<f64>], partition: &[String]) -> Vec<Cell> {
    let mut previous: HashMap<&str, Option<f64>> = HashMap::new();
    values
        .iter()
        .zip(partition)
        .map(|(value, key)| {
            let prev = previous.get(key.as_str()).copied().flatten();
            let out = match (value, prev) {
                (Some(v), Some(p)) => Some(v - p),
                _ => None,
            };
            previous.insert(key.as_str(), *value);
            out.into()
        })
        .collect()
}

/// Differences that treat a decrease as a counter reset (the new value is the increase).
fn counter_rate(values: &[Option<f64>], partition: &[String]) -> Vec<Cell> {
    let mut previous: HashMap<&str, Option<f64>> = HashMap::new();
    values
        .iter()
        .zip(partition)
        .map(|(value, key)| {
            let prev = previous.get(key.as_str()).copied().flatten();
            let out = match (value, prev) {
                (Some(v), Some(p)) if *v >= p => Some(v - p),
                (Some(v), Some(_)) => Some(*v),
                _ => None,
            };
            previous.insert(key.as_str(), *value);
            out.into()
        })
        .collect()
}

fn overall(values: &[Option<f64>], partition: &[String], function: OverallFunction) -> Vec<Cell> {
    let mut acc: HashMap<&str, (f64, usize)> = HashMap::new();
    for (value, key) in values.iter().zip(partition) {
        let Some(v) = value else { continue };
        let entry = acc.entry(key.as_str()).or_insert(match function {
            OverallFunction::Min => (f64::INFINITY, 0),
            OverallFunction::Max => (f64::NEG_INFINITY, 0),
            _ => (0.0, 0),
        });
        entry.0 = match function {
            OverallFunction::Sum | OverallFunction::Average => entry.0 + v,
            OverallFunction::Min => entry.0.min(*v),
            OverallFunction::Max => entry.0.max(*v),
        };
        entry.1 += 1;
    }
    partition
        .iter()
        .map(|key| {
            acc.get(key.as_str())
                .map(|(total, n)| {
                    if function == OverallFunction::Average {
                        total / *n as f64
                    } else {
                        *total
                    }
                })
                .into()
        })
        .collect()
}

/// Evaluates a formula for one row. Like Kibana, any missing input or
/// arithmetic error (e.g. dividing by zero) leaves the cell empty.
fn eval(expr: &Expr, columns: &HashMap<String, Vec<Cell>>, row: usize) -> Option<f64> {
    match expr {
        Expr::Number(n) => Some(*n),
        Expr::Column(c) => columns.get(c)?.get(row)?.number(),
        Expr::Call { call, args } => {
            let a: Vec<Option<f64>> = args.iter().map(|e| eval(e, columns, row)).collect();
            let arg = |i: usize| a.get(i).copied().flatten();
            let value = match (call.as_str(), a.len()) {
                ("add", _) => a.iter().copied().sum::<Option<f64>>()?,
                ("subtract", 2) => arg(0)? - arg(1)?,
                ("multiply", _) => a.iter().copied().product::<Option<f64>>()?,
                ("divide", 2) => {
                    let d = arg(1)?;
                    if d == 0.0 {
                        return None;
                    }
                    arg(0)? / d
                }
                ("abs", 1) => arg(0)?.abs(),
                ("cbrt", 1) => arg(0)?.cbrt(),
                ("ceil", 1) => arg(0)?.ceil(),
                ("floor", 1) => arg(0)?.floor(),
                ("cube", 1) => arg(0)?.powi(3),
                ("square", 1) => arg(0)?.powi(2),
                ("sqrt", 1) => arg(0)?.sqrt(),
                ("exp", 1) => arg(0)?.exp(),
                ("log", 1) => arg(0)?.ln(),
                ("log", 2) => arg(0)?.log(arg(1)?),
                ("log10", 1) => arg(0)?.log10(),
                ("mod", 2) => arg(0)? % arg(1)?,
                ("pow", 2) => arg(0)?.powf(arg(1)?),
                ("round", 1) => arg(0)?.round(),
                ("round", 2) => {
                    let scale = 10f64.powf(arg(1)?);
                    (arg(0)? * scale).round() / scale
                }
                ("clamp", 3) => arg(0)?.clamp(arg(1)?, arg(2)?),
                ("pick_max", 2) => arg(0)?.max(arg(1)?),
                ("pick_min", 2) => arg(0)?.min(arg(1)?),
                ("defaults", 2) => arg(0).or(arg(1))?,
                _ => return None,
            };
            value.is_finite().then_some(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(v: &[Option<f64>]) -> Vec<Cell> {
        v.iter().map(|x| Cell::from(*x)).collect()
    }

    fn one_partition(n: usize) -> Vec<String> {
        vec![String::new(); n]
    }

    #[test]
    fn moving_average_excludes_the_current_value() {
        let values = [Some(2.0), Some(4.0), None, Some(6.0), Some(8.0)];
        assert_eq!(
            moving_average(&values, &one_partition(5), 2),
            cells(&[None, Some(2.0), None, Some(3.0), Some(5.0)])
        );
    }

    #[test]
    fn cumulative_sum_carries_over_empty_rows() {
        let values = [Some(1.0), None, Some(2.0)];
        assert_eq!(
            cumulative_sum(&values, &one_partition(3)),
            cells(&[Some(1.0), Some(1.0), Some(3.0)])
        );
    }

    #[test]
    fn differences_reset_after_an_empty_row() {
        let values = [Some(1.0), Some(4.0), None, Some(5.0), Some(3.0)];
        assert_eq!(
            differences(&values, &one_partition(5)),
            cells(&[None, Some(3.0), None, None, Some(-2.0)])
        );
    }

    #[test]
    fn pipelines_run_per_partition() {
        let values = [Some(1.0), Some(10.0), Some(2.0), Some(20.0)];
        let partition: Vec<String> = ["a", "b", "a", "b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            cumulative_sum(&values, &partition),
            cells(&[Some(1.0), Some(10.0), Some(3.0), Some(30.0)])
        );
        assert_eq!(
            overall(&values, &partition, OverallFunction::Sum),
            cells(&[Some(3.0), Some(30.0), Some(3.0), Some(30.0)])
        );
        assert_eq!(
            overall(&values, &partition, OverallFunction::Average),
            cells(&[Some(1.5), Some(15.0), Some(1.5), Some(15.0)])
        );
    }

    #[test]
    fn formulas_evaluate_and_fail_softly() {
        let mut columns = HashMap::new();
        columns.insert("a".to_string(), cells(&[Some(3.0), Some(1.0), None]));
        columns.insert("b".to_string(), cells(&[Some(4.0), Some(0.0), Some(2.0)]));
        let ratio = Expr::Call {
            call: "divide".into(),
            args: vec![Expr::Column("a".into()), Expr::Column("b".into())],
        };
        assert_eq!(eval(&ratio, &columns, 0), Some(0.75));
        assert_eq!(eval(&ratio, &columns, 1), None, "divide by zero");
        assert_eq!(eval(&ratio, &columns, 2), None, "missing input");
        let rounded = Expr::Call {
            call: "round".into(),
            args: vec![ratio, Expr::Number(1.0)],
        };
        assert_eq!(eval(&rounded, &columns, 0), Some(0.8));
        let unknown = Expr::Call {
            call: "no_such_fn".into(),
            args: vec![],
        };
        assert_eq!(eval(&unknown, &columns, 0), None);
    }
}
