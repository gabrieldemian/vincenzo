mod utils;

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
    token::Comma,
    DeriveInput, Ident, Token,
};

#[proc_macro_derive(Message)]
pub fn derive_msg(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    // name of the struct
    let _name = &input.ident;

    let expanded = quote! {};

    TokenStream::from(expanded)
}

#[derive(Debug)]
struct ExtArgs {
    _id_name: syn::Ident,
    _eq1: Token![=],
    id_value: syn::LitInt,
    _comma1: Token![,],
    _codec_name: syn::Ident,
    _eq2: Token![=],
    codec_value: syn::Type,
    _comma2: Token![,],
    _msg_name: syn::Ident,
    _eq3: Token![=],
    msg_value: syn::Type,
}

impl Parse for ExtArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        Ok(ExtArgs {
            _id_name: input.parse().and_then(|v: Ident| {
                if v != *"id" {
                    return Err(syn::Error::new(v.span(), "Expected `id`"));
                }
                Ok(v)
            })?,
            _eq1: input.parse()?,
            id_value: input.parse()?,
            _comma1: input.parse()?,
            _codec_name: input.parse().and_then(|v: Ident| {
                if v != *"codec" {
                    return Err(syn::Error::new(v.span(), "Expected `codec`"));
                }
                Ok(v)
            })?,
            _eq2: input.parse()?,
            codec_value: input.parse()?,
            _comma2: input.parse()?,
            _msg_name: input.parse().and_then(|v: Ident| {
                if v != *"msg" {
                    return Err(syn::Error::new(v.span(), "Expected `msg`"));
                }
                Ok(v)
            })?,
            _eq3: input.parse()?,
            msg_value: input.parse()?,
        })
    }
}

/// Implement ExtensionTrait on the struct.
/// Usage:
/// ```
/// #[derive(Extension)]
/// #[extension(id = 3, codec = MetadataCodec)]
/// pub struct MetadataExt;
/// ```
#[proc_macro_derive(Extension, attributes(extension))]
pub fn derive_extension(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    // name of the extension
    let name = &input.ident;

    let Some(attrs) = input.attrs.get(0..1) else {
        return syn::Error::new(
            name.span(),
            "Expected attribute `extension(id = `...`, codec = `...`, msg = `...`)`",
        )
        .to_compile_error()
        .into();
    };

    let Ok(parsed) = attrs[0].parse_args::<ExtArgs>() else {
        return syn::Error::new(
            name.span(),
            "Available values: `id`, `codec` and `msg`",
        )
        .to_compile_error()
        .into();
    };
    let id = parsed.id_value;
    let codec = parsed.codec_value;
    let msg = parsed.msg_value;

    let expanded = quote! {
        use crate::extensions::*;

        impl extended::MessageTrait for #msg {
            fn extension(&self) -> impl extended::ExtensionTrait {
                #name
            }
        }

        impl From<#msg> for core::ExtendedMessage {
            fn from(val: #msg) -> Self {
                let payload: bytes::BytesMut = val.into();
                ExtendedMessage(MetadataExt.id(), payload.to_vec())
            }
        }

        impl extended::ExtensionTrait for #name {
            type Msg = #msg;

            fn id(&self) -> u8 {
                #id
            }

            fn codec(
                &self
            ) ->
                Box<dyn CodecTrait<Self::Msg>>
            // impl
            //     tokio_util::codec::Encoder<Self::Msg, Error = crate::error::Error> +
            //     tokio_util::codec::Decoder<Item = Self::Msg, Error = crate::error::Error>
            {
                Box::new(#codec)
            }
        }
    };

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
