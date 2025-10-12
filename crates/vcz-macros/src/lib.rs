mod utils;

use darling::FromDeriveInput;
use proc_macro::TokenStream;
use quote::quote;
use syn::{
    DeriveInput, Token, parse::Parse, parse_macro_input,
    punctuated::Punctuated, token::Comma,
};

#[derive(FromDeriveInput)]
#[darling(attributes(extension))]
struct ExtArgs {
    ident: syn::Ident,
    id: syn::LitInt,
    #[darling(default)]
    bencoded: bool,
}

/// Implement ExtensionTrait on the struct.
/// Usage:
/// ```
/// #[derive(Extension)]
/// #[extension(id = 3)]
/// pub struct MetadataExt;
/// ```
#[proc_macro_derive(Extension, attributes(extension))]
pub fn derive_extension(input: TokenStream) -> TokenStream {
    let input: DeriveInput = parse_macro_input!(input);
    let args = match ExtArgs::from_derive_input(&input) {
        Ok(args) => args,
        Err(e) => return e.write_errors().into(),
    };

    // name of the extension
    let name = args.ident;
    let id = args.id;

    let ser_type = if args.bencoded {
        quote! {
           impl core::convert::TryFrom<crate::extensions::ExtendedMessage> for #name {
               type Error = crate::error::Error;
               fn try_from(value: crate::extensions::ExtendedMessage) -> Result<Self, Self::Error> {
                   if value.0 != #id {
                        // todo: change error
                       return Err(Error::PeerIdInvalid);
                   }
                   #name::from_bencode(&value.1).map_err(|e| e.into())
               }
           }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl crate::extensions::ExtMsg for #name {
            const ID: u8 = #id;
        }
        #ser_type
    };

    TokenStream::from(expanded)
}

#[proc_macro_derive(Message)]
pub fn derive_msg(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    // name of the struct
    let _name = &input.ident;
    let expanded = quote! {};
    TokenStream::from(expanded)
}

/// List of Messages (enums) names separated with comma
struct Items(Punctuated<syn::Ident, Comma>);

impl Parse for Items {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let punctuated =
            input.parse_terminated(syn::Ident::parse, Token![,])?;

        Ok(Self(punctuated))
    }
}

/// From a list of types that implement
/// `ExtensionTrait`, generate a [`Message`]
/// enum with all of their messages. Aditionally, a struct `MessageCodec` that
/// implements Encoder and Decoder for it's branches.
///
/// example:
///
/// ```ignore
/// declare_message!(CoreCodec, ExtendedCodec, MetadataCodec);
///
/// pub enum Message {
///     CoreCodec(Core),
///     ExtendedCodec(Extended),
///     MetadataCodec(Metadata),
/// }
/// ```
#[proc_macro]
pub fn declare_message(input: TokenStream) -> TokenStream {
    let _a = parse_macro_input!(input as Items).0;

    // let mut ext = Vec::new();
    // ext.extend(a);

    // let error: syn::Path = syn::parse_str("crate::error::Error").unwrap();
    //
    // let encoder: syn::Path =
    //     syn::parse_str("tokio_util::codec::Encoder").unwrap();
    //
    // let decoder: syn::Path =
    //     syn::parse_str("tokio_util::codec::Decoder").unwrap();
    //
    // let core_codec: syn::Path =
    //     syn::parse_str("crate::extensions::CoreCodec").unwrap();
    //
    // let msg_trait: syn::Path =
    //     syn::parse_str("crate::extensions::extended::r#trait::MessageTrait")
    //         .unwrap();

    let expanded = quote! {
        // use crate::extensions::*;

        // #[derive(Debug)]
        // pub struct MessageCodec;

        // #[derive(Debug, Clone, PartialEq)]
        // pub enum Message {
        //     #(
        //         #ext(<#ext as ExtensionTrait>::Msg),
        //     )*
        // }

        // From<Metadata> for <ExtendedMessage>
        // #(
        //     impl From<<#ext as ExtensionTrait>::Msg> for Message {
        //         fn from(value: <#ext as ExtensionTrait>::Msg) -> Self {
        //             Message::#ext(value)
        //         }
        //     }
        //     impl TryFrom<Message> for <#ext as ExtensionTrait>::Msg {
        //         type Error = #error;
        //
        //         fn try_from(value: Message) -> Result<Self, #error> {
        //             if let Message::#ext(v) = value {
        //                 Ok(v)
        //             } else {
        //                 Err(#error::PeerIdInvalid)
        //             }
        //         }
        //     }
        // )*
        // #[derive(Debug, Clone)]
        // pub enum Extensions {
        //     #(
        //         #ext(#ext),
        //     )*
        // }
        // impl #encoder<Message> for MessageCodec {
        //     type Error = #error;
        //     fn encode(
        //         &mut self,
        //         item: Message,
        //         dst: &mut bytes::BytesMut,
        //     ) -> Result<(), Self::Error> {
        //         Ok(())
        //     }
        // }
        // impl #decoder for MessageCodec {
        //     type Error = #error;
        //     type Item = Message;
        //
        //     fn decode(
        //         &mut self,
        //         src: &mut bytes::BytesMut,
        //     ) -> Result<Option<Self::Item>, Self::Error> {
        //         todo!();
        //     }
        // }
    };

    TokenStream::from(expanded)
}
