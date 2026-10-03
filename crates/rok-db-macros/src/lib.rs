//! Derive macros for [rok-db](https://docs.rs/rok-db). Use them through the
//! `rok-db` crate rather than depending on this crate directly.

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident, LitStr, Path, parse_macro_input, spanned::Spanned};

/// Derive `rok_db::Model` (and `sqlx::FromRow`) for a struct with named fields.
///
/// # Container attributes
///
/// - `#[rok(table = "users")]` — table name, optionally schema-qualified.
///   Defaults to the struct name in `snake_case`, pluralised
///   (`User` → `users`, `BlogPost` → `blog_posts`, `Category` → `categories`).
/// - `#[rok(timestamps)]` — the `created_at` and `updated_at` fields are
///   managed automatically: both are set to `now()` on insert and
///   `updated_at` on every update.
/// - `#[rok(soft_delete)]` — the `deleted_at` field (an `Option` timestamp)
///   marks deleted rows: `delete` sets it, queries skip such rows unless
///   `with_trashed()`/`only_trashed()` is used, and `restore` clears it.
/// - `#[rok(has_many(posts = Post::USER_ID))]` — a one-to-many relation:
///   generates `User::POSTS` (a [`HasMany`]) and `user.posts()` (a query).
/// - `#[rok(has_one(profile = Profile::USER_ID))]` — a one-to-one relation:
///   generates `User::PROFILE` and `user.profile()`.
/// - `#[rok(hooks)]` — you implement `rok_db::Hooks` yourself (otherwise a
///   no-op implementation is generated).
/// - `#[rok(validate_with = path::to::fn)]` — record-level validation,
///   `fn(&Self) -> Result<(), ValidationErrors>`, run with the field rules.
/// - `#[rok(default_scope = path::to::fn)]` — `fn() -> Expr<Self>` applied
///   to every query; remove it with `.unscoped()`.
/// - `#[rok(no_from_row)]` — don't generate `FromRow` (bring your own).
/// - `#[rok(crate = "path::to::rok_db")]` — path of the `rok_db` crate when
///   it is re-exported under another name.
///
/// # Field attributes
///
/// - `#[rok(primary_key)]` — the primary key. Defaults to the field named `id`.
///   Mark several fields for a composite key (in field order); look records
///   up with a tuple: `Membership::find(&db, (org_id, user_id))`.
/// - `#[rok(generated)]` — filled in by the database (serial ids, defaults,
///   triggers): read, but never written by `insert`/`save`.
/// - `#[rok(column = "name")]` — column name, if it differs from the field.
/// - `#[rok(skip)]` — not a column; initialised with `Default::default()`.
/// - `#[rok(created_at)]` / `#[rok(updated_at)]` — managed timestamp column
///   with a custom name (see `timestamps`).
/// - `#[rok(deleted_at)]` — soft-delete column with a custom name.
/// - `#[rok(tenant)]` — the tenant column for row-level multi-tenancy
///   (see `rok_db::tenant`).
/// - `#[rok(version)]` — optimistic-locking counter (an integer): every
///   update increments it, and `save`/`delete` fail with a conflict error if
///   it changed since the record was loaded.
/// - `#[rok(validate(…))]` — validation rules checked before every insert
///   and save: `length(min = 1, max = 50)`, `range(min = 0, max = 150)`,
///   `email`, `non_empty`, `custom = path::to::fn` (`fn(&T) -> Result<(), String>`).
///   Rules on `Option` fields apply only to `Some`.
/// - `#[rok(belongs_to = User)]` on a foreign key field `user_id` — generates
///   `Post::USER` (a [`BelongsTo`]) and `post.user()`. Use
///   `#[rok(belongs_to(author = User))]` to pick the name.
///
/// [`HasMany`]: https://docs.rs/rok-db/latest/rok_db/struct.HasMany.html
/// [`BelongsTo`]: https://docs.rs/rok-db/latest/rok_db/struct.BelongsTo.html
///
/// # Generated items
///
/// Besides the trait impls, one typed column constant is generated per
/// column, named after the field in `SCREAMING_SNAKE_CASE`:
///
/// ```ignore
/// #[derive(Model)]
/// struct User { #[rok(primary_key, generated)] id: i64, email: String }
///
/// User::filter(User::EMAIL.eq("ann@example.com"))
/// ```
#[proc_macro_derive(Model, attributes(rok))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct Field {
    ident: Ident,
    column: String,
    primary_key: bool,
    generated: bool,
    skip: bool,
    created_at: bool,
    updated_at: bool,
    deleted_at: bool,
    version: bool,
    tenant: bool,
    belongs_to: Option<(Ident, Path)>,
    rules: Vec<Rule>,
}

