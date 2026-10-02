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
/// - `#[rok(no_from_row)]` — don't generate `FromRow` (bring your own).
/// - `#[rok(crate = "path::to::rok_db")]` — path of the `rok_db` crate when
///   it is re-exported under another name.
///
/// # Field attributes
///
/// - `#[rok(primary_key)]` — the primary key. Defaults to the field named `id`.
/// - `#[rok(generated)]` — filled in by the database (serial ids, defaults,
///   triggers): read, but never written by `insert`/`save`.
/// - `#[rok(column = "name")]` — column name, if it differs from the field.
/// - `#[rok(skip)]` — not a column; initialised with `Default::default()`.
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
    let mut from_row = true;
    let mut krate: Option<Path> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("rok")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse::<LitStr>()?.value());
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

    let named = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return Err(syn::Error::new(
                    name.span(),
                    "`#[derive(Model)]` requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                name.span(),
                "`#[derive(Model)]` can only be used on structs",
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
        };
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("rok")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("primary_key") {
                    f.primary_key = true;
                } else if meta.path.is_ident("generated") || meta.path.is_ident("auto") {
                    f.generated = true;
                } else if meta.path.is_ident("skip") {
                    f.skip = true;
                } else if meta.path.is_ident("column") || meta.path.is_ident("rename") {
                    f.column = meta.value()?.parse::<LitStr>()?.value();
                } else {
                    return Err(meta.error(
                        "unknown `rok` attribute; expected `primary_key`, `generated`, `column` or `skip`",
                    ));
                }
                Ok(())
            })?;
        }
        if f.skip && (f.primary_key || f.generated) {
            return Err(syn::Error::new(
                f.ident.span(),
                "`skip` cannot be combined with `primary_key` or `generated`",
            ));
        }
        fields.push(f);
    }

    let columns: Vec<&Field> = fields.iter().filter(|f| !f.skip).collect();
    let mut pks = columns.iter().filter(|f| f.primary_key);
    let pk = match (pks.next(), pks.next()) {
        (Some(pk), None) => *pk,
        (Some(_), Some(second)) => {
            return Err(syn::Error::new(
                second.ident.span(),
                "only one field can be `#[rok(primary_key)]` (composite keys are not supported)",
            ));
        }
        (None, _) => *columns.iter().find(|f| f.ident == "id").ok_or_else(|| {
            syn::Error::new(
                name.span(),
                "no primary key: add a field named `id` or mark one with `#[rok(primary_key)]`",
            )
        })?,
    };

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

    let from_row_impl = from_row.then(|| {
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
    });

    Ok(quote! {
        #from_row_impl

        impl #name {
            #(#consts)*
        }

        impl #krate::Model for #name {
            const TABLE: &'static str = #table;
            const PRIMARY_KEY: &'static str = #pk_column;
            const COLUMNS: &'static [&'static str] = &[#(#column_names),*];
            const GENERATED: &'static [&'static str] = &[#(#generated),*];

            fn primary_key(&self) -> #krate::Value {
                #krate::Value::from(&self.#pk_ident)
            }

            fn values(&self) -> ::std::vec::Vec<(&'static str, #krate::Value)> {
                ::std::vec![#(#values),*]
            }
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
    use super::*;

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
