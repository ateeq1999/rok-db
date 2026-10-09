//! Rust source for the generated crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::ir::{EnumType, Schema, Table};
use crate::types::{self, Feature};
use crate::{Error, naming};

/// Paths of generated items, shared by models and queries.
pub(crate) struct Names<'a> {
    schema: &'a Schema,
}

impl<'a> Names<'a> {
    pub(crate) fn new(schema: &'a Schema) -> Self {
        Self { schema }
    }

    /// `crate::user::UserRole` for an enum type name.
    pub(crate) fn enum_path(&self, sql_name: &str) -> Option<String> {
        let e = self
            .schema
            .enums
            .iter()
            .find(|e| e.name == sql_name || naming::unqualified(&e.name) == sql_name)?;
        Some(format!("crate::{}::{}", e.module, enum_name(e)))
    }

    /// `crate::user::User` for a table name.
    pub(crate) fn model_path(&self, table: &str) -> Option<String> {
        let t = self.schema.tables.iter().find(|t| t.name == table)?;
        Some(format!(
            "crate::{}::{}",
            t.module,
            naming::struct_name(&t.name)
        ))
    }

    /// The Rust type of a column type, `Option` when nullable.
    pub(crate) fn rust_type(
        &self,
        sql_type: &str,
        nullable: bool,
        features: &mut BTreeSet<Feature>,
    ) -> Result<String, String> {
        let ty = types::rust_type(sql_type, &|name| self.enum_path(name), features)?;
        Ok(if nullable {
            format!("Option<{ty}>")
        } else {
            ty
        })
    }
}

pub(crate) fn enum_name(e: &EnumType) -> String {
    naming::pascal(naming::unqualified(&e.name))
}

/// Field name for a column, and whether it differs from the column name.
pub(crate) fn field_name(column: &str) -> (String, bool) {
    let field = naming::snake_ident(column);
    let differs = field != column;
    (field, differs)
}

/// A relation on a parent model (`has_many` / `has_one`).
struct ChildRelation {
    name: String,
    kind: &'static str,
    target: String,
}

/// Relations implied by single-column foreign keys to single-column keys.
struct Relations {
    /// (table, column) -> (relation name, parent model path)
    belongs_to: BTreeMap<(String, String), (String, String)>,
    /// parent table -> relations
    children: BTreeMap<String, Vec<ChildRelation>>,
}

fn relations(schema: &Schema, names: &Names<'_>) -> Relations {
    let mut belongs_to = BTreeMap::new();
    let mut children: BTreeMap<String, Vec<ChildRelation>> = BTreeMap::new();
    for table in &schema.tables {
        for fk in &table.foreign_keys {
            let Some(parent) = schema.tables.iter().find(|t| t.name == fk.ref_table) else {
                continue;
            };
            if fk.columns.len() != 1
                || parent.primary_key.len() != 1
                || fk.ref_columns != parent.primary_key
            {
                continue;
            }
            let column = &fk.columns[0];
            let relation = match column.strip_suffix("_id") {
                Some(stem) if !stem.is_empty() => naming::snake_ident(stem),
                _ => format!("{}_ref", naming::snake_ident(column)),
            };
            let (Some(parent_path), Some(child_path)) = (
                names.model_path(&parent.name),
                names.model_path(&table.name),
            ) else {
                continue;
            };
            belongs_to.insert(
                (table.name.clone(), column.clone()),
                (relation.clone(), parent_path),
            );

            let to_same_parent = table
                .foreign_keys
                .iter()
                .filter(|f| f.ref_table == parent.name && f.columns.len() == 1)
                .count();
            let child_word = naming::singular(naming::unqualified(&table.name));
            let unique = table.is_unique(&fk.columns);
            let base = if unique {
                naming::snake_ident(&child_word)
            } else {
                naming::plural(&naming::snake_ident(&child_word))
            };
            let name = if to_same_parent > 1 || table.name == parent.name {
                format!("{base}_by_{relation}")
            } else {
                base
            };
            let (field, _) = field_name(column);
            children
                .entry(parent.name.clone())
                .or_default()
                .push(ChildRelation {
                    name,
                    kind: if unique { "has_one" } else { "has_many" },
                    target: format!("{child_path}::{}", naming::screaming(&field)),
                });
        }
    }
    Relations {
        belongs_to,
        children,
    }
}

