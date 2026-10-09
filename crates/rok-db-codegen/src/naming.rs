//! Naming rules: file stems to modules, tables to structs, columns to fields.

/// Rust keywords (strict and reserved), which get a trailing `_`.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "abstract", "become", "box", "do", "final", "gen", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// Output file names the generator writes itself.
pub(crate) const RESERVED_MODULES: &[&str] = &["lib", "up", "down", "migrations", "mod", "types"];

/// A snake_case Rust identifier for `name`: other characters become `_`,
/// a leading digit gets a `_` prefix and keywords get a `_` suffix.
pub(crate) fn snake_ident(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev_lower {
                out.push('_');
            }
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
            out.push(c.to_ascii_lowercase());
        } else {
            prev_lower = false;
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
        }
    }
    let mut out = out.trim_end_matches('_').to_owned();
    if out.is_empty() {
        out.push('_');
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// PascalCase for `name` (`blog_post` -> `BlogPost`).
pub(crate) fn pascal(name: &str) -> String {
    let mut out = String::new();
    for part in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'T');
    }
    if out == "Self" {
        out.push('_');
    }
    out
}

/// The constant the `Model` derive generates for a field (`type_` -> `TYPE_`).
pub(crate) fn screaming(field: &str) -> String {
    field.to_ascii_uppercase()
}

/// Singular of an English plural (`users` -> `user`, `categories` ->
/// `category`, `addresses` -> `address`). Unknown forms are returned as is.
pub(crate) fn singular(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let irregular = [
        ("people", "person"),
        ("children", "child"),
        ("men", "man"),
        ("women", "woman"),
        ("data", "datum"),
    ];
    for (plural, single) in irregular {
        if lower == plural {
            return single.to_owned();
        }
        if let Some(prefix) = lower.strip_suffix(&format!("_{plural}")) {
            return format!("{prefix}_{single}");
        }
    }
    if let Some(stem) = word.strip_suffix("ies") {
        if !stem.is_empty() {
            return format!("{stem}y");
        }
    }
    for suffix in ["sses", "shes", "ches", "xes", "zes"] {
        if word.ends_with(suffix) {
            return word[..word.len() - 2].to_owned();
        }
    }
    if word.ends_with("ss") || word.ends_with("us") || word.ends_with("is") {
        return word.to_owned();
    }
    word.strip_suffix('s').unwrap_or(word).to_owned()
}

/// Plural of a singular English word, for relation names.
pub(crate) fn plural(word: &str) -> String {
    if let Some(stem) = word.strip_suffix('y') {
        if !stem.ends_with(['a', 'e', 'i', 'o', 'u']) {
            return format!("{stem}ies");
        }
    }
    if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|s| word.ends_with(s))
    {
        return format!("{word}es");
    }
    format!("{word}s")
}

/// The unqualified part of a possibly schema-qualified table name.
pub(crate) fn unqualified(table: &str) -> &str {
    table.rsplit('.').next().unwrap_or(table)
}

/// Struct name for a table (`app.blog_posts` -> `BlogPost`).
pub(crate) fn struct_name(table: &str) -> String {
    pascal(&singular(unqualified(table)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers() {
        assert_eq!(snake_ident("user-profile"), "user_profile");
        assert_eq!(snake_ident("type"), "type_");
        assert_eq!(snake_ident("2024 users"), "_2024_users");
        assert_eq!(snake_ident("userId"), "user_id");
        assert_eq!(snake_ident("ID"), "id");
        assert_eq!(pascal("blog_post"), "BlogPost");
        assert_eq!(screaming("type_"), "TYPE_");
    }

    #[test]
    fn plurals() {
        for (p, s) in [
            ("users", "user"),
            ("categories", "category"),
            ("addresses", "address"),
            ("boxes", "box"),
            ("status", "status"),
            ("people", "person"),
            ("blog_posts", "blog_post"),
            ("user_data", "user_datum"),
        ] {
            assert_eq!(singular(p), s, "{p}");
        }
        assert_eq!(plural("category"), "categories");
        assert_eq!(plural("post"), "posts");
        assert_eq!(plural("box"), "boxes");
        assert_eq!(struct_name("app.blog_posts"), "BlogPost");
    }
}
