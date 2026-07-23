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

use std::fmt;
use std::io::{self, Write};

use crate::decrypt_text;

/// Header shown for the row-label column, which the format itself leaves
/// unnamed. Named explicitly so it cannot be read as data, and so a numeric
/// row label (KITLIST.2DA) cannot be mistaken for a row ordinal.
const LABEL_HEADER: &str = "[label]";

/// Prefix of the column-index line. Doubles as the left margin of every other
/// grid line, which keeps each index aligned over its column.
const INDEX_PREFIX: &str = "col:";

/// Resource type code for 2DA files.
pub const TWODA_TYPE: u16 = 0x03f4;

/// A parsed 2DA table. Kept for its row labels — the value→name lookups other
/// resources index it for — and for printing; an absent cell resolves to
/// [`default`](Self::default).
pub struct TwoDA {
    default: String,
    /// Value-column headers, in order. Excludes the (unlabelled) row-label column.
    columns: Vec<String>,
    rows: Vec<Row>,
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
        for line in lines {
            let mut tokens = line.split_whitespace();
            let Some(label) = tokens.next() else {
                continue;
            };
            let cells: Vec<String> = tokens.map(str::to_string).collect();
            rows.push(Row {
                label: label.to_string(),
                cells,
            });
        }

        Ok(TwoDA {
            default,
            columns,
            rows,
        })
    }

    /// Label of the row at a positional index — the value→name direction used
    /// when another resource indexes this table as a dictionary (e.g. an EFF's
    /// primary type into MSCHOOL.2DA). `None` if the index is out of range.
    pub fn row_label(&self, index: usize) -> Option<&str> {
        self.rows.get(index).map(|row| row.label.as_str())
    }

    /// Write the machine-readable view: the bare grid, as the file itself lays
    /// it out. Deliberately free of the [`Display`] annotations, which are a
    /// reading aid rather than data.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        write!(
            out,
            "{} rows x {} cols, default {:?}\n{}",
            self.rows.len(),
            self.columns.len(),
            self.default,
            self.render(false)
        )
    }

    /// Value of the cell at column `c` of `row`, defaulting when absent.
    fn cell<'a>(&'a self, row: &'a Row, c: usize) -> &'a str {
        row.cells
            .get(c)
            .map(String::as_str)
            .unwrap_or(&self.default)
    }

    /// Render the grid, each column padded to its widest cell. `annotate` adds
    /// the column-index line and names the row-label column; see [`Display`].
    fn render(&self, annotate: bool) -> String {
        // Column 0 is the row-label column, which the format leaves unheaded;
        // the rest are the value columns.
        let mut widths = vec![0usize; self.columns.len() + 1];
        if annotate {
            widths[0] = LABEL_HEADER.len();
        }
        for (i, col) in self.columns.iter().enumerate() {
            widths[i + 1] = col.len();
        }
        for row in &self.rows {
            widths[0] = widths[0].max(row.label.len());
            for c in 0..self.columns.len() {
                widths[c + 1] = widths[c + 1].max(self.cell(row, c).len());
            }
        }

        // The index line's prefix sets the left margin every other line matches.
        let margin = |line: &mut String| {
            if annotate {
                push_cell(line, "", INDEX_PREFIX.len());
            }
        };
        let mut out = String::new();
        let mut flush = |line: &str| {
            out.push_str(line.trim_end());
            out.push('\n');
        };

        if annotate {
            let mut line = String::new();
            push_cell(&mut line, INDEX_PREFIX, INDEX_PREFIX.len());
            for (i, width) in widths.iter().enumerate() {
                push_cell(&mut line, &i.to_string(), *width);
            }
            flush(&line);
        }

        let mut header = String::new();
        margin(&mut header);
        push_cell(
            &mut header,
            if annotate { LABEL_HEADER } else { "" },
            widths[0],
        );
        for (i, col) in self.columns.iter().enumerate() {
            push_cell(&mut header, col, widths[i + 1]);
        }
        flush(&header);

        for row in &self.rows {
            let mut line = String::new();
            margin(&mut line);
            push_cell(&mut line, &row.label, widths[0]);
            for c in 0..self.columns.len() {
                push_cell(&mut line, self.cell(row, c), widths[c + 1]);
            }
            flush(&line);
        }

        out
    }
}

impl fmt::Display for TwoDA {
    /// Human-readable view: a summary line, a column-index line, then the grid.
    ///
    /// The indices are WeiDU's, as `SET_2DA_ENTRY`/`READ_2DA_ENTRY` count them:
    /// column 0 is the row label, so the first *headed* column is 1. Absent
    /// cells render as the default value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} rows x {} cols (+1 row label -> {} WeiDU cols), default {:?}\n{}",
            self.rows.len(),
            self.columns.len(),
            self.columns.len() + 1,
            self.default,
            self.render(true)
        )
    }
}

/// One data row: a label plus its cell values. `cells` may be shorter than the
/// table's column count; absent trailing cells take the table default.
struct Row {
    label: String,
    cells: Vec<String>,
}

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

/// Append a left-aligned, width-padded cell plus a two-space gutter.
fn push_cell(line: &mut String, cell: &str, width: usize) {
    line.push_str(cell);
    for _ in cell.len()..width {
        line.push(' ');
    }
    line.push_str("  ");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table shaped like KITLIST.2DA: the row label is a bare integer, so the
    /// header row looks one column further left than it is.
    const NUMBERED: &[u8] = b"2DA V1.0\n*\nROWNAME HELP\n0 RESERVE 1\n1 BERSERKER 2\n";

    fn parse(raw: &[u8]) -> TwoDA {
        TwoDA::parse(raw).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    /// The human view states both totals: value columns, and the WeiDU count
    /// that includes the row label.
    #[test]
    fn summary_reports_weidu_column_count() {
        let shown = parse(NUMBERED).to_string();
        let summary = shown.lines().next().unwrap_or_default();
        assert_eq!(
            summary,
            "2 rows x 2 cols (+1 row label -> 3 WeiDU cols), default \"*\""
        );
    }

    /// Each index sits over its own column, and the row label is column 0 —
    /// so a `SET_2DA_ENTRY` number reads straight off the dump.
    #[test]
    fn column_indices_align_over_their_columns() {
        let shown = parse(NUMBERED).to_string();
        let mut lines = shown.lines().skip(1);
        let index = lines.next().unwrap_or_default();
        let header = lines.next().unwrap_or_default();

        assert_eq!(index, "col:  0        1          2");
        assert_eq!(header, "      [label]  ROWNAME    HELP");
        // The trap this guards: HELP is column 2, not the column 1 that a
        // header row read against a numeric label column suggests.
        for (i, name) in [(0, "[label]"), (1, "ROWNAME"), (2, "HELP")] {
            let at = index.find(&i.to_string()).unwrap_or_default();
            assert_eq!(header.find(name), Some(at), "column {i} is {name}");
        }
    }

    /// The annotations are a reading aid; the machine-readable view stays bare.
    #[test]
    fn raw_view_is_unannotated() {
        let mut out = Vec::new();
        parse(NUMBERED)
            .write_raw(&mut out)
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        let raw = String::from_utf8_lossy(&out);

        assert_eq!(
            raw,
            "2 rows x 2 cols, default \"*\"\n\
             \x20  ROWNAME    HELP\n\
             0  RESERVE    1\n\
             1  BERSERKER  2\n"
        );
    }

    /// Absent trailing cells take the table default in both views.
    #[test]
    fn ragged_rows_fill_with_default() {
        let shown = parse(b"2DA V1.0\nX\nA B\nrow 1\n").to_string();
        let last = shown.lines().last().unwrap_or_default();
        assert_eq!(last, "      row      1  X");
    }
}