/// The models and enums of each module, keyed by module name.
pub(crate) fn models(
    schema: &Schema,
    features: &mut BTreeSet<Feature>,
) -> Result<BTreeMap<String, String>, Error> {
    let names = Names::new(schema);
    let relations = relations(schema, &names);
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for e in &schema.enums {
        let code = out.entry(e.module.clone()).or_default();
        write_enum(code, e);
    }
    for table in &schema.tables {
        let code = out.entry(table.module.clone()).or_default();
        write_model(code, table, &names, &relations, features)
            .map_err(|message| Error::new(format!("table `{}`: {message}", table.name)))?;
    }
    Ok(out)
}

fn doc_lines(code: &mut String, doc: &[String], fallback: &str) {
    if doc.is_empty() {
        let _ = writeln!(code, "/// {fallback}");
    } else {
        for line in doc {
            let _ = writeln!(code, "/// {line}");
        }
    }
}

fn write_enum(code: &mut String, e: &EnumType) {
    doc_lines(code, &e.doc, &format!("The `{}` enum type.", e.name));
    let _ = writeln!(
        code,
        "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, rok_db::DbEnum)]\n#[rok(type_name = {:?})]\npub enum {} {{",
        e.name,
        enum_name(e)
    );
    let mut used = BTreeSet::new();
    for value in &e.values {
        let mut variant = naming::pascal(value);
        while !used.insert(variant.clone()) {
            variant.push('_');
        }
        let _ = writeln!(
            code,
            "    /// `{value}`\n    #[rok(rename = {value:?})]\n    {variant},"
        );
    }
    code.push_str("}\n\n");
}

fn write_model(
    code: &mut String,
    table: &Table,
    names: &Names<'_>,
    relations: &Relations,
    features: &mut BTreeSet<Feature>,
) -> Result<(), String> {
    let struct_name = naming::struct_name(&table.name);
    let has = |name: &str, types: &[&str]| {
        table
            .column(name)
            .is_some_and(|c| types.contains(&c.sql_type.as_str()))
    };
    let stamps = ["TIMESTAMPTZ", "TIMESTAMP"];
    let created = has("created_at", &stamps);
    let updated = has("updated_at", &stamps);
    let timestamps = created && updated;
    let soft_delete = table
        .column("deleted_at")
        .is_some_and(|c| c.nullable && stamps.contains(&c.sql_type.as_str()));
    let version = has("version", &["INTEGER", "BIGINT", "SMALLINT"]);

    let mut attrs = vec![format!("table = {:?}", table.name)];
    if timestamps {
        attrs.push("timestamps".to_owned());
    }
    if soft_delete {
        attrs.push("soft_delete".to_owned());
    }
    for child in relations.children.get(&table.name).into_iter().flatten() {
        attrs.push(format!("{}({} = {})", child.kind, child.name, child.target));
    }

    doc_lines(
        code,
        &table.doc,
        &format!("A row of the `{}` table.", table.name),
    );
    let _ = writeln!(
        code,
        "#[derive(Debug, Clone, PartialEq, rok_db::Model)]\n#[rok({})]\npub struct {struct_name} {{",
        attrs.join(", ")
    );
    for column in &table.columns {
        let (field, renamed) = field_name(&column.name);
        let mut field_attrs = Vec::new();
        if renamed {
            field_attrs.push(format!("column = {:?}", column.name));
        }
        if table.primary_key.contains(&column.name) {
            field_attrs.push("primary_key".to_owned());
        }
        let managed = match column.name.as_str() {
            "created_at" | "updated_at" if timestamps => true,
            "created_at" if created => {
                field_attrs.push("created_at".to_owned());
                true
            }
            "updated_at" if updated => {
                field_attrs.push("updated_at".to_owned());
                true
            }
            "deleted_at" if soft_delete => true,
            "version" if version => {
                field_attrs.push("version".to_owned());
                true
            }
            _ => false,
        };
        // Serial, identity and computed columns are filled in by PostgreSQL,
        // and so is a key with a DEFAULT. Other defaults only apply to
        // inserts that leave the column out, so the field stays writable.
        let database_value = types::is_serial(&column.sql_type)
            || column.identity.is_some()
            || column.generated.is_some()
            || (table.primary_key.contains(&column.name) && column.default.is_some());
        if database_value && !managed {
            field_attrs.push("generated".to_owned());
        }
        if column.tenant {
            field_attrs.push("tenant".to_owned());
        }
        if let Some((relation, parent)) = relations
            .belongs_to
            .get(&(table.name.clone(), column.name.clone()))
        {
            field_attrs.push(format!("belongs_to({relation} = {parent})"));
        }
        let ty = names
            .rust_type(&column.sql_type, column.nullable, features)
            .map_err(|e| format!("column `{}`: {e}", column.name))?;
        for line in &column.doc {
            let _ = writeln!(code, "    /// {line}");
        }
        if column.doc.is_empty() {
            let _ = writeln!(
                code,
                "    /// `{}` ({}).",
                column.name,
                describe_column(column)
            );
        }
        if !field_attrs.is_empty() {
            let _ = writeln!(code, "    #[rok({})]", field_attrs.join(", "));
        }
        let _ = writeln!(code, "    pub {field}: {ty},");
    }
    code.push_str("}\n\n");
    Ok(())
}

