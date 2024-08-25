use vincenzo_macros::*;

#[test]
fn derive_ext() {
    struct Codec;

    #[derive(Extension)]
    #[extension(id = 2, codec = 2)]
    struct MyExt;
}
