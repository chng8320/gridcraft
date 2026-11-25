//! Review › Compare Files: the active workbook against a revised copy, sheet by sheet and cell by
//! cell (values and formulas), as a report. Read-only: neither workbook changes.

use std::collections::{BTreeMap, BTreeSet};

use gridcraft_core::CellRef;
use gridcraft_model::{Cell, Sheet, Workbook};
use serde_json::{Value as Json, json};

use super::{CommandSpec, Result, Session, bad, bool_param, cmd, has_doc, str_param, u32_param};

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        query "review.compareFiles",
        "Compare Files…",
        ["Review", "Compare"],
        None,
        "{path} | {name, base64}: the revised workbook, plus {password?}, {formulas?: bool (default true: a changed formula counts even when its result is the same), values?: bool (default true), max?: n (default 5000 cells listed)} → {sheets: {added, removed, compared}, cells: [{sheet, cell, change: added|removed|value|formula, before?, after?, formulaBefore?, formulaAfter?}], counts, truncated}",
        has_doc,
        compare_files
    )]
}

/// What a compare looks at.
#[derive(Clone, Copy, Debug)]
pub struct CompareOptions {
    pub formulas: bool,
    pub values: bool,
    pub max: usize,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self { formulas: true, values: true, max: 5000 }
    }
}

fn compare_files(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "review.compareFiles";
    let (name, bytes) = if let Some(b) = str_param(p, "base64") {
        (str_param(p, "name").unwrap_or("Revised.xlsx").to_string(), crate::io::base64_decode(b).ok_or_else(|| bad(CMD, "invalid base64"))?)
    } else if let Some(path) = str_param(p, "path") {
        (path.to_string(), crate::io::read_file(path)?)
    } else {
        return Err(bad(CMD, "give the revised workbook: `path`, or `name` and `base64`"));
    };
    let (revised, _warnings) = crate::io::open_bytes_with_password(&name, &bytes, str_param(p, "password"))?;
    let d = s.active().ok_or_else(|| bad(CMD, "no workbook is open"))?;
    let opts = CompareOptions {
        formulas: bool_param(p, "formulas").unwrap_or(true),
        values: bool_param(p, "values").unwrap_or(true),
        max: u32_param(p, "max").map(|m| m.max(1) as usize).unwrap_or(5000),
    };
    Ok(compare_workbooks(&d.wb, &revised, opts))
}

/// A cell that holds something: a value or a formula (a cell with only a style is empty here).
fn content(c: &Cell) -> bool {
    c.formula.is_some() || c.py.is_some() || !matches!(c.value, gridcraft_core::Value::Empty)
}

fn cells(sh: &Sheet) -> BTreeMap<CellRef, &Cell> {
    sh.cells.iter().filter(|(_, c)| content(c)).collect()
}

fn formula_text(c: &Cell) -> Option<String> {
    c.formula.as_ref().map(|f| if f.text.starts_with('=') { f.text.clone() } else { format!("={}", f.text) })
}

