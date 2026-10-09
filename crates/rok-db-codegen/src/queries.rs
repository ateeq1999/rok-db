//! Typed functions for annotated queries.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, Tokenizer, Word};

use crate::describe::Description;
use crate::emit::{Names, raw_string};
use crate::ir::{Query, QueryKind, Schema};
use crate::types::{self, Feature};
use crate::{Error, naming};

/// Facts read from the query text.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Shape {
    /// Name of each `$n` parameter (index 0 is `$1`).
    pub(crate) param_names: Vec<String>,
    /// Tables written by `INSERT INTO`, `UPDATE` and `DELETE FROM`.
    pub(crate) writes: Vec<String>,
    /// Tables named after `FROM`, `JOIN`, `INTO` and `UPDATE`.
    pub(crate) reads: Vec<String>,
}

fn words(sql: &str) -> Result<Vec<Token>, String> {
    let tokens = Tokenizer::new(&PostgreSqlDialect {}, sql)
        .tokenize()
        .map_err(|e| e.to_string())?;
    Ok(tokens
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect())
}

fn word_name(w: &Word) -> String {
    if w.quote_style.is_some() {
        w.value.clone()
    } else {
        w.value.to_ascii_lowercase()
    }
}

/// A possibly qualified name starting at `i`: (`schema.table`, next index).
fn name_at(tokens: &[Token], mut i: usize) -> Option<(String, usize)> {
    let mut parts = Vec::new();
    while let Some(Token::Word(w)) = tokens.get(i) {
        parts.push(word_name(w));
        i += 1;
        if tokens.get(i) == Some(&Token::Period) {
            i += 1;
        } else {
            break;
        }
    }
    (!parts.is_empty()).then(|| (parts.join("."), i))
}

fn placeholder(t: &Token) -> Option<usize> {
    match t {
        Token::Placeholder(p) => p.strip_prefix('$')?.parse().ok(),
        _ => None,
    }
}

fn is_keyword(t: Option<&Token>, k: Keyword) -> bool {
    matches!(t, Some(Token::Word(w)) if w.keyword == k)
}

fn is_comparison(t: Option<&Token>) -> bool {
    matches!(
        t,
        Some(Token::Eq | Token::Neq | Token::Lt | Token::Gt | Token::LtEq | Token::GtEq)
    ) || [Keyword::LIKE, Keyword::ILIKE]
        .iter()
        .any(|k| is_keyword(t, *k))
}

/// The column name ending at index `end` (`users.email` -> `email`).
fn column_before(tokens: &[Token], end: usize) -> Option<String> {
    const STRUCTURAL: &[Keyword] = &[
        Keyword::AND,
        Keyword::OR,
        Keyword::NOT,
        Keyword::WHERE,
        Keyword::ON,
        Keyword::SET,
        Keyword::SELECT,
        Keyword::FROM,
        Keyword::BY,
        Keyword::WHEN,
        Keyword::THEN,
        Keyword::ELSE,
        Keyword::CASE,
        Keyword::IS,
        Keyword::NULL,
        Keyword::ALL,
        Keyword::ANY,
        Keyword::SOME,
        Keyword::IN,
        Keyword::LIKE,
        Keyword::ILIKE,
        Keyword::BETWEEN,
        Keyword::HAVING,
        Keyword::RETURNING,
        Keyword::VALUES,
        Keyword::LIMIT,
        Keyword::OFFSET,
    ];
    match tokens.get(end)? {
        Token::Word(w) if w.quote_style.is_some() || !STRUCTURAL.contains(&w.keyword) => {
            Some(word_name(w))
        }
        _ => None,
    }
}

