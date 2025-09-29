use rkyv::{
    Archive, Deserialize, Place, Portable, Serialize,
    bytecheck::{CheckBytes, InvalidEnumDiscriminantError, Verify},
    primitive::ArchivedU32,
    rancor::{Fallible, Source, fail},
    traits::NoUndef,
};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Action {
    Connect = 0,
    #[default]
    Announce = 1,
    Scrape = 2,
}

#[derive(CheckBytes, Portable, Debug, PartialEq)]
#[bytecheck(crate = rkyv::bytecheck, verify)]
#[repr(C)]
pub struct ArchivedAction(ArchivedU32);

unsafe impl NoUndef for ArchivedAction {}

impl PartialEq<Action> for ArchivedAction {
    fn eq(&self, other: &Action) -> bool {
        self.0 == (*other) as u32
    }
}

impl ArchivedAction {
    // Internal fallible conversion back to the original enum
    fn try_to_native(&self) -> Option<Action> {
        Some(match self.0.to_native() {
            0 => Action::Connect,
            1 => Action::Announce,
            2 => Action::Scrape,
            _ => return None,
        })
    }

    // Public infallible conversion back to the original enum
    pub fn to_native(&self) -> Action {
        unsafe { self.try_to_native().unwrap_unchecked() }
    }
}

unsafe impl<C: Fallible + ?Sized> Verify<C> for ArchivedAction
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
                enum_name: "ArchivedAction",
                invalid_discriminant: self.0.to_native(),
            })
        }
        Ok(())
    }
}

impl Archive for Action {
    type Archived = ArchivedAction;
    type Resolver = ();

    fn resolve(&self, _: Self::Resolver, out: Place<Self::Archived>) {
        // Convert Action -> u32 -> ArchivedU32 and write to `out`
        out.write(ArchivedAction((*self as u32).into()));
    }
}

// Serialization is a no-op because there's no out-of-line data
impl<S: Fallible + ?Sized> Serialize<S> for Action {
    fn serialize(
        &self,
        _: &mut S,
    ) -> Result<Self::Resolver, <S as Fallible>::Error> {
        Ok(())
    }
}

// Deserialization just calls the public conversion and returns the result
impl<D: Fallible + ?Sized> Deserialize<Action, D> for ArchivedAction {
    fn deserialize(&self, _: &mut D) -> Result<Action, <D as Fallible>::Error> {
        Ok(self.to_native())
    }
}
