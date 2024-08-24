mod utils;

use proc_macro::TokenStream;
use syn::{
    parse_macro_input, Data, DataStruct, DeriveInput, Fields, FieldsNamed,
};

#[proc_macro_derive(Message)]
pub fn derive_msg(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    // name of the struct
    let name = &input.ident;

    // list of field names of the struct
    // [ { ident: a, type: u32 }, { ident: b, type: u32 } ]
    let struct_field = if let Data::Struct(DataStruct {
        fields: Fields::Named(FieldsNamed { ref named, .. }),
        ..
    }) = input.data
    {
        named
    } else {
        // user tried to use the macro on non-enum type
        return syn::Error::new(name.span(), "expected `\"enum\"`")
            .to_compile_error()
            .into();
    };

    todo!()
}
