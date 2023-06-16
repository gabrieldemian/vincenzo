use magnet_url::{Magnet, MagnetError};

// const MORE_TRACKERS: [&str; 19] = [
//     "udp://tracker.opentrackr.org:1337/announce",
//     "udp://opentracker.i2p.rocks:6969/announce",
//     "udp://tracker.openbittorrent.com:6969/announce",
//     "udp://open.demonii.com:1337/announce",
//     "udp://open.stealth.si:80/announce",
//     "udp://exodus.desync.com:6969/announce",
//     "udp://tracker.torrent.eu.org:451/announce",
//     "udp://tracker.moeking.me:6969/announce",
//     "udp://tracker.bitsearch.to:1337/announce",
//     "udp://explodie.org:6969/announce",
//     "udp://uploads.gamecoast.net:6969/announce",
//     "udp://tracker1.bt.moack.co.kr:80/announce",
//     "udp://tracker.tiny-vps.com:6969/announce",
//     "udp://tracker.theoks.net:6969/announce",
//     "udp://tracker.auctor.tv:6969/announce",
//     "udp://tracker.4.babico.name.tr:3131/announce",
//     "udp://sanincode.com:6969/announce",
//     "udp://private.anonseed.com:6969/announce",
//     "udp://movies.zsw.ca:6969/announce",
// ];

pub fn get_magnet(str: &str) -> Result<Magnet, MagnetError> {
    let mut m = Magnet::new(str)?;

    // m.tr.extend_from_slice(&MORE_TRACKERS.map(|x| x.to_owned()));

    // Remove URL encoding of Display Name of torrent
    if let Some(dn) = m.dn.clone() {
        if let Ok(dn) = urlencoding::decode(&dn) {
            m.dn = Some(dn.to_string());
        }
    }

    // Remove URL encoding of trackers URLs
    let tr: Vec<String> =
        m.tr.iter_mut()
            .map(|x| {
                *x = urlencoding::decode(x).unwrap().to_string();
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
    // I dont need to parse to SHA1 because the magnet
    // info_hash is already parsed to SHA1.
    // I would need to do that if I were getting the hash
    // on a .torrent file
    let infohash = hex::decode(info).unwrap();
    let mut x = [0u8; 20];

    x[..20].copy_from_slice(&infohash[..20]);

    x
}
