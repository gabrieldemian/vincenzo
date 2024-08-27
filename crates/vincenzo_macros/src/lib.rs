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
    let name = &input.ident;

    let expanded = quote! {
        impl crate::extensions::extended::MessageTrait2 for #name {}
    };

    // list of field names of the struct
    // [ { ident: a, type: u32 }, { ident: b, type: u32 } ]
    // let struct_field = if let Data::Struct(DataStruct {
    //     fields: Fields::Named(FieldsNamed { ref named, .. }),
    //     ..
    // }) = input.data
    // {
    //     named
    // } else {
    //     // user tried to use the macro on non-enum type
    //     return syn::Error::new(name.span(), "expected `\"enum\"`")
    //         .to_compile_error()
    //         .into();
    // };

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
    // _comma2: Token![,],
    // _msg_name: syn::Ident,
    // _eq3: Token![=],
    // msg_value: syn::Type,
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
            // _comma2: input.parse()?,
            // _msg_name: input.parse().and_then(|v: Ident| {
            //     if v != *"msg" {
            //         return Err(syn::Error::new(v.span(), "Expected `msg`"));
            //     }
            //     Ok(v)
            // })?,
            // _eq3: input.parse()?,
            // msg_value: input.parse()?,
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
    // let msg = parsed.msg_value;

    let expanded = quote! {
        use crate::extensions::*;

        impl ExtensionTrait2 for #name {
            // type Msg = #msg;
            type Msg = <#codec as Decoder>::Item;

            fn codec(&self) -> Box<dyn CodecTrait<Self::Msg>> {
                Box::new(#codec)
            }
            fn id(&self) -> u8 {
                #id
            }
            // fn get_msg(&self) -> Self::Msg {
            //     #msg
            // }
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
    let a = parse_macro_input!(input as Items).0;

    let mut ext = Vec::new();
    ext.extend(a);

    let error: syn::Path = syn::parse_str("crate::error::Error").unwrap();

    let encoder: syn::Path =
        syn::parse_str("tokio_util::codec::Encoder").unwrap();

    let decoder: syn::Path =
        syn::parse_str("tokio_util::codec::Decoder").unwrap();

    let expanded = quote! {
        use crate::extensions::*;

        #[derive(Debug)]
        pub struct MessageCodec;

        #[derive(Debug, Clone, PartialEq)]
        pub enum Message {
            #(
                #ext(<#ext as ExtensionTrait2>::Msg),
            )*
        }

        #[derive(Debug, Clone)]
        pub enum Extensions {
            #(
                #ext(#ext),
            )*
        }

        #(
            impl From<<#ext as ExtensionTrait2>::Msg> for Message {
                fn from(value: <#ext as ExtensionTrait2>::Msg) -> Self {
                    Message::#ext(value)
                }
            }
        )*

        impl #encoder<Message> for MessageCodec {
            type Error = #error;

            fn encode(
                &mut self,
                item: Message,
                dst: &mut bytes::BytesMut,
            ) -> Result<(), Self::Error> {
                match item {
                    #(
                        Message::#ext(v) => {
                            #ext.codec().encode(v, dst)?;
                        },
                    )*
                };
                Ok(())
            }
        }

        impl #decoder for MessageCodec {
            type Error = #error;
            type Item = Message;

            fn decode(
                &mut self,
                src: &mut bytes::BytesMut,
            ) -> Result<Option<Self::Item>, Self::Error> {
                let core = CoreExt.codec().decode(src)?;

                // todo: change this error
                let core = core.ok_or(crate::error::Error::PeerIdInvalid)?;

                match core {
                    #(
                        // find if there is an extension that supports the given message extension
                        // ID (src) by comparing their ids.
                        Core::Extended(id, _payload) if id == #ext.id() => {
                            let v = #ext.codec().decode(src)?
                                .ok_or(#error::PeerIdInvalid)?;
                            return Ok(Some(Message::#ext(v)));
                        },
                    )*
                    // if not, its a Core message
                    _ => Ok(Some(Message::CoreExt(core)))
                }
            }
        }

        // impl Extensions {
        //     pub fn get_codec<M>(&self, msg: &M) -> Option<Box<dyn CodecTrait<M>>>
        //         where M: MessageTrait2 
        //     {
        //         match self {
        //             #(
        //                 Extensions::#ext(ext) => {
        //                     let msg_ty = ext.get_msg();
        //                     let does_match = msg_ty == msg;
        //                     if does_match {
        //                         let codec = ext.codec();
        //                         return Some(codec as Box<dyn CodecTrait<M>>);
        //                         // return Some(ext.codec() as CodecTrait<M>);
        //                     }
        //                 }
        //             )*
        //         }
        //         None
        //     }
        // }

        impl Message {
            // pub async fn handle_msg(
            //     &self,
            //     peer: &mut Peer,
            // ) -> Result<(), #error>
            //     {
            //     match self {
            //         #(
            //             Message::#ext(msg) => {
            //                 let codec = msg.codec();
            //                 if codec.is_supported(&peer.extension) {
            //                     codec.handle_msg(&msg, peer).await?;
            //                 }
            //             }
            //         )*
            //     }
            //     Ok(())
            // }
        }
    };

    TokenStream::from(expanded)
}