pub(crate) fn shape(query: &Query) -> Result<Shape, Error> {
    let tokens = words(&query.sql).map_err(|e| Error::at(&query.location, e))?;
    let count = tokens.iter().filter_map(placeholder).max().unwrap_or(0);
    let mut names: Vec<Option<String>> = vec![None; count];
    let mut shape = Shape::default();

    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if let Some(n) = placeholder(t) {
            let guess = guess_name(&tokens, i);
            if names[n - 1].is_none() {
                names[n - 1] = guess;
            }
        }
        if let Token::Word(w) = t {
            let target = match w.keyword {
                Keyword::INTO | Keyword::UPDATE => Some(true),
                Keyword::FROM => Some(is_keyword(tokens.get(i.wrapping_sub(1)), Keyword::DELETE)),
                Keyword::JOIN => Some(false),
                _ => None,
            };
            if let Some(writes) = target {
                let mut j = i + 1;
                if is_keyword(tokens.get(j), Keyword::ONLY) {
                    j += 1;
                }
                if let Some((name, next)) = name_at(&tokens, j) {
                    // `ON CONFLICT ... DO UPDATE SET` is not a table.
                    let is_set = is_keyword(tokens.get(j), Keyword::SET);
                    if !is_set {
                        if writes && !shape.writes.contains(&name) {
                            shape.writes.push(name.clone());
                        }
                        if !shape.reads.contains(&name) {
                            shape.reads.push(name.clone());
                        }
                    }
                    if w.keyword == Keyword::INTO {
                        insert_names(&tokens, next, &mut names);
                    }
                }
            }
        }
        i += 1;
    }
    for (n, name) in &query.param_names {
        if *n == 0 || *n > count {
            return Err(Error::at(
                &query.location,
                format!("`-- param: ${n}` but the query has {count} parameter(s)"),
            ));
        }
        names[n - 1] = Some(name.clone());
    }
    let mut used = BTreeSet::new();
    for (n, name) in names.into_iter().enumerate() {
        let mut name = naming::snake_ident(&name.unwrap_or_else(|| format!("p{}", n + 1)));
        if name == "db" || !used.insert(name.clone()) {
            name = format!("{name}_{}", n + 1);
            used.insert(name.clone());
        }
        shape.param_names.push(name);
    }
    Ok(shape)
}

/// The column an operand ending at `end` names: `email`, `u.email` or
/// `lower(email)`.
fn operand_before(tokens: &[Token], end: usize) -> Option<String> {
    match tokens.get(end)? {
        Token::RParen => column_before(tokens, end.checked_sub(1)?),
        _ => column_before(tokens, end),
    }
}

/// The column an operand starting at `start` names: `email` or `lower(email)`.
fn operand_after(tokens: &[Token], start: usize) -> Option<String> {
    if matches!(tokens.get(start)?, Token::Word(_)) && tokens.get(start + 1) == Some(&Token::LParen)
    {
        return column_before(tokens, start + 2);
    }
    column_before(tokens, start)
}

fn guess_name(tokens: &[Token], i: usize) -> Option<String> {
    // `$1` may be wrapped in a function call: `lower($1)`.
    let wrapped = tokens.get(i.wrapping_sub(1)) == Some(&Token::LParen)
        && matches!(tokens.get(i.wrapping_sub(2)), Some(Token::Word(w)) if w.keyword == Keyword::NoKeyword || w.keyword == Keyword::LOWER || w.keyword == Keyword::UPPER || w.keyword == Keyword::TRIM)
        && tokens.get(i + 1) == Some(&Token::RParen);
    let (start, end) = if wrapped { (i - 2, i + 1) } else { (i, i) };
    let prev = tokens.get(start.wrapping_sub(1));
    // col = $1, lower(col) = lower($1), col LIKE $1
    if is_comparison(prev) {
        if let Some(name) = start.checked_sub(2).and_then(|k| operand_before(tokens, k)) {
            return Some(name);
        }
    }
    // $1 = col
    if is_comparison(tokens.get(end + 1)) {
        if let Some(name) = operand_after(tokens, end + 2) {
            return Some(name);
        }
    }
    let prev = tokens.get(i.wrapping_sub(1));
    if is_keyword(prev, Keyword::LIMIT) {
        return Some("limit".to_owned());
    }
    if is_keyword(prev, Keyword::OFFSET) {
        return Some("offset".to_owned());
    }
    // col BETWEEN $1 AND $2
    if is_keyword(prev, Keyword::BETWEEN) {
        return column_before(tokens, i - 2).map(|c| format!("{c}_from"));
    }
    if is_keyword(prev, Keyword::AND)
        && placeholder(tokens.get(i.wrapping_sub(2))?).is_some()
        && is_keyword(tokens.get(i.wrapping_sub(3)), Keyword::BETWEEN)
    {
        return column_before(tokens, i - 4).map(|c| format!("{c}_to"));
    }
    // col = ANY($1), col IN ($1)
    if prev == Some(&Token::LParen) {
        let before = tokens.get(i.wrapping_sub(2));
        let column_at =
            if is_keyword(before, Keyword::ANY) && is_comparison(tokens.get(i.wrapping_sub(3))) {
                Some(i - 4)
            } else if is_keyword(before, Keyword::IN) {
                Some(i - 3)
            } else {
                None
            };
        if let Some(at) = column_at {
            return column_before(tokens, at).map(|c| naming::plural(&c));
        }
    }
    None
}

