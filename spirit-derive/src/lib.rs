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

use std::iter;

use either::Either;
use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{
    Attribute, Data, DataStruct, DeriveInput, Expr, Field, Fields, Ident, Lit, Meta, MetaList,
    MetaNameValue, NestedMeta, Type,
};

fn instruction(
    struct_name: &Ident,
    field_name: &Ident,
    field_type: &Type,
    extract_name: &Ident,
    instruction: &Meta,
) -> TokenStream {
    match instruction.name().to_string().as_ref() {
        "pipeline" => {
            // TODO: Allow overriding extract
            let pipeline = quote!({
                let pipeline: spirit::fragment::pipeline::Pipeline<_, _, _, _, (O, #struct_name)> =
                    spirit::fragment::pipeline::Pipeline::new(stringify!(#field_name))
                        .extract_cfg(#extract_name);
                pipeline
            });

            let inner = match instruction {
                Meta::Word(_) => Either::Left(iter::empty::<&NestedMeta>()),
                Meta::List(MetaList { ref nested, .. }) => Either::Right(nested.iter()),
                Meta::NameValue(_) => panic!("pipeline = '...' makes no sense"),
            };

            let modifiers = inner.map(|nested| match nested {
                NestedMeta::Meta(Meta::NameValue(MetaNameValue {
                    ident,
                    lit: Lit::Str(content),
                    ..
                })) => {
                    let params: Expr = content.parse().unwrap();
                    quote!(#ident(#params))
                }
                NestedMeta::Meta(Meta::Word(ident)) => quote!(#ident()),
                _ => panic!("Pipeline modifiers need to be method = 'content'"),
            });

            quote!(let builder = builder.with(#pipeline #( . #modifiers )*);)
        }
        "extension" => {
            // TODO: Optionally extract the name for the method
            quote!(let builder = builder.with(<#field_type>::extension(#extract_name));)
        }
        "immutable" => quote!(
            let builder = builder.with(spirit::extension::immutable_cfg(
                #extract_name,
                stringify!(#field_name),
            ));
        ),
        name => panic!("Unknown spirit instruction {}", name),
    }
}

fn gen_methods(
    struct_name: &Ident,
    _top_attributes: &[Attribute],
    fields: &Punctuated<Field, Comma>,
) -> TokenStream {
    let cmds = fields.iter().flat_map(|field| {
        let name = field.ident.as_ref().unwrap();
        let ty = &field.ty;
        let extract_name = Ident::new(&format!("_extract_{}", name), name.span());
        // TODO: Check for cloned attribute
        let extract = quote! {
            fn #extract_name(cfg: &#struct_name) -> &#ty {
                &cfg.#name
            }
        };

        let pipelines = field
            .attrs
            .iter()
            .filter(|attr| attr.path.is_ident("spirit"))
            .map(|attr| {
                attr.parse_meta()
                    .expect("Attributes need to be in form spirit(...)")
            })
            .flat_map(|meta| match meta {
                Meta::Word(_) => panic!("The spirit attribute needs parameters"),
                Meta::List(MetaList { nested, .. }) => nested.into_iter().map(|ins| match ins {
                    NestedMeta::Literal(_) => panic!("Unsupported literal inside spirit"),
                    NestedMeta::Meta(ins) => instruction(struct_name, name, ty, &extract_name, &ins),
                }),
                Meta::NameValue(_) => panic!("The spirit attribute can't be 'spirit = ...'"),
            })
            .collect::<Vec<_>>(); // Force evaluation for borrow checker.

        iter::once(extract).chain(pipelines)
    });
    quote! {
        fn extension<O>(mut builder: spirit::Builder<O, Self>)
            -> Result<spirit::Builder<O, Self>, spirit::macro_support::Error>
        {
            use spirit::extension::Extensible;
            #(#cmds)*
            // Trick to make it into -> Result<Builder, _> even if the list of .with above is
            // empty.
            builder.with(|builder: spirit::Builder<O, Self>| builder)
        }
    }
}

/// Derive of the `Spirit`.
///
/// Note that it is *not* a trait. It generates one or more methods that can be used, but they live
/// directly on the type.
#[proc_macro_derive(Spirit, attributes(spirit))]
pub fn spirit_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: DeriveInput = syn::parse(input).unwrap();
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let methods = match input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(fields),
            ..
        }) => gen_methods(name, &input.attrs, &fields.named),
        _ => unimplemented!("Only named structs are supported for now"),
    };

    //panic!("{}", (quote! {
    (quote! {
        impl #impl_generics #name #ty_generics
        #where_clause
        {
            #methods
        }
    })
    .into()
}
