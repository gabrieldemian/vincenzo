//! Handle magnet link
use std::ops::{Deref, DerefMut};

use magnet_url::Magnet as Magnet_;

use crate::{error::Error, torrent::InfoHash};

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
            Magnet_::new(magnet_url).map_err(|_| Error::MagnetLinkInvalid)?,
        ))
    }

    /// The name will come URL encoded, and it is also optional.
    pub fn parse_dn(&self) -> String {
        if let Some(dn) = self.dn.clone() {
            if let Ok(dn) = urlencoding::decode(&dn) {
                return dn.to_string();
            }
        }
        "Unknown".to_string()
    }

    /// Transform the "xt" field from hex, to a slice.
    pub fn parse_xt(&self) -> [u8; 20] {
        let info_hash = hex::decode(self.xt.clone().unwrap()).unwrap();
        let mut x = [0u8; 20];

        x[..20].copy_from_slice(&info_hash[..20]);
        x
    }

    /// Transform the "xt" field from hex, to a slice.
    pub fn parse_xt_infohash(&self) -> InfoHash {
        let info_hash = hex::decode(self.xt.clone().unwrap()).unwrap();
        let mut x = [0u8; 20];

        x[..20].copy_from_slice(&info_hash[..20]);
        x.into()
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

#[cfg(test)]
pub mod tests {
    #[test]
    fn parse_string_to_magnet() {
        let mstr = "magnet:?xt=urn:btih:56BC861F42972DEA863AE853362A20E15C7BA07E&amp;dn=Rust%20for%20Rustaceans%3A%20Idiomatic%20Programming&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.dler.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fexodus.desync.com%3A6969&amp;tr=udp%3A%2F%2Fopen.demonii.com%3A1337%2Fannounce";
    }
}
