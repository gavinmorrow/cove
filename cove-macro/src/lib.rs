#![forbid(unsafe_code)]
// Rustc lint groups
#![warn(future_incompatible)]
#![warn(rust_2018_idioms)]
#![warn(unused)]
// Rustc lints
#![warn(noop_method_call)]
#![warn(single_use_lifetimes)]
// Clippy lints
#![warn(clippy::use_self)]

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, Data, DeriveInput, Expr, ExprLit, Field, Lit, LitStr, MetaNameValue, Token,
};

fn strlit(expr: &Expr) -> Option<&LitStr> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(lit), ..
        }) => Some(lit),
        _ => None,
    }
}

/// Given a struct field, this finds all attributes like `#[doc = "bla"]`,
/// unindents, concatenates and returns them.
fn docstring(field: &Field) -> syn::Result<String> {
    let mut lines = vec![];

    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
    {
        if let Some(lit) = strlit(&attr.meta.require_name_value()?.value) {
            let value = lit.value();
            let value = value
                .strip_prefix(' ')
                .map(|value| value.to_string())
                .unwrap_or(value);
            lines.push(value);
        }
    }

    Ok(lines.join("\n"))
}

/// Given a struct field, this finds all key-value pairs of the form
/// `#[document(key = value, ...)]`.
fn document_attributes(field: &Field) -> syn::Result<Vec<MetaNameValue>> {
    let mut attrs = vec![];

    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("document"))
    {
        let args =
            attr.parse_args_with(Punctuated::<MetaNameValue, Token![,]>::parse_terminated)?;
        attrs.extend(args);
    }

    Ok(attrs)
}

fn field_doc(field: &Field) -> syn::Result<Option<TokenStream>> {
    let Some(ident) = field.ident.as_ref() else { return Ok(None); };
    let ident = ident.to_string();
    let ty = &field.ty;

    let mut setters = vec![];

    let docstring = docstring(field)?;
    if !docstring.is_empty() {
        setters.push(quote! {
            doc.description = Some(#docstring.to_string());
        });
    }

    for attr in document_attributes(field)? {
        let value = attr.value;
        if attr.path.is_ident("default") {
            setters.push(quote! { doc.value_info.default = Some(#value.to_string()); });
        } else if attr.path.is_ident("metavar") {
            setters.push(quote! { doc.wrap_info.metavar = Some(#value.to_string()); });
        } else {
            return Err(syn::Error::new(attr.path.span(), "unknown argument name"));
        }
    }

    Ok(Some(quote! {
        fields.insert(
            #ident.to_string(),
            {
                let mut doc = <#ty as Document>::doc();
                #( #setters )*
                Box::new(doc)
            }
        );
    }))
}

fn derive_document_impl(input: DeriveInput) -> syn::Result<TokenStream> {
    let Data::Struct(data) = input.data else {
        return Err(syn::Error::new(input.span(), "Must be a struct"));
    };

    let mut fields = Vec::new();
    for field in data.fields.iter() {
        if let Some(field) = field_doc(field)? {
            fields.push(field);
        }
    }

    let ident = input.ident;
    let tokens = quote!(
        impl crate::doc::Document for #ident {
            fn doc() -> crate::doc::Doc {
                use ::std::{boxed::Box, collections::HashMap};
                use crate::doc::{Doc, Document};

                let mut fields = HashMap::new();
                #( #fields )*

                let mut doc = Doc::default();
                doc.struct_info.fields = fields;
                doc
            }
        }
    );

    Ok(tokens)
}

#[proc_macro_derive(Document, attributes(document))]
pub fn derive_document(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_document_impl(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}