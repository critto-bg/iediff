//! 2DA parser.
//!
//! A 2DA ("2-dimensional array") is a whitespace-separated text table of game
//! rules — THAC0 progression, HP-per-class tables, weapon proficiencies — often
//! referenced by other resources to resolve a field. Layout:
//!
//! ```text
//! 2DA V1.0            <- signature (tokens, may be indented/padded)
//! <default>           <- value substituted for any absent cell
//! <col> <col> ...     <- column headers (the row-label column has no header)
//! <label> <v> <v> ... <- one data row: label then positional cell values
//! ```
//!
//! Rows are ragged: a row with fewer values than columns fills the rest with
//! the default. 2DA is plain text in practice (none are encrypted in BG2:EE),
//! but the format permits the same `0xFFFF` XOR scheme as IDS, which
//! [`decrypt_text`](crate::decrypt_text) handles transparently.
//! Spec: `md/file_formats/ie_formats/2da.md`.

use std::collections::HashMap;
use std::fmt;

use crate::decrypt_text;

/// Resource type code for 2DA files.
pub const TWODA_TYPE: u16 = 0x03f4;

/// Anything that can go wrong while parsing a 2DA file.
#[derive(Debug)]
pub enum TwoDAError {
    /// The first line's first token was not `2DA`.
    BadSignature(String),
    /// The table ended before the default-value and column-header rows.
    MissingHeader,
}

impl fmt::Display for TwoDAError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TwoDAError::BadSignature(found) => {
                write!(f, "not a 2DA file: first token was {found:?}")
            }
            TwoDAError::MissingHeader => {
                write!(f, "malformed 2DA: missing default-value or header row")
            }
        }
    }
}

impl std::error::Error for TwoDAError {}

/// One data row: a label plus its cell values. `cells` may be shorter than the
/// table's column count; absent trailing cells take the table default.
struct Row {
    label: String,
    cells: Vec<String>,
}

/// A parsed 2DA table. Cells are looked up by index or by label; an absent cell
/// resolves to [`default`](Self::default).
pub struct TwoDA {
    default: String,
    /// Value-column headers, in order. Excludes the (unlabelled) row-label column.
    columns: Vec<String>,
    rows: Vec<Row>,
    /// UPPERCASE row label -> row index. First occurrence wins.
    by_row: HashMap<String, usize>,
    /// UPPERCASE column label -> column index. First occurrence wins.
    by_col: HashMap<String, usize>,
}

impl TwoDA {
    /// Parse a 2DA resource. Decrypts first if needed. Fails only when the
    /// signature or the structural header rows are absent; data rows are taken
    /// leniently.
    pub fn parse(raw: &[u8]) -> Result<TwoDA, TwoDAError> {
        let data = decrypt_text(raw);
        let text = String::from_utf8_lossy(&data);
        // Blank lines carry no structure; skip them everywhere.
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());

        let signature = lines.next().unwrap_or("");
        match signature.split_whitespace().next() {
            Some("2DA") => {}
            Some(other) => return Err(TwoDAError::BadSignature(other.to_string())),
            None => return Err(TwoDAError::BadSignature(String::new())),
        }

        // Default value is the first token of the second row.
        let default = lines
            .next()
            .and_then(|line| line.split_whitespace().next())
            .ok_or(TwoDAError::MissingHeader)?
            .to_string();

        let columns: Vec<String> = lines
            .next()
            .ok_or(TwoDAError::MissingHeader)?
            .split_whitespace()
            .map(str::to_string)
            .collect();

        let mut rows = Vec::new();
        let mut by_row = HashMap::new();
        for line in lines {
            let mut tokens = line.split_whitespace();
            let Some(label) = tokens.next() else {
                continue;
            };
            let cells: Vec<String> = tokens.map(str::to_string).collect();
            by_row
                .entry(label.to_ascii_uppercase())
                .or_insert(rows.len());
            rows.push(Row {
                label: label.to_string(),
                cells,
            });
        }

        let mut by_col = HashMap::with_capacity(columns.len());
        for (i, col) in columns.iter().enumerate() {
            by_col.entry(col.to_ascii_uppercase()).or_insert(i);
        }

        Ok(TwoDA {
            default,
            columns,
            rows,
            by_row,
            by_col,
        })
    }

    /// Label of the row at a positional index — the value→name direction used
    /// when another resource indexes this table as a dictionary (e.g. an EFF's
    /// primary type into MSCHOOL.2DA). `None` if the index is out of range.
    pub fn row_label(&self, index: usize) -> Option<&str> {
        self.rows.get(index).map(|row| row.label.as_str())
    }

    /// Cell value at a row/column index. `None` if either index is out of range;
    /// an absent cell within range yields the table default.
    #[allow(dead_code)] // consumed by resource-field resolution (later milestone)
    pub fn get(&self, row: usize, col: usize) -> Option<&str> {
        if col >= self.columns.len() {
            return None;
        }
        let row = self.rows.get(row)?;
        Some(
            row.cells
                .get(col)
                .map(String::as_str)
                .unwrap_or(&self.default),
        )
    }

    /// Cell value by row and column label (both case-insensitive). `None` if
    /// either label is unknown.
    #[allow(dead_code)] // consumed by resource-field resolution (later milestone)
    pub fn get_by_label(&self, row: &str, col: &str) -> Option<&str> {
        let row = *self.by_row.get(&row.to_ascii_uppercase())?;
        let col = *self.by_col.get(&col.to_ascii_uppercase())?;
        self.get(row, col)
    }

    /// Table size as `(rows, columns)`.
    #[allow(dead_code)] // consumed by resource-field resolution (later milestone)
    pub fn dimensions(&self) -> (usize, usize) {
        (self.rows.len(), self.columns.len())
    }
}

impl fmt::Display for TwoDA {
    /// Human-readable view: a summary line, then the full grid with each column
    /// padded to its widest cell. Absent cells render as the default value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} rows x {} cols, default {:?}",
            self.rows.len(),
            self.columns.len(),
            self.default
        )?;

        // Column 0 is the row-label column (no header); the rest are value
        // columns. Compute each column's display width from its widest cell.
        let mut widths = vec![0usize; self.columns.len() + 1];
        for (i, col) in self.columns.iter().enumerate() {
            widths[i + 1] = col.len();
        }
        for row in &self.rows {
            widths[0] = widths[0].max(row.label.len());
            for c in 0..self.columns.len() {
                let cell = row
                    .cells
                    .get(c)
                    .map(String::as_str)
                    .unwrap_or(&self.default);
                widths[c + 1] = widths[c + 1].max(cell.len());
            }
        }

        let mut header = String::new();
        push_cell(&mut header, "", widths[0]);
        for (i, col) in self.columns.iter().enumerate() {
            push_cell(&mut header, col, widths[i + 1]);
        }
        writeln!(f, "{}", header.trim_end())?;

        for row in &self.rows {
            let mut line = String::new();
            push_cell(&mut line, &row.label, widths[0]);
            for c in 0..self.columns.len() {
                let cell = row
                    .cells
                    .get(c)
                    .map(String::as_str)
                    .unwrap_or(&self.default);
                push_cell(&mut line, cell, widths[c + 1]);
            }
            writeln!(f, "{}", line.trim_end())?;
        }

        Ok(())
    }
}

/// Append a left-aligned, width-padded cell plus a two-space gutter.
fn push_cell(line: &mut String, cell: &str, width: usize) {
    line.push_str(cell);
    for _ in cell.len()..width {
        line.push(' ');
    }
    line.push_str("  ");
}
