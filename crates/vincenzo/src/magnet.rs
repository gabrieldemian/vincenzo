//! Handle magnet link
use std::ops::{Deref, DerefMut};

use hashbrown::HashMap;
use magnet_url::Magnet as Magnet_;

use crate::{error::Error, torrent::InfoHash};

#[derive(Debug, Clone, Hash)]
pub struct Magnet(pub Magnet_);

impl From<Magnet_> for Magnet {
    fn from(value: Magnet_) -> Self {
        Self(value)
    }
}

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
        if let Some(dn) = self.0.display_name()
            && let Ok(dn) = urlencoding::decode(dn)
        {
            return dn.to_string();
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
    // todo: return a Result
    pub fn parse_xt_infohash(&self) -> InfoHash {
        let info_hash = hex::decode(
            self.hash().unwrap_or("0000000000000000000000000000000000000000"),
        )
        .unwrap();
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
            let Some(b) = x.find("%3A%2F%2F") else { continue };
            let Some(e) = x[b + 8..].find("%2F") else { continue };
            let prot = &x[..b];

            // skip the utp:// at the beggining and the /announce at the end.
            let url = &x[(b + 9)..e + b + 8];
            // decode url_encoded `%3A` to `:`
            let url = url.to_owned().replace("%3A", ":");

            let Some(trackers) = hashmap.get_mut(prot) else { continue };
            trackers.push(url.to_owned());
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
    use super::*;

    #[test]
    fn organize_trackers() {
        let mstr =
            "magnet:?xt=urn:btih:0000000000000000000000000000000000000000&\
             dn=Name&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce&\
             tr=&tr=udp%3A%2F%2Fopen.tracker.cl%3A1337%2Fannounce&tr=&tr=udp%\
             3A%2F%2F9.rarbg.com%3A2810%2Fannounce&tr=&tr=udp%3A%2F%2Ftracker.\
             openbittorrent.com%3A6969%2Fannounce&tr=&tr=udp%3A%2F%2Fwww.\
             torrent.eu.org%3A451%2Fannounce&tr=&tr=udp%3A%2F%2Ftracker.\
             torrent.eu.org%3A451%2Fannounce&tr=&tr=udp%3A%2F%2Fexodus.desync.\
             com%3A6969%2Fannounce&tr=&tr=udp%3A%2F%2Ftracker.opentrackr.org%\
             3A1337%2Fannounce&tr=http%3A%2F%2Ftracker.openbittorrent.com%\
             3A80%2Fannounce&tr=udp%3A%2F%2Fopentracker.i2p.rocks%3A6969%\
             2Fannounce&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%\
             2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%\
             2Fannounce&tr=udp%3A%2F%2Fcoppersurfer.tk%3A6969%2Fannounce&\
             tr=udp%3A%2F%2Ftracker.zer0day.to%3A1337%2Fannounce";
        let magnet = Magnet::new(mstr).unwrap();
        let org = magnet.organize_trackers();

        assert_eq!(org["http"], ["tracker.openbittorrent.com:80"]);
        assert_eq!(
            org["udp"],
            [
                "tracker.opentrackr.org:1337",
                "open.tracker.cl:1337",
                "9.rarbg.com:2810",
                "tracker.openbittorrent.com:6969",
                "www.torrent.eu.org:451",
                "tracker.torrent.eu.org:451",
                "exodus.desync.com:6969",
                "tracker.opentrackr.org:1337",
                "opentracker.i2p.rocks:6969",
                "tracker.internetwarriors.net:1337",
                "tracker.leechers-paradise.org:6969",
                "coppersurfer.tk:6969",
                "tracker.zer0day.to:1337",
            ]
        );

        let mstr = "magnet:?xt=urn:btih:\
                    0000000000000000000000000000000000000000&dn=%5BSubsPlease%\
                    5D%20Dandadan%20-%2024%20%281080p%29%20%5BAD3DEA4E%5D.mkv&\
                    tr=http%3A%2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%\
                    3A%2F%2Fopen.stealth.si%3A80%2Fannounce&tr=udp%3A%2F%\
                    2Ftracker.opentrackr.org%3A1337%2Fannounce&tr=udp%3A%2F%\
                    2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%\
                    2Ftracker.torrent.eu.org%3A451%2Fannounce";
        let magnet = Magnet::new(mstr).unwrap();
        let org = magnet.organize_trackers();

        assert_eq!(org["http"], ["nyaa.tracker.wf:7777"]);
        assert_eq!(
            org["udp"],
            [
                "open.stealth.si:80",
                "tracker.opentrackr.org:1337",
                "exodus.desync.com:6969",
                "tracker.torrent.eu.org:451",
            ]
        );
    }

    #[test]
    fn parse_string_to_magnet() {
        let mstr = "magnet:?xt=urn:btih:1234567890abcdef1234567890abcdef12345678&dn=My%20Torrent&xl=12345&tr=udp://tracker.example.com:6969&tr=udp://tracker2.example.com:6969&tr=wss://tracker3.example.com&ws=https://example.com/see";

        assert!(Magnet_::new(mstr).is_ok());
    }
}