/// `original` against `revised`: sheets matched by name (case-insensitive, as Excel matches them),
/// then every cell either holds. A cell whose formula changed is a `formula` change (with its
/// results); otherwise a changed value is a `value` change.
pub fn compare_workbooks(original: &Workbook, revised: &Workbook, opts: CompareOptions) -> Json {
    let key = |s: &str| s.to_lowercase();
    let a: BTreeMap<String, &Sheet> = original.sheets.iter().map(|s| (key(&s.name), s.as_ref())).collect();
    let b: BTreeMap<String, &Sheet> = revised.sheets.iter().map(|s| (key(&s.name), s.as_ref())).collect();
    let removed: Vec<&str> = original.sheets.iter().filter(|s| !b.contains_key(&key(&s.name))).map(|s| s.name.as_str()).collect();
    let added: Vec<&str> = revised.sheets.iter().filter(|s| !a.contains_key(&key(&s.name))).map(|s| s.name.as_str()).collect();
    let mut out = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut compared = Vec::new();
    for sh in original.sheets.iter().filter(|s| b.contains_key(&key(&s.name))) {
        let Some(other) = b.get(&key(&sh.name)) else { continue };
        compared.push(sh.name.as_str());
        let (ca, cb) = (cells(sh), cells(other));
        let at: BTreeSet<CellRef> = ca.keys().chain(cb.keys()).copied().collect();
        for c in at {
            let row = match (ca.get(&c), cb.get(&c)) {
                (Some(x), None) => Some(json!({"change": "removed", "before": x.value.display(), "formulaBefore": formula_text(x)})),
                (None, Some(y)) => Some(json!({"change": "added", "after": y.value.display(), "formulaAfter": formula_text(y)})),
                (Some(x), Some(y)) => {
                    let (fx, fy) = (formula_text(x), formula_text(y));
                    if opts.formulas && fx != fy {
                        Some(json!({"change": "formula", "before": x.value.display(), "after": y.value.display(), "formulaBefore": fx, "formulaAfter": fy}))
                    } else if opts.values && x.value != y.value {
                        Some(json!({"change": "value", "before": x.value.display(), "after": y.value.display()}))
                    } else {
                        None
                    }
                }
                (None, None) => None,
            };
            let Some(mut row) = row else { continue };
            let kind = row["change"].as_str().unwrap_or("");
            *counts.entry(match kind {
                "added" => "added",
                "removed" => "removed",
                "formula" => "formula",
                _ => "value",
            })
            .or_default() += 1;
            if out.len() < opts.max {
                row["sheet"] = json!(sh.name);
                row["cell"] = json!(c.a1());
                if let Some(o) = row.as_object_mut() {
                    o.retain(|_, v| !v.is_null());
                }
                out.push(row);
            }
        }
    }
    let total: usize = counts.values().sum();
    json!({
        "sheets": {"added": added, "removed": removed, "compared": compared},
        "cells": out,
        "counts": {"added": counts.get("added").copied().unwrap_or(0), "removed": counts.get("removed").copied().unwrap_or(0),
                   "value": counts.get("value").copied().unwrap_or(0), "formula": counts.get("formula").copied().unwrap_or(0), "total": total},
        "truncated": total > out.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcraft_core::Value;
    use gridcraft_model::Formula;
    use std::sync::Arc;

    fn book(sheets: &[(&str, &[(&str, Cell)])]) -> Workbook {
        let mut wb = Workbook::new();
        wb.sheets.clear();
        for (name, cells) in sheets {
            let mut sh = Sheet::new(*name);
            for (at, c) in cells.iter() {
                sh.cells.set(CellRef::parse(at).unwrap_or_else(|| panic!("{at}")), c.clone());
            }
            wb.sheets.push(Arc::new(sh));
        }
        wb
    }

    fn num(n: f64) -> Cell {
        Cell::value(Value::Number(n))
    }

    fn formula(text: &str, result: f64) -> Cell {
        let mut c = Cell::formula(Formula::new(text));
        c.value = Value::Number(result);
        c
    }

    #[test]
    fn changed_added_and_removed_cells_and_sheets_are_reported() {
        let a = book(&[("Data", &[("A1", num(1.0)), ("A2", num(2.0)), ("B1", formula("=A1*2", 2.0))]), ("Old", &[("A1", num(9.0))])]);
        let b = book(&[("data", &[("A1", num(1.0)), ("A2", num(5.0)), ("B1", formula("=A1*3", 3.0)), ("C1", num(7.0))]), ("New", &[])]);
        let r = compare_workbooks(&a, &b, CompareOptions::default());
        assert_eq!(r["sheets"]["removed"], json!(["Old"]));
        assert_eq!(r["sheets"]["added"], json!(["New"]));
        assert_eq!(r["sheets"]["compared"], json!(["Data"]), "sheet names match regardless of case");
        let changes: Vec<(String, String)> = r["cells"].as_array().into_iter().flatten().map(|c| (c["cell"].as_str().unwrap_or("").into(), c["change"].as_str().unwrap_or("").into())).collect();
        assert!(changes.contains(&("A2".into(), "value".into())), "{changes:?}");
        assert!(changes.contains(&("B1".into(), "formula".into())), "{changes:?}");
        assert!(changes.contains(&("C1".into(), "added".into())), "{changes:?}");
        assert!(!changes.iter().any(|(c, _)| c == "A1"), "an unchanged cell is not listed");
        assert_eq!(r["counts"]["total"], 3);
    }

    #[test]
    fn a_rewritten_formula_with_the_same_result_counts_only_when_formulas_are_compared() {
        let a = book(&[("S", &[("A1", formula("=1+1", 2.0))])]);
        let b = book(&[("S", &[("A1", formula("=2*1", 2.0))])]);
        assert_eq!(compare_workbooks(&a, &b, CompareOptions::default())["counts"]["formula"], 1);
        assert_eq!(compare_workbooks(&a, &b, CompareOptions { formulas: false, ..CompareOptions::default() })["counts"]["total"], 0);
    }

    #[test]
    fn the_list_stops_at_max_but_the_counts_do_not() {
        let many: Vec<(String, Cell)> = (1..=20).map(|i| (format!("A{i}"), num(i as f64))).collect();
        let refs: Vec<(&str, Cell)> = many.iter().map(|(a, c)| (a.as_str(), c.clone())).collect();
        let a = book(&[("S", &[])]);
        let b = book(&[("S", &refs)]);
        let r = compare_workbooks(&a, &b, CompareOptions { max: 5, ..CompareOptions::default() });
        assert_eq!((r["cells"].as_array().map(Vec::len), r["counts"]["added"].as_u64(), r["truncated"].as_bool()), (Some(5), Some(20), Some(true)));
    }
}
