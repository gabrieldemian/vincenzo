use speedy::{Readable, Writable};

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Readable, Writable, Default)]
pub enum Event {
    None = 0,
    Completed = 1,
    #[default]
    Started = 2,
    Stopped = 3,
}
