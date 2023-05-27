use magnet_url::{Magnet, MagnetError};
use sha1::Digest;
use sha1::Sha1;

pub fn get_magnet(str: &str) -> Result<Magnet, MagnetError> {
    let mut m = Magnet::new(str)?;

    let tr: Vec<String> =
        m.tr.iter_mut()
            .map(|x| {
                *x = urlencoding::decode(&x).unwrap().to_string();
                *x = x.replace("http://", "");
                *x = x.replace("udp://", "");
                // remove any /announce
                if let Some(i) = x.find('/') {
                    *x = x[..i].to_string();
                };
                x.to_owned()
            })
            .collect();
    m.tr = tr;

    Ok(m)
}

/// The infohash from the magnet link needs to be
/// feeded into a SHA1 function, before converting
/// the hex string to a byte vec
pub fn get_info_hash(info: &str) -> [u8; 20] {
    let infohash = hex::decode(info).unwrap();
    let mut hasher = Sha1::new();
    hasher.update(infohash);

    let infohash: [u8; 20] = hasher.finalize().into();
    infohash
}