/// `INSERT INTO t (a, b) VALUES ($1, $2)`: name each parameter after its column.
fn insert_names(tokens: &[Token], mut i: usize, names: &mut [Option<String>]) {
    if tokens.get(i) != Some(&Token::LParen) {
        return;
    }
    i += 1;
    let mut columns = Vec::new();
    while let Some(t) = tokens.get(i) {
        match t {
            Token::Word(w) => columns.push(word_name(w)),
            Token::RParen => break,
            _ => {}
        }
        i += 1;
    }
    let Some(values) = (i..tokens.len()).find(|&j| is_keyword(tokens.get(j), Keyword::VALUES))
    else {
        return;
    };
    // Only the first row: ( item, item, ... ), where an item that is just `$n` is named.
    let mut j = values + 2;
    let mut position = 0;
    let mut depth = 0;
    let mut item: Vec<&Token> = Vec::new();
    while let Some(t) = tokens.get(j) {
        match t {
            Token::LParen => depth += 1,
            Token::RParen if depth == 0 => {
                assign(&item, position, &columns, names);
                break;
            }
            Token::RParen => depth -= 1,
            Token::Comma if depth == 0 => {
                assign(&item, position, &columns, names);
                item.clear();
                position += 1;
                j += 1;
                continue;
            }
            _ => {}
        }
        item.push(t);
        j += 1;
    }
}

fn assign(item: &[&Token], position: usize, columns: &[String], names: &mut [Option<String>]) {
    if let ([t], Some(column)) = (item, columns.get(position)) {
        if let Some(n) = placeholder(t) {
            if names[n - 1].is_none() {
                names[n - 1] = Some(column.clone());
            }
        }
    }
}

/// What a generated function returns per row.
enum Returns {
    Model(String),
    Scalar(String),
    Row(String, Vec<(String, String, String)>),
    Nothing,
}

