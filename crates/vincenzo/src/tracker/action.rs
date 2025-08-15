use speedy::{Readable, Writable};

#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Writable, Readable)]
pub enum Action {
    Connect = 0,
    #[default]
    Announce = 1,
    Scrape = 2,
    Unsupported = 0xffff,
}
