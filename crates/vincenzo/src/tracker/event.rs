use rkyv::{
    Archive, Deserialize, Place, Portable, Serialize,
    bytecheck::{CheckBytes, InvalidEnumDiscriminantError, Verify},
    primitive::ArchivedU32,
    rancor::{Fallible, Source, fail},
    traits::NoUndef,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Event {
    None = 0,
    Completed = 1,
    #[default]
    Started = 2,
    Stopped = 3,
}

#[derive(CheckBytes, Portable, Debug, PartialEq)]
#[bytecheck(crate = rkyv::bytecheck, verify)]
#[repr(C)]
pub struct ArchivedEvent(ArchivedU32);

unsafe impl NoUndef for ArchivedEvent {}

impl PartialEq<Event> for ArchivedEvent {
    fn eq(&self, other: &Event) -> bool {
        self.0 == (*other) as u32
    }
}

impl ArchivedEvent {
    // Internal fallible conversion back to the original enum
    fn try_to_native(&self) -> Option<Event> {
        Some(match self.0.to_native() {
            0 => Event::None,
            1 => Event::Completed,
            2 => Event::Started,
            3 => Event::Stopped,
            _ => return None,
        })
    }

    // Public infallible conversion back to the original enum
    pub fn to_native(&self) -> Event {
        unsafe { self.try_to_native().unwrap_unchecked() }
    }
}

unsafe impl<C: Fallible + ?Sized> Verify<C> for ArchivedEvent
where
    C::Error: Source,
{
    // verify runs after all of the fields have been checked
    fn verify(&self, _: &mut C) -> Result<(), C::Error> {
        // Use the internal conversion to try to convert back
        if self.try_to_native().is_none() {
            // Return an error if it fails (i.e. the discriminant did not match
            // any valid discriminants)
            fail!(InvalidEnumDiscriminantError {
                enum_name: "ArchivedEvent",
                invalid_discriminant: self.0.to_native(),
            })
        }
        Ok(())
    }
}

impl Archive for Event {
    type Archived = ArchivedEvent;
    type Resolver = ();

    fn resolve(&self, _: Self::Resolver, out: Place<Self::Archived>) {
        // Convert MyEnum -> u32 -> ArchivedU32 and write to `out`
        out.write(ArchivedEvent((*self as u32).into()));
    }
}

// Serialization is a no-op because there's no out-of-line data
impl<S: Fallible + ?Sized> Serialize<S> for Event {
    fn serialize(
        &self,
        _: &mut S,
    ) -> Result<Self::Resolver, <S as Fallible>::Error> {
        Ok(())
    }
}

// Deserialization just calls the public conversion and returns the result
impl<D: Fallible + ?Sized> Deserialize<Event, D> for ArchivedEvent {
    fn deserialize(&self, _: &mut D) -> Result<Event, <D as Fallible>::Error> {
        Ok(self.to_native())
    }
}
