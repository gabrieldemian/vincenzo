//! Handle magnet link
use std::ops::{Deref, DerefMut};

use hashbrown::HashMap;
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
        if let Some(dn) = self.0.display_name() {
            if let Ok(dn) = urlencoding::decode(dn) {
                return dn.to_string();
            }
        }
        "Unknown".to_owned()
    }

    /// Transform the "xt" field from hex, to a slice.
    pub fn parse_xt(&self) -> [u8; 20] {
        let info_hash = hex::decode(self.hash().unwrap()).unwrap();
        let mut x = [0u8; 20];

        x[..20].copy_from_slice(&info_hash[..20]);
        x
    }

    /// Transform the "xt" field from hex, to a slice.
    pub fn parse_xt_infohash(&self) -> InfoHash {
        let info_hash = hex::decode(self.hash().unwrap()).unwrap();
        let mut x = [0u8; 20];

        x[..20].copy_from_slice(&info_hash[..20]);
        x.into()
    }

    pub fn organize_trackers(&self) -> HashMap<&str, Vec<String>> {
        let mut hashmap = HashMap::from([
            ("udp", vec![]),
            ("http", vec![]),
            ("https", vec![]),
        ]);

        for x in self.trackers() {
            let x = x.clone();
            let mut uri = urlencoding::decode(&x).unwrap().to_string();
            let (protocol, _) = x.split_once("%3A").unwrap();
            // %3A%2F%2F -> ://

            if protocol == "udp" {
                uri = uri.replace("udp://", "");
                // remove any /announce
                if let Some(i) = uri.find("/announce") {
                    uri = uri[..i].to_string();
                };
            }

            let trackers = hashmap.get_mut(protocol).unwrap();

            trackers.push(uri.to_string());
        }

        hashmap
    }

    /// Parse trackers so they can be used as socket addresses.
    pub fn parse_trackers(&self) -> Vec<String> {
        let tr: Vec<String> = self
            .trackers()
            .to_owned()
            .iter_mut()
            .filter(|x| x.starts_with("udp") || x.starts_with("http"))
            .map(|x| x.to_owned())
            .collect();
        tr
    }
}

#[cfg(test)]
pub mod tests {
    use magnet_url::MagnetBuilder;

    use super::*;

    #[tokio::test]
    async fn parse_string_to_magnet() {
        let mstr = "magnet:?xt=urn:btih:1234567890abcdef1234567890abcdef12345678&dn=My%20Torrent&xl=12345&tr=udp://tracker.example.com:6969&tr=udp://tracker2.example.com:6969&tr=wss://tracker3.example.com&ws=https://example.com/see";

        let magnet = Magnet_::new(mstr).unwrap();

        println!("trackers {:?}", magnet.trackers());

        let magnet = MagnetBuilder::new()
            .display_name("My Torrent")
            .hash_type("btih")
            .hash("1234567890abcdef1234567890abcdef12345678")
            .length(12345)
            .add_tracker("udp://tracker.example.com:6969")
            .add_trackers(&[
                "udp://tracker2.example.com:6969",
                "wss://tracker3.example.com",
                "http://tracker4.example.com",
            ])
            .web_seed("https://example.com/seed")
            .build();

        println!("Generated magnet URL: {:?}", magnet.trackers());

        // assert!(false);
    }

    #[tokio::test]
    async fn magnet_from_string() {
        let s = "magnet:?xt=urn:btih:61084b062ec1a41002810f99e4cddc33057b3bd5&\
                 dn=%5BSubsPlease%5D%20Dandadan%20-%2013%20%281080p%29%20%\
                 5BA3CDCC67%5D.mkv&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%\
                 2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&\
                 tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&\
                 tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%\
                 2F%2Ftracker.torrent.eu.org%3A451%2Fannounce";

        let magnet = Magnet::new(s).unwrap();
        // println!("{magnet:#?}");
        // println!("{:#?}", magnet.organize_trackers());

        // assert!(false);
    }
}

// magnet:?xt=urn:btih:3d5e3dec8fedacf52a984a91089ea60ef0a93086&dn=%5BSubsPlease%5D%20Dandadan%20-%2016%20%281080p%29%20%5B5681367F%5D.mkv&tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce
