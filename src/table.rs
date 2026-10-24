//! GFM table source: locating cells in row lines, and rewriting a whole
//! table (for row/column operations) with aligned columns.

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// Cell content ranges of one row line, trimmed. An empty cell's range is
/// empty and sits one space after its opening pipe, where typing should go.
pub fn row_cells(line: &str) -> Vec<Range<usize>> {
    row_cells_limit(line).into_iter().map(|(r, _)| r).collect()
}

/// Like `row_cells`, with each cell's limit: how far its text may extend
/// while being edited (up to the space before the closing pipe).
pub fn row_cells_limit(line: &str) -> Vec<(Range<usize>, usize)> {
    let b = line.as_bytes();
    let lead = line.len() - line.trim_start().len();
    let mut start = if b.get(lead) == Some(&b'|') { lead + 1 } else { lead };
    let mut zones = Vec::new();
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'|' => {
                zones.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if !line[start.min(line.len())..].trim().is_empty() {
        zones.push(start..line.len());
    }
    zones
        .into_iter()
        .map(|z| {
            let s = &line[z.clone()];
            let t = s.trim();
            let r = if t.is_empty() {
                let at = z.start + s.len().min(1);
                at..at
            } else {
                let a = z.start + (s.len() - s.trim_start().len());
                a..a + t.len()
            };
            let limit = if s.ends_with(' ') { z.end - 1 } else { z.end };
            let end = r.end;
            (r, limit.max(end))
        })
        .collect()
}

fn parse_align(cell: &str) -> Align {
    let c = cell.trim();
    match (c.starts_with(':'), c.ends_with(':') && c.len() > 1) {
        (true, true) => Align::Center,
        (true, false) => Align::Left,
        (false, true) => Align::Right,
        _ => Align::None,
    }
}

/// A table as raw cell Markdown; row 0 is the header.
#[derive(Debug, PartialEq)]
pub struct Grid {
    pub aligns: Vec<Align>,
    pub rows: Vec<Vec<String>>,
}

/// Width in bytes of the container prefix (`> `, list indentation) on lines
/// after the first, given that the first line's prefix is `first_prefix`.
fn strip_prefix(line: &str, width: usize) -> &str {
    let n = line.bytes().take(width).take_while(|c| matches!(c, b' ' | b'>' | b'\t')).count();
    &line[n..]
}

/// Lines of a table block's source with the container prefix removed, each
/// with its byte offset in `text`. `prefix` is the width of the prefix
/// before the table on its first line (the block source starts after it).
pub fn lines(text: &str, prefix: usize) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (k, line) in text.split('\n').enumerate() {
        let body = if k == 0 { line } else { strip_prefix(line, prefix) };
        out.push((at + line.len() - body.len(), body));
        at += line.len() + 1;
    }
    out
}

impl Grid {
    pub fn parse(text: &str, prefix: usize) -> Grid {
        let ls = lines(text, prefix);
        let aligns: Vec<Align> = ls.get(1).map_or_else(Vec::new, |(_, l)| row_cells(l).iter().map(|r| parse_align(&l[r.clone()])).collect());
        let cols = aligns.len().max(1);
        let rows = ls
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != 1)
            .map(|(_, (_, l))| {
                let mut row: Vec<String> = row_cells(l).into_iter().take(cols).map(|r| l[r].to_string()).collect();
                row.resize(cols, String::new());
                row
            })
            .collect();
        Grid { aligns, rows }
    }

    pub fn cols(&self) -> usize {
        self.aligns.len()
    }

    /// Source text with padded columns; continuation lines start with
    /// `prefix`. Returns the text and each cell's content range in it
    /// (row-major, like table parts).
    pub fn render(&self, prefix: &str) -> (String, Vec<Range<usize>>) {
        let w: Vec<usize> = (0..self.cols())
            .map(|c| self.rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0).max(3))
            .collect();
        let mut out = String::new();
        let mut cells = Vec::new();
        let line = |out: &mut String, row: &[String], cells: Option<&mut Vec<Range<usize>>>| {
            out.push('|');
            let mut ranges = Vec::new();
            for (c, text) in row.iter().enumerate() {
                let pad = w[c] - text.chars().count();
                let (l, r) = match self.aligns[c] {
                    Align::Right => (pad, 0),
                    Align::Center => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };
                out.push(' ');
                out.push_str(&" ".repeat(l));
                ranges.push(out.len()..out.len() + text.len());
                out.push_str(text);
                out.push_str(&" ".repeat(r));
                out.push_str(" |");
            }
            if let Some(cells) = cells {
                cells.extend(ranges);
            }
        };
        for (k, row) in self.rows.iter().enumerate() {
            if k > 0 {
                out.push('\n');
                out.push_str(prefix);
            }
            line(&mut out, row, Some(&mut cells));
            if k == 0 {
                out.push('\n');
                out.push_str(prefix);
                let delim: Vec<String> = (0..self.cols())
                    .map(|c| {
                        let d = "-".repeat(w[c] - 2);
                        match self.aligns[c] {
                            Align::None => format!("-{d}-"),
                            Align::Left => format!(":{d}-"),
                            Align::Center => format!(":{d}:"),
                            Align::Right => format!("-{d}:"),
                        }
                    })
                    .collect();
                line(&mut out, &delim, None);
            }
        }
        (out, cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_in_rows() {
        let l = "| a | b\\|c |  | d";
        let cells: Vec<&str> = row_cells(l).into_iter().map(|r| &l[r]).collect();
        assert_eq!(cells, ["a", "b\\|c", "", "d"]);
        // An empty cell's caret goes after one space.
        assert_eq!(row_cells("| a |  |")[1], 6..6);
        assert_eq!(row_cells("a|b").len(), 2);
    }

    #[test]
    fn parse_and_render() {
        let src = "| Name | Qty |\n|:-|-:|\n| apple | 3 |\n| b";
        let g = Grid::parse(src, 0);
        assert_eq!(g.aligns, [Align::Left, Align::Right]);
        assert_eq!(g.rows, [vec!["Name", "Qty"], vec!["apple", "3"], vec!["b", ""]]);
        let (out, cells) = g.render("> ");
        assert_eq!(out, "| Name  | Qty |\n> | :---- | --: |\n> | apple |   3 |\n> | b     |     |");
        assert_eq!(&out[cells[3].clone()], "3");
        assert_eq!(cells[5], cells[5].start..cells[5].start);
        assert_eq!(Grid::parse(&out, 2), g);
    }
}