/// Generate the functions of one module. Warnings (missing tenant or
/// soft-delete filters) are appended to `warnings`.
pub(crate) fn functions(
    queries: &[(&Query, &Description)],
    schema: &Schema,
    features: &mut BTreeSet<Feature>,
    warnings: &mut Vec<String>,
) -> Result<String, Error> {
    let names = Names::new(schema);
    let mut code = String::new();
    for (query, description) in queries {
        let at = |m: String| Error::at(&query.location, m);
        if query.sql.contains('?') {
            return Err(at(
                "`?` can't be used in a generated query (rok-db would read it as a placeholder); \
                 use `$n` parameters and functions such as `jsonb_exists` instead of the `?` operator"
                    .to_owned(),
            ));
        }
        let shape = shape(query)?;
        if shape.param_names.len() != description.params.len() {
            return Err(at(format!(
                "PostgreSQL reports {} parameter(s), the query text has {}",
                description.params.len(),
                shape.param_names.len()
            )));
        }
        scope_warnings(query, &shape, schema, warnings);

        let mut params = Vec::new();
        for (name, pg) in shape.param_names.iter().zip(&description.params) {
            let canonical = types::canonical_from_pg(pg);
            let ty = names
                .rust_type(&canonical, false, features)
                .map_err(|e| at(format!("parameter `{name}`: {e}")))?;
            let arg = match ty.as_str() {
                "String" => "&str".to_owned(),
                t if t.starts_with("Vec<") => format!("&[{}]", &t[4..t.len() - 1]),
                t => t.to_owned(),
            };
            params.push((name.clone(), arg));
        }

        let returns = if query.kind == QueryKind::Exec {
            Returns::Nothing
        } else {
            returns(query, description, &shape, schema, &names, features).map_err(at)?
        };

        let fn_name = &query.name;
        let row_type = match &returns {
            Returns::Model(path) | Returns::Scalar(path) => path.clone(),
            Returns::Row(name, fields) => {
                let _ = writeln!(code, "/// A row returned by [`{fn_name}`].");
                let _ = writeln!(
                    code,
                    "#[derive(Debug, Clone, PartialEq, rok_db::FromRow)]\npub struct {name} {{"
                );
                for (field, column, ty) in fields {
                    let _ = writeln!(code, "    /// The `{column}` column.");
                    if field != column {
                        let _ = writeln!(code, "    #[rok(column = {column:?})]");
                    }
                    let _ = writeln!(code, "    pub {field}: {ty},");
                }
                code.push_str("}\n\n");
                name.clone()
            }
            Returns::Nothing => String::new(),
        };
        let scalar = matches!(returns, Returns::Scalar(_));
        let decode = if scalar {
            format!("({row_type},)")
        } else {
            row_type.clone()
        };

        for line in &query.doc {
            let _ = writeln!(code, "/// {line}");
        }
        if !query.doc.is_empty() {
            code.push_str("///\n");
        }
        let kind = match query.kind {
            QueryKind::One => ":one",
            QueryKind::OneRequired => ":one!",
            QueryKind::Many => ":many",
            QueryKind::Stream => ":stream",
            QueryKind::Exec => ":exec",
        };
        let _ = writeln!(
            code,
            "/// `-- name: {fn_name} {kind}` from `{}`:\n///\n/// ```sql",
            query.location
        );
        for line in query.sql.lines() {
            let _ = writeln!(code, "/// {}", line.trim_end());
        }
        code.push_str("/// ```\n");

        let args: String = params.iter().map(|(n, t)| format!(", {n}: {t}")).collect();
        let mut chain = format!("rok_db::raw({})", raw_string(&query.sql));
        for (name, _) in &params {
            let _ = write!(chain, ".bind({name})");
        }
        for table in &shape.writes {
            if schema.tables.iter().any(|t| &t.name == table) {
                let _ = write!(chain, ".invalidates({table:?})");
            }
        }
        let (ret, body) = match query.kind {
            QueryKind::Exec => (
                "rok_db::Result<u64>".to_owned(),
                format!("{chain}.execute(db).await"),
            ),
            QueryKind::One => (
                format!("rok_db::Result<Option<{row_type}>>"),
                if scalar {
                    format!(
                        "{chain}.fetch_optional::<{decode}, _>(db).await.map(|row| row.map(|(value,)| value))"
                    )
                } else {
                    format!("{chain}.fetch_optional(db).await")
                },
            ),
            QueryKind::OneRequired => (
                format!("rok_db::Result<{row_type}>"),
                if scalar {
                    format!("{chain}.fetch_one::<{decode}, _>(db).await.map(|(value,)| value)")
                } else {
                    format!("{chain}.fetch_one(db).await")
                },
            ),
            QueryKind::Many => (
                format!("rok_db::Result<Vec<{row_type}>>"),
                if scalar {
                    format!(
                        "{chain}.fetch_all::<{decode}, _>(db).await.map(|rows| rows.into_iter().map(|(value,)| value).collect())"
                    )
                } else {
                    format!("{chain}.fetch_all(db).await")
                },
            ),
            QueryKind::Stream => (
                format!("rok_db::BoxStream<'e, rok_db::Result<{row_type}>>"),
                if scalar {
                    format!(
                        "rok_db::StreamExt::boxed(rok_db::StreamExt::map({chain}.stream::<{decode}, _>(db), |row| row.map(|(value,)| value)))"
                    )
                } else {
                    format!("{chain}.stream(db)")
                },
            ),
        };
        if query.kind == QueryKind::Stream {
            let _ = writeln!(
                code,
                "pub fn {fn_name}<'e>(db: impl rok_db::Executor<'e> + 'e{args}) -> {ret} {{\n    {body}\n}}\n"
            );
        } else {
            let _ = writeln!(
                code,
                "pub async fn {fn_name}<'e>(db: impl rok_db::Executor<'e>{args}) -> {ret} {{\n    {body}\n}}\n"
            );
        }
    }
    Ok(code)
}