/// A `#[rok(validate(…))]` rule.
enum Rule {
    Length(Option<syn::Expr>, Option<syn::Expr>),
    Range(Option<syn::Expr>, Option<syn::Expr>),
    Email,
    NonEmpty,
    Custom(Path),
}

fn parse_bounds(
    meta: &syn::meta::ParseNestedMeta<'_>,
) -> syn::Result<(Option<syn::Expr>, Option<syn::Expr>)> {
    let (mut min, mut max) = (None, None);
    meta.parse_nested_meta(|b| {
        if b.path.is_ident("min") {
            min = Some(b.value()?.parse()?);
        } else if b.path.is_ident("max") {
            max = Some(b.value()?.parse()?);
        } else {
            return Err(b.error("expected `min` or `max`"));
        }
        Ok(())
    })?;
    if min.is_none() && max.is_none() {
        return Err(meta.error("expected at least one of `min`, `max`"));
    }
    Ok((min, max))
}

fn parse_rules(meta: &syn::meta::ParseNestedMeta<'_>, rules: &mut Vec<Rule>) -> syn::Result<()> {
    meta.parse_nested_meta(|r| {
        if r.path.is_ident("length") {
            let (min, max) = parse_bounds(&r)?;
            rules.push(Rule::Length(min, max));
        } else if r.path.is_ident("range") {
            let (min, max) = parse_bounds(&r)?;
            rules.push(Rule::Range(min, max));
        } else if r.path.is_ident("email") {
            rules.push(Rule::Email);
        } else if r.path.is_ident("non_empty") {
            rules.push(Rule::NonEmpty);
        } else if r.path.is_ident("custom") {
            rules.push(Rule::Custom(r.value()?.parse()?));
        } else {
            return Err(r.error(
                "unknown rule; expected `length`, `range`, `email`, `non_empty` or `custom`",
            ));
        }
        Ok(())
    })
}

/// `has_many(name = Model::FK_COLUMN)` / `has_one(…)`.
struct Relation {
    kind: &'static str,
    name: Ident,
    model: Path,
    foreign_key: Path,
}

