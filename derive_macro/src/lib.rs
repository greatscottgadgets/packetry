extern crate proc_macro;

use proc_macro2::TokenStream;
use quote::quote;
use syn::{self, parse_macro_input, spanned::Spanned, MetaNameValue};

#[proc_macro_derive(ItemFields, attributes(name, expander))]
pub fn item_fields_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let ast = parse_macro_input!(input);
    let output = impl_item_fields(&ast);
    proc_macro::TokenStream::from(output)
}

fn impl_item_fields(ast: &syn::DeriveInput) -> TokenStream {
    let mut field_idents = vec![];
    let mut field_names = vec![];
    let mut expanders = vec![];
    match &ast.data {
        syn::Data::Struct(s) => {
            for field in &s.fields {
                let field_name = field.attrs.iter().find_map(attr_name);
                if field_name.is_none() {
                    return syn::Error::new(field.span(), "field must have a 'name' attribute, e.g '#[name = \"Field Name\"]'").to_compile_error();
                }

                let expander = field.attrs.iter().find_map(attr_expander).is_some();

                if field.ident.is_none() {
                    return syn::Error::new(field.span(), "struct field must have an identifier").to_compile_error();
                };
                let ident = field.ident.as_ref().unwrap();

                field_idents.push(ident);
                field_names.push(field_name.unwrap());
                expanders.push(expander);
            }
        },
        _ => return syn::Error::new(ast.span(), "ItemFields can only be derived on a struct").to_compile_error(),
    };

    let name = &ast.ident;

    quote! {
        impl #name {
            pub fn field_names() -> ::std::vec::Vec<&'static ::std::primitive::str> {
                ::std::vec![#(#field_names),*]
            }

            pub fn fields(&self) -> ::std::vec::Vec<&::std::primitive::str> {
                ::std::vec![#(&self.#field_idents),*]
            }

            pub fn expanders() -> ::std::vec::Vec<::std::primitive::bool> {
                ::std::vec![#(#expanders),*]
            }
        }
    }
}

fn attr_name(attr: &syn::Attribute) -> Option<String> {
    match &attr.parse_meta().unwrap() {
        syn::Meta::NameValue(MetaNameValue { path, eq_token: _, lit: syn::Lit::Str(lit_str) } ) => {
            path.is_ident("name").then(|| lit_str.value())
        },
        _ => None,
    }
}

fn attr_expander(attr: &syn::Attribute) -> Option<()> {
    match &attr.parse_meta().unwrap() {
        syn::Meta::Path(path) => {
            path.is_ident("expander").then(|| ())
        },
        _ => None,
    }
}