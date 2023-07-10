#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    None,
    Completed,
    Started,
    Stopped,
}

impl Default for Event {
    fn default() -> Event {
        Event::None
    }
}

impl From<Event> for u64 {
    fn from(a: Event) -> Self {
        match a {
            Event::None => 0,
            Event::Completed => 1,
            Event::Started => 2,
            Event::Stopped => 3,
        }
    }
}

impl From<u64> for Event {
    fn from(x: u64) -> Self {
        match x {
            0 => Event::None,
            1 => Event::Completed,
            2 => Event::Started,
            3 => Event::Stopped,
            _ => Event::None,
        }
    }
}
