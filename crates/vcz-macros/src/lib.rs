mod utils;

use darling::{FromDeriveInput, util::PathList};
use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

#[derive(FromDeriveInput)]
#[darling(attributes(extension))]
struct ExtArgs {
    ident: syn::Ident,
    id: syn::LitInt,
    #[darling(default)]
    bencoded: bool,
}

/// Implement ExtMsg on the type.
///
/// Usage:
///
/// ```ignore
/// #[derive(vcz_macros::Extension)]
/// #[extension(id = 3)]
/// pub struct MetadataMsg;
/// ```
///
/// can also implement `TryFrom<ExtendedMessage>` for this type
/// if it implements  `bendy::FromBencode`
///
/// ```ignore
/// #[derive(vcz_macros::Extension)]
/// #[extension(id = 2, bencoded)]
/// pub struct BencodedMsg;
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

               fn try_from(value: crate::extensions::ExtendedMessage)
                -> std::result::Result<Self, Self::Error>
               {
                   if value.0 != #id {
                        return std::result::Result::Err(
                            crate::error::Error::WrongExtensionId {
                                local: #id,
                                received: value.0,
                            },
                        );
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

#[derive(FromDeriveInput)]
#[darling(attributes(extensions))]
struct PeerExtArgs {
    // a list of types that implement `ExtMsg`.
    #[darling(flatten)]
    exts: PathList,
}

#[proc_macro_derive(Peer, attributes(extensions))]
pub fn derive_peer_extensions(input: TokenStream) -> TokenStream {
    let input: DeriveInput = parse_macro_input!(input);
    let args = match PeerExtArgs::from_derive_input(&input) {
        Ok(args) => args,
        Err(e) => return e.write_errors().into(),
    };
    let ident = input.ident;
    let exts = args.exts;

    let expanded = quote! {
        impl #ident<crate::peer::types::Connected> {
            pub async fn handle_message(
                &mut self,
                msg: crate::extensions::Core,
            )
            -> std::result::Result<(), crate::error::Error>
            {
                match msg {
                    crate::extensions::Core::Extended(
                        msg @ crate::extensions::ExtendedMessage(ext_id, _)
                    ) => {
                        match ext_id {
                            #(
                                <#exts as crate::extensions::ExtMsg>::ID => {
                                    let msg: #exts = msg.try_into()?;
                                    self.handle_msg(msg).await?;
                                }
                            )*
                            _ => {}
                        }
                    }
                    _ => self.handle_msg(msg).await?
                }
                std::result::Result::Ok(())
            }
        }
    };

    TokenStream::from(expanded)
}