fn returns(
    query: &Query,
    description: &Description,
    shape: &Shape,
    schema: &Schema,
    names: &Names<'_>,
    features: &mut BTreeSet<Feature>,
) -> Result<Returns, String> {
    let columns = &description.columns;
    if columns.is_empty() {
        return Err("the query returns no columns; annotate it with `:exec`".to_owned());
    }
    // Every column of one table, in order: return its model.
    for table_name in &shape.reads {
        let Some(table) = schema.tables.iter().find(|t| &t.name == table_name) else {
            continue;
        };
        // Models decode by column name, so the order doesn't matter (columns
        // added by a later migration come last in the database).
        let same = table.columns.len() == columns.len()
            && table.columns.iter().all(|tc| {
                columns
                    .iter()
                    .any(|dc| tc.name == dc.name && (tc.nullable || dc.nullable != Some(true)))
            });
        if same {
            return Ok(Returns::Model(
                names.model_path(&table.name).unwrap_or_default(),
            ));
        }
    }
    let mut fields = Vec::new();
    let mut seen = BTreeSet::new();
    for column in columns {
        if column.name == "?column?" {
            return Err("give every computed column a name (`count(*) AS total`)".to_owned());
        }
        let (name, nullable) = match column.name.strip_suffix('!') {
            Some(name) => (name, false),
            None => match column.name.strip_suffix('?') {
                Some(name) => (name, true),
                None => (column.name.as_str(), column.nullable != Some(false)),
            },
        };
        let field = naming::snake_ident(name);
        if !seen.insert(field.clone()) {
            return Err(format!(
                "two result columns are named `{field}`; rename one with AS"
            ));
        }
        let ty = names
            .rust_type(
                &types::canonical_from_pg(&column.type_name),
                nullable,
                features,
            )
            .map_err(|e| format!("column `{name}`: {e}"))?;
        fields.push((field, column.name.clone(), ty));
    }
    if fields.len() == 1 {
        return Ok(Returns::Scalar(fields.remove(0).2));
    }
    Ok(Returns::Row(
        format!("{}Row", naming::pascal(&query.name)),
        fields,
    ))
}

fn scope_warnings(query: &Query, shape: &Shape, schema: &Schema, warnings: &mut Vec<String>) {
    let text = query.sql.to_ascii_lowercase();
    for table in schema
        .tables
        .iter()
        .filter(|t| shape.reads.contains(&t.name))
    {
        for column in table.columns.iter().filter(|c| c.tenant) {
            if !text.contains(&column.name.to_ascii_lowercase()) {
                warnings.push(format!(
                    "{}: `{}` reads tenant table `{}` without filtering on `{}`",
                    query.location, query.name, table.name, column.name
                ));
            }
        }
        let soft_delete = table.column("deleted_at").is_some_and(|c| c.nullable);
        let selects =
            text.trim_start().starts_with("select") || text.trim_start().starts_with("with");
        if soft_delete && selects && !text.contains("deleted_at") {
            warnings.push(format!(
                "{}: `{}` reads soft-delete table `{}` without filtering on `deleted_at`",
                query.location, query.name, table.name
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(sql: &str) -> Query {
        Query {
            name: "q".into(),
            module: "m".into(),
            kind: QueryKind::Many,
            sql: sql.into(),
            doc: vec![],
            param_names: vec![],
            location: "db/m.sql:1".into(),
        }
    }

    #[test]
    fn parameter_names() {
        let s = shape(&q("SELECT * FROM users u WHERE u.email = $1 AND $2 < age AND created_at BETWEEN $3 AND $4 AND id = ANY($5) AND role IN ($6) LIMIT $7 OFFSET $8")).unwrap();
        assert_eq!(
            s.param_names,
            [
                "email",
                "age",
                "created_at_from",
                "created_at_to",
                "ids",
                "roles",
                "limit",
                "offset"
            ]
        );
        assert_eq!(s.reads, ["users"]);
        assert!(s.writes.is_empty());

        let s = shape(&q("INSERT INTO app.posts (author_id, title, views) VALUES ($1, lower($2), $3) RETURNING *")).unwrap();
        assert_eq!(s.param_names, ["author_id", "p2", "views"]);
        assert_eq!(s.writes, ["app.posts"]);

        let s = shape(&q(
            "UPDATE users SET name = $2 WHERE id = $1 AND name <> $2",
        ))
        .unwrap();
        assert_eq!(s.param_names, ["id", "name"]);
        assert_eq!(s.writes, ["users"]);

        let s = shape(&q(
            "SELECT 1 FROM users WHERE lower(email) = lower($1) AND trim($2) = name",
        ))
        .unwrap();
        assert_eq!(s.param_names, ["email", "name"]);

        let s = shape(&q("DELETE FROM posts WHERE id = $1 OR id = $2")).unwrap();
        assert_eq!(s.param_names, ["id", "id_2"]);
        assert_eq!(s.writes, ["posts"]);
    }

    #[test]
    fn param_overrides() {
        let mut query = q("SELECT * FROM t WHERE x > now() - $1::interval");
        query.param_names = vec![(1, "age".into())];
        assert_eq!(shape(&query).unwrap().param_names, ["age"]);
        query.param_names = vec![(2, "nope".into())];
        assert!(shape(&query).is_err());
    }
}