fn describe_column(column: &crate::ir::Column) -> String {
    let mut s = column.sql_type.clone();
    if !column.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(d) = &column.default {
        let _ = write!(s, " DEFAULT {d}");
    }
    s
}

/// Format Rust source with prettyplease, prefixed with `header` comments.
pub(crate) fn format(source: &str, header: &str) -> Result<String, Error> {
    let file = syn::parse_file(source).map_err(|e| {
        Error::new(format!(
            "rok-db-gen produced invalid Rust ({e}); please report this with your .sql files.\n{source}"
        ))
    })?;
    Ok(format!("{header}\n{}", prettyplease::unparse(&file)))
}

/// A raw string literal that can hold `s`.
pub(crate) fn raw_string(s: &str) -> String {
    let mut hashes = 1;
    while s.contains(&format!("\"{}", "#".repeat(hashes))) {
        hashes += 1;
    }
    let h = "#".repeat(hashes);
    format!("r{h}\"{s}\"{h}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{SourceFile, parse};

    #[test]
    fn models_with_relations_and_managed_columns() {
        let parsed = parse(&[
            SourceFile {
                path: "db/user.sql".into(),
                module: "user".into(),
                text: "CREATE TYPE user_role AS ENUM ('admin', 'in review');
                       CREATE TABLE users (id BIGSERIAL PRIMARY KEY, email TEXT NOT NULL UNIQUE,
                         role user_role NOT NULL DEFAULT 'admin', type TEXT,
                         manager_id BIGINT REFERENCES users,
                         created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                         deleted_at TIMESTAMPTZ);"
                    .into(),
            },
            SourceFile {
                path: "db/post.sql".into(),
                module: "post".into(),
                text: "CREATE TABLE posts (id BIGSERIAL PRIMARY KEY, author_id BIGINT NOT NULL REFERENCES users (id),
                         editor_id BIGINT REFERENCES users (id), tags TEXT[] NOT NULL);"
                    .into(),
            },
        ])
        .unwrap();
        let mut features = BTreeSet::new();
        let models = models(&parsed.schema, &mut features).unwrap();
        let user = squash(&format(&models["user"], "").unwrap());
        assert!(
            user.contains(&squash("#[rok(type_name = \"user_role\")]")),
            "{user}"
        );
        assert!(
            user.contains(&squash("#[rok(rename = \"in review\")]\n    InReview,")),
            "{user}"
        );
        assert!(user.contains(&squash("timestamps, soft_delete")), "{user}");
        assert!(
            user.contains(&squash(
                "has_many(posts_by_author = crate::post::Post::AUTHOR_ID)"
            )),
            "{user}"
        );
        assert!(
            user.contains(&squash(
                "has_many(users_by_manager = crate::user::User::MANAGER_ID)"
            )),
            "{user}"
        );
        assert!(
            user.contains(&squash(
                "#[rok(column = \"type\")]\n    pub type_: Option<String>,"
            )),
            "{user}"
        );
        assert!(
            user.contains(&squash("pub role: crate::user::UserRole,")),
            "{user}"
        );
        assert!(
            user.contains(&squash("#[rok(primary_key, generated)]\n    pub id: i64,")),
            "{user}"
        );
        assert!(
            !user.contains(&squash("generated)]\n    pub created_at")),
            "{user}"
        );
        let post = squash(&format(&models["post"], "").unwrap());
        assert!(
            post.contains(&squash("#[rok(belongs_to(author = crate::user::User))]")),
            "{post}"
        );
        assert!(post.contains(&squash("pub tags: Vec<String>,")), "{post}");
        assert_eq!(
            features.iter().map(|f| f.name()).collect::<Vec<_>>(),
            ["chrono"]
        );
    }

    fn squash(s: &str) -> String {
        s.split_whitespace().collect()
    }

    #[test]
    fn raw_strings_fit_their_content() {
        assert_eq!(raw_string("a"), "r#\"a\"#");
        assert_eq!(raw_string("x \"# y"), "r##\"x \"# y\"##");
    }
}
