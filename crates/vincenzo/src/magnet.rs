//! Handle magnet link
use std::ops::{Deref, DerefMut};

use magnet_url::Magnet as Magnet_;

use crate::error::Error;

#[derive(Debug, Clone, Hash)]
pub struct Magnet(Magnet_);

impl Deref for Magnet {
    type Target = Magnet_;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Magnet {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Magnet {
    pub fn new(magnet_url: &str) -> Result<Self, Error> {
        Ok(Self(
            Magnet_::new(&magnet_url).map_err(|_| Error::MagnetLinkInvalid)?,
        ))
    }

    /// The name will come URL encoded, and it is also optional.
    pub fn parse_dn(&self) -> String {
        if let Some(dn) = self.dn.clone() {
            if let Ok(dn) = urlencoding::decode(&dn) {
                return dn.to_string();
            }
        }
        return "Unknown".to_string();
    }

    /// Transform the "xt" field from hex, to a slice.
    pub fn parse_xt(&self) -> [u8; 20] {
        let info_hash = hex::decode(self.xt.clone().unwrap()).unwrap();
        let mut x = [0u8; 20];

        x[..20].copy_from_slice(&info_hash[..20]);
        x
    }

    /// Parse trackers so they can be used as socket addresses.
    pub fn parse_trackers(&self) -> Vec<String> {
        let tr: Vec<String> = self
            .tr
            .clone()
            .iter_mut()
            .filter(|x| x.starts_with("udp"))
            .map(|x| {
                *x = urlencoding::decode(x).unwrap().to_string();
                *x = x.replace("udp://", "");

                // remove any /announce
                if let Some(i) = x.find('/') {
                    *x = x[..i].to_string();
                };

                x.to_owned()
            })
            .collect();
        tr
    }
}
