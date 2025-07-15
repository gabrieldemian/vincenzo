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
        let info_hash = hex::decode(self.hash().clone().unwrap()).unwrap();
        let mut x = [0u8; 20];

        x[..20].copy_from_slice(&info_hash[..20]);
        x
    }

    /// Transform the "xt" field from hex, to a slice.
    pub fn parse_xt_infohash(&self) -> InfoHash {
        let info_hash = hex::decode(self.hash().clone().unwrap()).unwrap();
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
            let (protocol, uri) = x.split_once("://").unwrap();

            let mut uri = uri.to_owned();

            if uri.starts_with("udp://") {
                uri = uri.replacen("udp://", "", 1);

                // remove any /announce
                if let Some(i) = uri.find('/') {
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
        // .filter(|x| x.starts_with("udp"))
        // .map(|x| {
        //     *x = urlencoding::decode(x).unwrap().to_string();
        //     *x = x.replace("udp://", "");
        //
        //     // remove any /announce
        //     if let Some(i) = x.find('/') {
        //         *x = x[..i].to_string();
        //     };
        //
        //     x.to_owned()
        // })
        // .collect();
        tr
    }
}

#[cfg(test)]
pub mod tests {
    use magnet_url::MagnetBuilder;
    use tokio::net::UdpSocket;

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

        assert!(false);
    }
}
