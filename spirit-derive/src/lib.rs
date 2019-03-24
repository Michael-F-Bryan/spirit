#![doc(
    html_root_url = "https://docs.rs/spirit-derive/0.1.0/spirit-derive/",
    test(attr(deny(warnings)))
)]
#![allow(clippy::type_complexity)]
#![forbid(unsafe_code)]

//! A procedural derive macros for the [`spirit`] crate. See the documentation there.
//!
//! [`spirit`]: https://docs.rs/spirit

// No way to use compiler's internal crates in the 2018 way yet :-(
extern crate proc_macro;

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{Attribute, Data, DataStruct, DeriveInput, Field, Fields};

fn gen_methods(top_attributes: &[Attribute], fields: &Punctuated<Field, Comma>) -> TokenStream {
    unimplemented!()
}

/// Derive of the `Spirit`.
///
/// Note that it is *not* a trait. It generates one or more methods that can be used, but they live
/// directly on the type.
#[proc_macro_derive(StructDoc, attributes(structdoc))]
pub fn structdoc_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: DeriveInput = syn::parse(input).unwrap();
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let methods = match input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(fields),
            ..
        }) => gen_methods(&input.attrs, &fields.named),
        _ => unimplemented!("Only named structs are supported for now"),
    };

    (quote! {
        impl #impl_generics #name #ty_generics
        #where_clause
        {
            #methods
        }
    })
    .into()
}
