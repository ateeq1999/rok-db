//! Split a `.sql` file into statements, keeping the comment block directly
//! above each one (`-- name:` annotations, `-- rok:` directives and docs).

/// One statement and the comment lines immediately before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawStatement {
    /// The statement text, trimmed, without the trailing `;`.
    pub(crate) sql: String,
    /// Comment lines directly above it (no blank line in between), without
    /// the leading `--` and one following space.
    pub(crate) comments: Vec<String>,
    /// 1-based line where the statement starts.
    pub(crate) line: usize,
}

/// Split `text` at top-level `;`, respecting quotes, dollar quotes and
/// comments.
pub(crate) fn split(text: &str) -> Vec<RawStatement> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Splitter::default();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let between = out.current.trim().is_empty();
        if c == '\n' {
            if between && !out.line_has_content {
                // A blank line separates a comment block from what follows.
                out.pending.clear();
            }
            out.line += 1;
            out.line_has_content = false;
            out.current.push(c);
            i += 1;
            continue;
        }
        if !c.is_whitespace() {
            out.line_has_content = true;
        }
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            let end = find(&chars, i, |c| c == '\n').unwrap_or(chars.len());
            if between {
                let body: String = chars[i + 2..end].iter().collect();
                let body = body.strip_prefix(' ').unwrap_or(&body).trim_end();
                out.pending.push(body.to_owned());
            } else {
                out.current.extend(&chars[i..end]);
            }
            i = end;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                .map_or(chars.len(), |j| j + 2);
            out.take(&chars[i..end], !between);
            i = end;
            continue;
        }
        if between && !c.is_whitespace() {
            out.start_line = out.line;
            out.comments = std::mem::take(&mut out.pending);
        }
        let end = match c {
            ';' => {
                out.flush();
                i += 1;
                continue;
            }
            '\'' | '"' => quoted_end(&chars, i, c),
            '$' => dollar_quote_end(&chars, i).unwrap_or(i + 1),
            _ => i + 1,
        };
        out.take(&chars[i..end], true);
        i = end;
    }
    out.flush();
    out.statements
}

#[derive(Default)]
struct Splitter {
    statements: Vec<RawStatement>,
    current: String,
    pending: Vec<String>,
    comments: Vec<String>,
    start_line: usize,
    line: usize,
    line_has_content: bool,
}

impl Splitter {
    /// Append `chars` (when `keep`), counting the lines they span.
    fn take(&mut self, chars: &[char], keep: bool) {
        self.line += chars.iter().filter(|&&c| c == '\n').count();
        if keep {
            self.current.extend(chars);
        }
    }

    fn flush(&mut self) {
        let sql = self.current.trim().to_owned();
        if !sql.is_empty() {
            self.statements.push(RawStatement {
                sql,
                comments: std::mem::take(&mut self.comments),
                line: self.start_line + 1,
            });
        }
        self.current.clear();
        self.comments.clear();
    }
}

fn find(chars: &[char], from: usize, pred: impl Fn(char) -> bool) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| pred(c))
        .map(|p| from + p)
}

/// End (exclusive) of a quoted string or identifier starting at `i`;
/// a doubled quote is an escaped quote.
fn quoted_end(chars: &[char], i: usize, quote: char) -> usize {
    let mut j = i + 1;
    while j < chars.len() {
        if chars[j] == quote {
            if chars.get(j + 1) == Some(&quote) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    chars.len()
}

/// End (exclusive) of a `$tag$ ... $tag$` string starting at `i`, or `None`
/// when `$` starts something else (a `$1` parameter).
fn dollar_quote_end(chars: &[char], i: usize) -> Option<usize> {
    let close = find(chars, i + 1, |c| !(c.is_alphanumeric() || c == '_'))?;
    if chars[close] != '$' || chars.get(i + 1).is_some_and(char::is_ascii_digit) {
        return None;
    }
    let tag = &chars[i..=close];
    let body = close + 1;
    let end = (body..=chars.len().saturating_sub(tag.len()))
        .find(|&j| chars[j..j + tag.len()] == *tag)
        .map_or(chars.len(), |j| j + tag.len());
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_keeps_comment_blocks() {
        let text = "-- file header\n\n-- A user.\nCREATE TABLE users (\n  -- The id.\n  id INT\n);\n\n-- name: find :one\nSELECT 'a;b', $$x;y$$, $f$ ; $f$ FROM users WHERE id = $1;\nSELECT 1";
        let s = split(text);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].comments, ["A user."]);
        assert!(s[0].sql.contains("-- The id."));
        assert_eq!(s[0].line, 4);
        assert_eq!(s[1].comments, ["name: find :one"]);
        assert_eq!(
            s[1].sql,
            "SELECT 'a;b', $$x;y$$, $f$ ; $f$ FROM users WHERE id = $1"
        );
        assert!(s[2].comments.is_empty());
        assert_eq!(s[2].sql, "SELECT 1");
    }

    #[test]
    fn escaped_quotes_and_block_comments() {
        let s = split("INSERT INTO t VALUES ('it''s;'); /* x; */ SELECT \"a;\"");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].sql, "INSERT INTO t VALUES ('it''s;')");
        assert_eq!(s[1].sql, "SELECT \"a;\"");
    }
}