fn parse_relations(
    meta: &syn::meta::ParseNestedMeta<'_>,
    kind: &'static str,
    out: &mut Vec<Relation>,
) -> syn::Result<()> {
    meta.parse_nested_meta(|inner| {
        let name = inner
            .path
            .get_ident()
            .cloned()
            .ok_or_else(|| inner.error("expected `name = Model::FOREIGN_KEY`"))?;
        let foreign_key: Path = inner.value()?.parse()?;
        let mut model = foreign_key.clone();
        if model.segments.len() < 2 {
            return Err(syn::Error::new(
                foreign_key.span(),
                "expected a column constant such as `Post::USER_ID`",
            ));
        }
        model.segments.pop();
        model.segments.pop_punct();
        out.push(Relation {
            kind,
            name,
            model,
            foreign_key,
        });
        Ok(())
    })
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "`#[derive(Model)]` does not support generic structs",
        ));
    }

    let mut table = None;
    let mut timestamps = false;
    let mut soft_delete = false;
    let mut hooks = false;
    let mut validate_with: Option<Path> = None;
    let mut default_scope: Option<Path> = None;
    let mut relations = Vec::new();
    let mut from_row = true;
    let mut krate: Option<Path> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("rok")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("timestamps") {
                timestamps = true;
            } else if meta.path.is_ident("soft_delete") {
                soft_delete = true;
            } else if meta.path.is_ident("hooks") {
                hooks = true;
            } else if meta.path.is_ident("validate_with") {
                validate_with = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("default_scope") {
                default_scope = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("has_many") {
                parse_relations(&meta, "HasMany", &mut relations)?;
            } else if meta.path.is_ident("has_one") {
                parse_relations(&meta, "HasOne", &mut relations)?;
            } else if meta.path.is_ident("no_from_row") {
                from_row = false;
            } else if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse::<LitStr>()?.parse()?);
            } else {
                return Err(meta
                    .error("unknown `rok` attribute; expected `table`, `no_from_row` or `crate`"));
            }
            Ok(())
        })?;
    }
    let table = table.unwrap_or_else(|| pluralize(&snake_case(&name.to_string())));
    let krate = match krate {
        Some(path) => quote!(#path),
        None => crate_path(),
    };

    let fields = parse_fields(&input, "Model")?;

    let columns: Vec<&Field> = fields.iter().filter(|f| !f.skip).collect();
    let mut pks: Vec<&Field> = columns.iter().filter(|f| f.primary_key).copied().collect();
    if pks.is_empty() {
        pks.push(*columns.iter().find(|f| f.ident == "id").ok_or_else(|| {
            syn::Error::new(
                name.span(),
                "no primary key: add a field named `id` or mark one or more fields with `#[rok(primary_key)]`",
            )
        })?);
    }
    let pk = pks[0];
    let pk_columns = pks.iter().map(|f| &f.column);

    let find_ts = |flag: fn(&Field) -> bool,
                   default: &str,
                   implied: bool,
                   container: &str|
     -> syn::Result<Option<String>> {
        let mut marked = columns.iter().filter(|f| flag(f));
        match (marked.next(), marked.next()) {
            (Some(_), Some(second)) => Err(syn::Error::new(
                second.ident.span(),
                format!("only one field can be `#[rok({default})]`"),
            )),
            (Some(f), None) => Ok(Some(f.column.clone())),
            (None, _) if implied => columns
                .iter()
                .find(|f| f.ident == default)
                .map(|f| Some(f.column.clone()))
                .ok_or_else(|| {
                    syn::Error::new(
                        name.span(),
                        format!("`#[rok({container})]` requires a `{default}` field (or mark one with `#[rok({default})]`)"),
                    )
                }),
            (None, _) => Ok(None),
        }
    };
    let created_at = find_ts(|f| f.created_at, "created_at", timestamps, "timestamps")?;
    let updated_at = find_ts(|f| f.updated_at, "updated_at", timestamps, "timestamps")?;
    let deleted_at = find_ts(|f| f.deleted_at, "deleted_at", soft_delete, "soft_delete")?;
    let version = find_ts(|f| f.version, "version", false, "")?;
    let tenant = find_ts(|f| f.tenant, "tenant", false, "")?;
    let opt = |v: Option<String>| match v {
        Some(c) => quote!(::core::option::Option::Some(#c)),
        None => quote!(::core::option::Option::None),
    };
    let created_at = opt(created_at);
    let updated_at = opt(updated_at);
    let deleted_at = opt(deleted_at);
    let version = opt(version);
    let tenant = opt(tenant);

    let value_arms = columns.iter().map(|f| {
        let ident = &f.ident;
        let column = &f.column;
        quote!(#column => ::core::option::Option::Some(#krate::Value::from(&self.#ident)))
    });

    let mut relation_items = Vec::new();
    for f in &columns {
        if let Some((rel, model)) = &f.belongs_to {
            let const_name = format_ident!("{}", unraw(rel).to_uppercase(), span = rel.span());
            let fk_const =
                format_ident!("{}", unraw(&f.ident).to_uppercase(), span = f.ident.span());
            let doc = format!(
                "`{name}` belongs to [`{}`] through `{}`.",
                quote!(#model).to_string().replace(' ', ""),
                f.column
            );
            relation_items.push(quote! {
                #[doc = #doc]
                #[allow(dead_code)]
                pub const #const_name: #krate::BelongsTo<#name, #model> = #krate::BelongsTo::new(Self::#fk_const);

                #[doc = #doc]
                #[allow(dead_code)]
                pub fn #rel(&self) -> #krate::Select<#model> {
                    Self::#const_name.query(self)
                }
            });
        }
    }
    for r in &relations {
        let Relation {
            kind,
            name: rel,
            model,
            foreign_key,
        } = r;
        let kind = Ident::new(kind, Span::call_site());
        let const_name = format_ident!("{}", unraw(rel).to_uppercase(), span = rel.span());
        let doc = format!(
            "`{name}` → [`{}`] through `{}`.",
            quote!(#model).to_string().replace(' ', ""),
            quote!(#foreign_key).to_string().replace(' ', "")
        );
        relation_items.push(quote! {
            #[doc = #doc]
            #[allow(dead_code)]
            pub const #const_name: #krate::#kind<#name, #model> = #krate::#kind::new(#foreign_key);

            #[doc = #doc]
            #[allow(dead_code)]
            pub fn #rel(&self) -> #krate::Select<#model> {
                Self::#const_name.query(self)
            }
        });
    }

    let validations: Vec<TokenStream2> = columns
        .iter()
        .flat_map(|f| {
            let ident = &f.ident;
            let field = unraw(ident);
            let v = quote!(#krate::validate);
            f.rules.iter().map(move |rule| {
                let opt = |e: &Option<syn::Expr>, cast: TokenStream2| match e {
                    Some(e) => quote!(::core::option::Option::Some((#e) as #cast)),
                    None => quote!(::core::option::Option::None),
                };
                let (code, check) = match rule {
                    Rule::Length(min, max) => {
                        let (min, max) = (opt(min, quote!(usize)), opt(max, quote!(usize)));
                        ("length", quote!(#v::length(&self.#ident, #min, #max)))
                    }
                    Rule::Range(min, max) => {
                        let (min, max) = (opt(min, quote!(f64)), opt(max, quote!(f64)));
                        ("range", quote!(#v::range(&self.#ident, #min, #max)))
                    }
                    Rule::Email => ("email", quote!(#v::email(&self.#ident))),
                    Rule::NonEmpty => ("non_empty", quote!(#v::non_empty(&self.#ident))),
                    Rule::Custom(path) => ("custom", quote!(#path(&self.#ident))),
                };
                quote! {
                    if let ::core::result::Result::Err(message) = #check {
                        errors.add(#field, #code, message);
                    }
                }
            })
        })
        .collect();
    let validate_fn = (!validations.is_empty() || validate_with.is_some()).then(|| {
        let record = validate_with.as_ref().map(|path| {
            quote! {
                if let ::core::result::Result::Err(more) = #path(self) {
                    errors.merge(more);
                }
            }
        });
        quote! {
            fn validate(&self) -> ::core::result::Result<(), #krate::ValidationErrors> {
                let mut errors = #krate::ValidationErrors::new();
                #(#validations)*
                #record
                errors.into_result()
            }
        }
    });
    let default_scope_fn = default_scope.map(|path| {
        quote! {
            fn default_scope() -> ::core::option::Option<#krate::Expr<Self>> {
                ::core::option::Option::Some(#path())
            }
        }
    });
    let hooks_impl = (!hooks).then(|| quote!(impl #krate::Hooks for #name {}));

    let pk_column = &pk.column;
    let pk_ident = &pk.ident;
    let column_names = columns.iter().map(|f| &f.column);
    let generated = columns.iter().filter(|f| f.generated).map(|f| &f.column);
    let values = columns.iter().map(|f| {
        let ident = &f.ident;
        let column = &f.column;
        quote!((#column, #krate::Value::from(&self.#ident)))
    });
    let consts = columns.iter().map(|f| {
        let const_name = format_ident!("{}", unraw(&f.ident).to_uppercase(), span = f.ident.span());
        let column = &f.column;
        let doc = format!("The `{column}` column.");
        quote! {
            #[doc = #doc]
            #[allow(dead_code)]
            pub const #const_name: #krate::Column<#name> = #krate::Column::new(#column);
        }
    });

    let from_row_impl = from_row.then(|| from_row_tokens(name, &fields, &krate));

    Ok(quote! {
        #from_row_impl
        #hooks_impl

        impl #name {
            #(#consts)*
            #(#relation_items)*
        }

        impl #krate::Model for #name {
            const TABLE: &'static str = #table;
            const PRIMARY_KEY: &'static str = #pk_column;
            const PRIMARY_KEYS: &'static [&'static str] = &[#(#pk_columns),*];
            const COLUMNS: &'static [&'static str] = &[#(#column_names),*];
            const GENERATED: &'static [&'static str] = &[#(#generated),*];
            const CREATED_AT_COLUMN: ::core::option::Option<&'static str> = #created_at;
            const UPDATED_AT_COLUMN: ::core::option::Option<&'static str> = #updated_at;
            const DELETED_AT_COLUMN: ::core::option::Option<&'static str> = #deleted_at;
            const VERSION_COLUMN: ::core::option::Option<&'static str> = #version;
            const TENANT_COLUMN: ::core::option::Option<&'static str> = #tenant;

            fn primary_key(&self) -> #krate::Value {
                #krate::Value::from(&self.#pk_ident)
            }

            fn values(&self) -> ::std::vec::Vec<(&'static str, #krate::Value)> {
                ::std::vec![#(#values),*]
            }

            #validate_fn
            #default_scope_fn

            fn value_of(&self, column: &str) -> ::core::option::Option<#krate::Value> {
                match column {
                    #(#value_arms,)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    })
}

fn parse_fields(input: &DeriveInput, derive: &str) -> syn::Result<Vec<Field>> {
    let named = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return Err(syn::Error::new(
                    input.ident.span(),
                    format!("`#[derive({derive})]` requires a struct with named fields"),
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                input.ident.span(),
                format!("`#[derive({derive})]` can only be used on structs"),
            ));
        }
    };

    let mut fields = Vec::new();
    for field in named {
        let ident = field.ident.clone().expect("named field");
        let mut f = Field {
            column: unraw(&ident),
            ident,
            primary_key: false,
            generated: false,
            skip: false,
            created_at: false,
            updated_at: false,
            deleted_at: false,
            version: false,
            tenant: false,
            belongs_to: None,
            rules: Vec::new(),
        };
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("rok")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("primary_key") {
                    f.primary_key = true;
                } else if meta.path.is_ident("generated") || meta.path.is_ident("auto") {
                    f.generated = true;
                } else if meta.path.is_ident("skip") {
                    f.skip = true;
                } else if meta.path.is_ident("created_at") {
                    f.created_at = true;
                } else if meta.path.is_ident("updated_at") {
                    f.updated_at = true;
                } else if meta.path.is_ident("deleted_at") {
                    f.deleted_at = true;
                } else if meta.path.is_ident("version") {
                    f.version = true;
                } else if meta.path.is_ident("tenant") {
                    f.tenant = true;
                } else if meta.path.is_ident("validate") {
                    parse_rules(&meta, &mut f.rules)?;
                } else if meta.path.is_ident("belongs_to") {
                    if meta.input.peek(syn::Token![=]) {
                        let model: Path = meta.value()?.parse()?;
                        let field = unraw(&f.ident);
                        let name = field.strip_suffix("_id").ok_or_else(|| {
                            meta.error(format!(
                                "can't derive a relation name from `{field}`; use `belongs_to(name = Model)`"
                            ))
                        })?;
                        f.belongs_to = Some((Ident::new(name, f.ident.span()), model));
                    } else {
                        meta.parse_nested_meta(|inner| {
                            let name = inner.path.get_ident().cloned().ok_or_else(|| {
                                inner.error("expected `belongs_to(name = Model)`")
                            })?;
                            let model: Path = inner.value()?.parse()?;
                            f.belongs_to = Some((name, model));
                            Ok(())
                        })?;
                    }
                } else if meta.path.is_ident("column") || meta.path.is_ident("rename") {
                    f.column = meta.value()?.parse::<LitStr>()?.value();
                } else {
                    return Err(meta.error(
                        "unknown `rok` attribute; expected `primary_key`, `generated`, `column`, `skip`, `created_at`, `updated_at`, `deleted_at`, `version`, `tenant`, `validate` or `belongs_to`",
                    ));
                }
                Ok(())
            })?;
        }
        let managed = f.created_at || f.updated_at || f.deleted_at || f.version || f.tenant;
        if f.skip
            && (f.primary_key
                || f.generated
                || managed
                || f.belongs_to.is_some()
                || !f.rules.is_empty())
        {
            return Err(syn::Error::new(
                f.ident.span(),
                "`skip` fields are not columns and can't have other `rok` attributes",
            ));
        }
        if managed && f.generated {
            return Err(syn::Error::new(
                f.ident.span(),
                "managed columns (timestamps, `deleted_at`, `version`) are written by rok-db and can't be `generated`",
            ));
        }
        fields.push(f);
    }
    Ok(fields)
}

fn from_row_tokens(name: &Ident, fields: &[Field], krate: &TokenStream2) -> TokenStream2 {
    let inits = fields.iter().map(|f| {
        let ident = &f.ident;
        let column = &f.column;
        if f.skip {
            quote!(#ident: ::core::default::Default::default())
        } else {
            quote!(#ident: #krate::__private::Row::try_get(row, #column)?)
        }
    });
    quote! {
        impl<'r> #krate::__private::FromRow<'r, #krate::__private::PgRow> for #name {
            fn from_row(
                row: &'r #krate::__private::PgRow,
            ) -> ::core::result::Result<Self, #krate::__private::SqlxError> {
                ::core::result::Result::Ok(Self { #(#inits,)* })
            }
        }
    }
}

/// Derive `sqlx::FromRow` for decoding query results into a plain struct,
/// e.g. the output of [`Select::select`] with aliased projections — without
/// depending on sqlx directly.
///
/// Fields are read by name; `#[rok(column = "name")]` and `#[rok(skip)]`
/// work as on [`Model`](derive@Model), and `#[rok(crate = "…")]` sets the
/// crate path.
///
/// ```ignore
/// #[derive(rok_db::FromRow)]
/// struct RoleStats { role: String, users: i64 }
/// ```
///
/// [`Select::select`]: https://docs.rs/rok-db/latest/rok_db/struct.Select.html#method.select
#[proc_macro_derive(FromRow, attributes(rok))]
pub fn derive_from_row(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_from_row(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_from_row(input: DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "`#[derive(FromRow)]` does not support generic structs",
        ));
    }
    let mut krate: Option<Path> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("rok")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse::<LitStr>()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("unknown `rok` attribute for `FromRow`; expected `crate`"))
            }
        })?;
    }
    let krate = match krate {
        Some(path) => quote!(#path),
        None => crate_path(),
    };
    let fields = parse_fields(&input, "FromRow")?;
    if let Some(f) = fields.iter().find(|f| {
        f.primary_key
            || f.generated
            || f.created_at
            || f.updated_at
            || f.deleted_at
            || f.version
            || f.tenant
            || f.belongs_to.is_some()
            || !f.rules.is_empty()
    }) {
        return Err(syn::Error::new(
            f.ident.span(),
            "only `column` and `skip` apply to `#[derive(FromRow)]`",
        ));
    }
    Ok(from_row_tokens(&input.ident, &fields, &krate))
}

fn parse_crate_attr(
    attrs: &[syn::Attribute],
    allowed: &[&str],
    mut other: impl FnMut(&syn::meta::ParseNestedMeta<'_>) -> syn::Result<bool>,
) -> syn::Result<TokenStream2> {
    let mut krate: Option<Path> = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("rok")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse::<LitStr>()?.parse()?);
                Ok(())
            } else if other(&meta)? {
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unknown `rok` attribute; expected one of: crate, {}",
                    allowed.join(", ")
                )))
            }
        })?;
    }
    Ok(match krate {
        Some(path) => quote!(#path),
        None => crate_path(),
    })
}

fn rename(name: &str, rule: &str) -> syn::Result<String> {
    let snake = snake_case(name);
    Ok(match rule {
        "snake_case" => snake,
        "SCREAMING_SNAKE_CASE" => snake.to_uppercase(),
        "kebab-case" => snake.replace('_', "-"),
        "lowercase" => name.to_lowercase(),
        "UPPERCASE" => name.to_uppercase(),
        "PascalCase" => name.to_owned(),
        _ => {
            return Err(syn::Error::new(
                Span::call_site(),
                "`rename_all` must be one of snake_case, SCREAMING_SNAKE_CASE, kebab-case, lowercase, UPPERCASE, PascalCase",
            ));
        }
    })
}

/// Derive a column type for a field-less enum, stored as `TEXT` (default)
/// or as a native PostgreSQL enum.
///
/// - `#[rok(type_name = "mood")]` — use the PostgreSQL enum type `mood`
///   (`CREATE TYPE mood AS ENUM (…)`) instead of `TEXT`.
/// - `#[rok(rename_all = "snake_case")]` — how variant names are stored
///   (default `snake_case`; also `SCREAMING_SNAKE_CASE`, `kebab-case`,
///   `lowercase`, `UPPERCASE`, `PascalCase`).
/// - `#[rok(rename = "…")]` on a variant — explicit stored name.
///
/// Also generates `as_str()`, `Display` and `FromStr`, and makes the enum
/// usable in models, filters and `set` (it needs `Clone + Debug`).
///
/// ```ignore
/// #[derive(Debug, Clone, Copy, PartialEq, DbEnum)]
/// enum Role { Admin, Member, #[rok(rename = "ro")] ReadOnly }
///
/// User::filter(User::ROLE.eq(Role::Admin))
/// ```
#[proc_macro_derive(DbEnum, attributes(rok))]
pub fn derive_db_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_db_enum(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_db_enum(input: DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "`#[derive(DbEnum)]` does not support generics",
        ));
    }
    let mut type_name: Option<String> = None;
    let mut rule = "snake_case".to_owned();
    let krate = parse_crate_attr(&input.attrs, &["type_name", "rename_all"], |meta| {
        if meta.path.is_ident("type_name") {
            type_name = Some(meta.value()?.parse::<LitStr>()?.value());
            Ok(true)
        } else if meta.path.is_ident("rename_all") {
            rule = meta.value()?.parse::<LitStr>()?.value();
            Ok(true)
        } else {
            Ok(false)
        }
    })?;
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new(
            name.span(),
            "`#[derive(DbEnum)]` can only be used on enums",
        ));
    };
    let mut variants = Vec::new();
    for v in &data.variants {
        if !matches!(v.fields, Fields::Unit) {
            return Err(syn::Error::new(
                v.ident.span(),
                "`#[derive(DbEnum)]` variants can't have fields",
            ));
        }
        let mut stored = rename(&v.ident.to_string(), &rule)?;
        for attr in v.attrs.iter().filter(|a| a.path().is_ident("rok")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    stored = meta.value()?.parse::<LitStr>()?.value();
                    Ok(())
                } else {
                    Err(meta.error("unknown `rok` attribute on a variant; expected `rename`"))
                }
            })?;
        }
        variants.push((v.ident.clone(), stored));
    }
    let p = quote!(#krate::__private);
    let to_str = variants.iter().map(|(v, s)| quote!(#name::#v => #s));
    let from_str = variants
        .iter()
        .map(|(v, s)| quote!(#s => ::core::result::Result::Ok(#name::#v)));
    let expected = variants
        .iter()
        .map(|(_, s)| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let err = format!("invalid `{name}` value {{:?}} (expected one of: {expected})");
    let (type_info, compatible) = match &type_name {
        Some(t) => (
            quote!(#p::PgTypeInfo::with_name(#t)),
            quote!(*ty == <Self as #p::Type<#p::Postgres>>::type_info() || <::std::string::String as #p::Type<#p::Postgres>>::compatible(ty)),
        ),
        None => (
            quote!(<::std::string::String as #p::Type<#p::Postgres>>::type_info()),
            quote!(<::std::string::String as #p::Type<#p::Postgres>>::compatible(ty)),
        ),
    };
    Ok(quote! {
        impl #name {
            /// The value stored in the database.
            pub const fn as_str(&self) -> &'static str {
                match self { #(#to_str,)* }
            }
        }

        impl ::core::fmt::Display for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::core::str::FromStr for #name {
            type Err = ::std::string::String;
            fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
                match s {
                    #(#from_str,)*
                    other => ::core::result::Result::Err(::std::format!(#err, other)),
                }
            }
        }

        impl #p::Type<#p::Postgres> for #name {
            fn type_info() -> #p::PgTypeInfo { #type_info }
            fn compatible(ty: &#p::PgTypeInfo) -> bool { #compatible }
        }

        impl<'q> #p::Encode<'q, #p::Postgres> for #name {
            fn encode_by_ref(
                &self,
                buf: &mut #p::PgArgumentBuffer,
            ) -> ::core::result::Result<#p::IsNull, #p::BoxDynError> {
                <&str as #p::Encode<'q, #p::Postgres>>::encode_by_ref(&self.as_str(), buf)
            }
        }

        impl<'r> #p::Decode<'r, #p::Postgres> for #name {
            fn decode(value: #p::PgValueRef<'r>) -> ::core::result::Result<Self, #p::BoxDynError> {
                let s = <&str as #p::Decode<'r, #p::Postgres>>::decode(value)?;
                <Self as ::core::str::FromStr>::from_str(s).map_err(::core::convert::Into::into)
            }
        }

        #krate::impl_value!(#name);
    })
}

/// Derive a column type for a single-field tuple struct (a "newtype"),
/// stored exactly like its inner type:
///
/// ```ignore
/// #[derive(Debug, Clone, PartialEq, DbNewtype)]
/// struct Email(String);
///
/// #[derive(Model)]
/// struct User { id: i64, email: Email }
/// ```
///
/// The newtype must be `Clone + Debug`.
#[proc_macro_derive(DbNewtype, attributes(rok))]
pub fn derive_db_newtype(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_db_newtype(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_db_newtype(input: DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "`#[derive(DbNewtype)]` does not support generics",
        ));
    }
    let krate = parse_crate_attr(&input.attrs, &[], |_| Ok(false))?;
    let inner = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Unnamed(f) if f.unnamed.len() == 1 => f.unnamed[0].ty.clone(),
            _ => None.ok_or_else(|| {
                syn::Error::new(
                    name.span(),
                    "`#[derive(DbNewtype)]` requires a struct with exactly one unnamed field",
                )
            })?,
        },
        _ => {
            return Err(syn::Error::new(
                name.span(),
                "`#[derive(DbNewtype)]` can only be used on structs",
            ));
        }
    };
    let p = quote!(#krate::__private);
    Ok(quote! {
        impl #p::Type<#p::Postgres> for #name {
            fn type_info() -> #p::PgTypeInfo { <#inner as #p::Type<#p::Postgres>>::type_info() }
            fn compatible(ty: &#p::PgTypeInfo) -> bool { <#inner as #p::Type<#p::Postgres>>::compatible(ty) }
        }

        impl<'q> #p::Encode<'q, #p::Postgres> for #name {
            fn encode_by_ref(
                &self,
                buf: &mut #p::PgArgumentBuffer,
            ) -> ::core::result::Result<#p::IsNull, #p::BoxDynError> {
                <#inner as #p::Encode<'q, #p::Postgres>>::encode_by_ref(&self.0, buf)
            }
        }

        impl<'r> #p::Decode<'r, #p::Postgres> for #name {
            fn decode(value: #p::PgValueRef<'r>) -> ::core::result::Result<Self, #p::BoxDynError> {
                <#inner as #p::Decode<'r, #p::Postgres>>::decode(value).map(#name)
            }
        }

        #krate::impl_value!(#name);
    })
}

/// Run an async test against a fresh, temporary database (feature
/// `testing`). The database is created on the server named by
/// `DATABASE_URL`, passed in as a [`Db`], and dropped afterwards — even if
/// the test panics. Without `DATABASE_URL` the test is skipped.
///
/// ```ignore
/// #[rok_db::test(migrations = "migrations", sql = "tests/fixtures/users.sql")]
/// async fn finds_admins(db: Db) {
///     assert_eq!(User::filter(User::ROLE.eq("admin")).count(&db).await.unwrap(), 1);
/// }
/// ```
///
/// - `migrations = "dir"` — run sqlx migrations from `dir` (needs the
///   `migrate` feature); may be repeated.
/// - `sql = "file.sql"` — execute an SQL file (schema or fixtures); may be
///   repeated, runs after migrations in order.
///
/// Paths are relative to the crate's `Cargo.toml`. The test runs on
/// `#[tokio::test]`, so `tokio` (with `macros` and `rt`) must be a
/// dev-dependency. It may return `()` or a `Result<(), E>`.
///
/// [`Db`]: https://docs.rs/rok-db/latest/rok_db/struct.Db.html
#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemFn);
    expand_test(args.into(), item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_test(args: TokenStream2, item: syn::ItemFn) -> syn::Result<TokenStream2> {
    let mut migrations = Vec::new();
    let mut sql_files = Vec::new();
    let mut krate: Option<Path> = None;
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("migrations") {
            migrations.push(meta.value()?.parse::<LitStr>()?);
        } else if meta.path.is_ident("sql") {
            sql_files.push(meta.value()?.parse::<LitStr>()?);
        } else if meta.path.is_ident("crate") {
            krate = Some(meta.value()?.parse::<LitStr>()?.parse()?);
        } else {
            return Err(meta.error("expected `migrations`, `sql` or `crate`"));
        }
        Ok(())
    });
    syn::parse::Parser::parse2(parser, args)?;
    let krate = match krate {
        Some(path) => quote!(#path),
        None => crate_path(),
    };

    let syn::ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = item;
    if sig.asyncness.is_none() {
        return Err(syn::Error::new(
            sig.fn_token.span(),
            "`#[rok_db::test]` functions must be `async`",
        ));
    }
    if sig.inputs.len() != 1 {
        return Err(syn::Error::new(
            sig.inputs.span(),
            "expected exactly one parameter, e.g. `db: Db`",
        ));
    }
    let syn::FnArg::Typed(param) = &sig.inputs[0] else {
        return Err(syn::Error::new(
            sig.inputs.span(),
            "expected a parameter like `db: Db`",
        ));
    };
    let (pat, ty) = (&param.pat, &param.ty);
    let name = &sig.ident;
    let output = &sig.output;
    let skip = match output {
        syn::ReturnType::Default => quote!(return),
        syn::ReturnType::Type(..) => quote!(return ::core::result::Result::Ok(())),
    };
    let migrate = migrations.iter().map(|dir| {
        quote! {
            __rok_test_db
                .migrate(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/", #dir))
                .await
                .expect(::core::concat!("failed to run migrations in ", #dir));
        }
    });
    let sql = sql_files.iter().map(|file| {
        quote! {
            __rok_test_db
                .db()
                .execute(::core::include_str!(::core::concat!(::core::env!("CARGO_MANIFEST_DIR"), "/", #file)))
                .await
                .expect(::core::concat!("failed to execute ", #file));
        }
    });
    Ok(quote! {
        #(#attrs)*
        #[::tokio::test]
        #vis async fn #name() #output {
            let __rok_test_db = match #krate::testing::TestDb::create().await {
                ::core::result::Result::Ok(::core::option::Option::Some(db)) => db,
                ::core::result::Result::Ok(::core::option::Option::None) => {
                    ::std::eprintln!("DATABASE_URL not set; skipping `{}`", ::core::stringify!(#name));
                    #skip;
                }
                ::core::result::Result::Err(e) => ::core::panic!("failed to create a test database: {e}"),
            };
            #(#migrate)*
            #(#sql)*
            let #pat: #ty = ::core::clone::Clone::clone(__rok_test_db.db());
            let __rok_result = async move #block.await;
            ::core::mem::drop(__rok_test_db);
            __rok_result
        }
    })
}

/// Resolve the path to the `rok_db` crate from the caller's point of view.
fn crate_path() -> TokenStream2 {
    let found = crate_name("rok-db")
        .map(|c| (c, "rok_db"))
        .or_else(|_| crate_name("rok-db-core").map(|c| (c, "rok_db_core")));
    match found {
        // Inside rok-db's own tests/examples the crate is reachable by name.
        Ok((FoundCrate::Itself, default)) => {
            let ident = Ident::new(default, Span::call_site());
            quote!(::#ident)
        }
        Ok((FoundCrate::Name(name), _)) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!(::#ident)
        }
        Err(_) => quote!(::rok_db),
    }
}

fn unraw(ident: &Ident) -> String {
    let s = ident.to_string();
    s.strip_prefix("r#").map(str::to_owned).unwrap_or(s)
}

fn snake_case(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let prev_upper = i > 0 && chars[i - 1].is_uppercase();
            if prev_lower || (prev_upper && next_lower) {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn pluralize(s: &str) -> String {
    let ends_with_consonant_y =
        s.ends_with('y') && !s.chars().rev().nth(1).is_some_and(|c| "aeiou".contains(c));
    if ends_with_consonant_y {
        format!("{}ies", &s[..s.len() - 1])
    } else if ["s", "x", "z", "ch", "sh"].iter().any(|e| s.ends_with(e)) {
        format!("{s}es")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::{pluralize, snake_case};

    #[test]
    fn table_names() {
        let t = |s: &str| pluralize(&snake_case(s));
        assert_eq!(t("User"), "users");
        assert_eq!(t("BlogPost"), "blog_posts");
        assert_eq!(t("Category"), "categories");
        assert_eq!(t("Day"), "days");
        assert_eq!(t("Address"), "addresses");
        assert_eq!(t("HTTPRequest"), "http_requests");
        assert_eq!(t("Box"), "boxes");
    }
}
