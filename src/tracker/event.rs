use crate::error::Error;

#[derive(Debug)]
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

impl TryFrom<u64> for Event {
    type Error = Error;
    fn try_from(x: u64) -> Result<Self, Self::Error> {
        match x {
            0 => Ok(Event::None),
            1 => Ok(Event::Completed),
            2 => Ok(Event::Started),
            3 => Ok(Event::Stopped),
            _ => Err(Error::TrackerEvent),
        }
    }
}
