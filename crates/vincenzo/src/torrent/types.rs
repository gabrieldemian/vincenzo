use std::ops::Deref;

use speedy::{Readable, Writable};

#[derive(Clone, PartialEq, Eq, Hash, Default, Readable, Writable)]
pub struct InfoHash([u8; 20]);

impl ToString for InfoHash {
    fn to_string(&self) -> String {
        hex::encode(self.0)
    }
}

impl Deref for InfoHash {
    type Target = [u8; 20];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::fmt::Debug for InfoHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.to_string();
        f.write_str(&s)
    }
}

impl From<InfoHash> for [u8; 20] {
    fn from(value: InfoHash) -> Self {
        value.0
    }
}

impl From<InfoHash> for String {
    fn from(value: InfoHash) -> Self {
        value.to_string()
    }
}

impl TryInto<InfoHash> for String {
    type Error = String;
    fn try_into(self) -> Result<InfoHash, Self::Error> {
        let buff = hex::decode(self).map_err(|e| e.to_string())?;
        let hash = InfoHash::try_from(buff)?;
        Ok(hash)
    }
}

impl From<[u8; 20]> for InfoHash {
    fn from(value: [u8; 20]) -> Self {
        Self(value)
    }
}

impl TryFrom<Vec<u8>> for InfoHash {
    type Error = &'static str;
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        if value.len() != 20 {
            return Err("The infohash must have exactly 20 bytes");
        }
        let mut buff = [0u8; 20];
        buff[..20].copy_from_slice(&value[..20]);
        Ok(InfoHash(buff))
    }
}

#[derive(Debug, Default, Clone, PartialEq, Readable, Writable)]
pub enum TorrentStatus {
    #[default]
    ConnectingTrackers,
    DownloadingMetainfo,
    Downloading,
    Seeding,
    Paused,
    Error,
}

impl<'a> From<TorrentStatus> for &'a str {
    fn from(val: TorrentStatus) -> Self {
        use TorrentStatus::*;
        match val {
            ConnectingTrackers => "Connecting to trackers",
            DownloadingMetainfo => "Downloading metainfo",
            Downloading => "Downloading",
            Seeding => "Seeding",
            Paused => "Paused",
            Error => "Error",
        }
    }
}

impl From<TorrentStatus> for String {
    fn from(val: TorrentStatus) -> Self {
        use TorrentStatus::*;
        match val {
            ConnectingTrackers => "Connecting to trackers".to_owned(),
            DownloadingMetainfo => "Downloading metainfo".to_owned(),
            Downloading => "Downloading".to_owned(),
            Seeding => "Seeding".to_owned(),
            Paused => "Paused".to_owned(),
            Error => "Error".to_owned(),
        }
    }
}

impl From<&str> for TorrentStatus {
    fn from(value: &str) -> Self {
        use TorrentStatus::*;
        match value {
            "Connecting to trackers" => ConnectingTrackers,
            "Downloading metainfo" => DownloadingMetainfo,
            "Downloading" => Downloading,
            "Seeding" => Seeding,
            "Paused" => Paused,
            _ => Error,
        }
    }
}
