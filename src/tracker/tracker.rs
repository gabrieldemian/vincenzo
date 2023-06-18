use std::{
    fmt::Debug,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use log::{debug, info, warn};
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    select, spawn,
    sync::mpsc::{self, Sender},
    time::{interval, timeout},
};

use crate::{error::Error, peer::Peer, torrent::TorrentMsg};

use super::{announce, connect};

#[derive(Debug)]
pub struct Tracker {
    /// UDP Socket of the `tracker_addr`
    /// Peers announcing will send handshakes
    /// to this addr
    pub socket: UdpSocket,
    pub ctx: TrackerCtx,
}

#[derive(Debug, Clone)]
pub struct TrackerCtx {
    /// Our ID for this connected Tracker
    pub peer_id: [u8; 20],
    /// UDP Socket of the `socket` in Tracker
    /// Peers announcing will send handshakes
    /// to this addr
    pub tracker_addr: String,
    pub connection_id: Option<u64>,
}

impl Default for TrackerCtx {
    fn default() -> Self {
        Self {
            peer_id: [0u8; 20],
            tracker_addr: "0.0.0.0:0".parse().unwrap(),
            connection_id: None,
        }
    }
}

impl Tracker {
    const ANNOUNCE_RES_BUF_LEN: usize = 8192;

    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    // todo: return only tracker woth the most seeders?
    pub async fn connect<A>(trackers: Vec<A>) -> Result<Self, Error>
    where
        A: ToSocketAddrs + Debug + Send + Sync + 'static + std::fmt::Display + Clone,
        A::Iter: Send,
    {
        info!("...trying to connect to {:?} trackers", trackers.len());

        let (tx, mut rx) = mpsc::channel::<Tracker>(30);

        // Connect to all trackers, return on the first
        // succesful handshake
        for tracker_addr in trackers {
            debug!("trying to connect {tracker_addr:?}");
            let tx = tx.clone();

            spawn(async move {
                let socket = match Self::new_udp_socket(tracker_addr.clone()).await {
                    Ok(socket) => socket,
                    Err(_) => {
                        warn!("could not connect to tracker");
                        return Ok::<(), Error>(());
                    }
                };
                let mut tracker = Tracker {
                    ctx: TrackerCtx {
                        peer_id: rand::random(),
                        tracker_addr: tracker_addr.to_string(),
                        connection_id: None,
                    },
                    socket,
                };
                if tracker.connect_exchange().await.is_ok() {
                    info!("announced to tracker {tracker_addr}");
                    debug!("DNS of the tracker {tracker:#?}");
                    if tx.send(tracker).await.is_err() {
                        return Ok(());
                    };
                }
                Ok(())
            });
        }

        while let Some(tracker) = rx.recv().await {
            debug!("Connected and announced to tracker {tracker:#?}");
            return Ok(tracker);
        }

        Err(Error::TrackerNoHosts)
    }
    async fn connect_exchange(&mut self) -> Result<(), Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LENGTH];
        let mut len: usize = 0;

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            debug!("sending connect number {i}...");
            self.socket.send(&req.serialize()).await?;

            match timeout(Duration::new(5, 0), self.socket.recv(&mut buf)).await {
                Ok(Ok(lenn)) => {
                    len = lenn;
                    break;
                }
                Err(e) => {
                    debug!("error receiving connect response, {e}");
                }
                _ => {}
            }
        }

        if len == 0 {
            return Err(Error::TrackerResponse);
        }

        let (res, _) = connect::Response::deserialize(&buf)?;

        info!("received res from tracker {res:#?}");

        if res.transaction_id != req.transaction_id || res.action != req.action {
            warn!("response not valid!");
            return Err(Error::TrackerResponse);
        }

        self.ctx.connection_id.replace(res.connection_id);
        Ok(())
    }

    pub async fn announce_exchange(&self, infohash: [u8; 20]) -> Result<Vec<Peer>, Error> {
        let connection_id = match self.ctx.connection_id {
            Some(x) => x,
            None => return Err(Error::TrackerNoConnectionId),
        };

        let req = announce::Request::new(
            connection_id,
            infohash,
            self.ctx.peer_id,
            self.socket.local_addr()?.port(),
        );

        debug!("local ip is {}", self.socket.local_addr()?);

        let mut len = 0_usize;
        let mut res = [0u8; Self::ANNOUNCE_RES_BUF_LEN];

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            info!("trying to send announce number {i}...");
            self.socket.send(&req.serialize()).await?;
            match timeout(Duration::new(3, 0), self.socket.recv(&mut res)).await {
                Ok(Ok(lenn)) => {
                    len = lenn;
                    break;
                }
                Err(e) => {
                    warn!("failed to announce {e:#?}");
                }
                _ => {}
            }
        }

        if len == 0 {
            return Err(Error::TrackerResponse);
        }

        let res = &res[..len];

        // res is the deserialized struct,
        // payload is a byte array of peers,
        // which are in the form of ips and ports
        let (res, payload) = announce::Response::deserialize(res)?;

        if res.transaction_id != req.transaction_id || res.action != req.action {
            return Err(Error::TrackerResponse);
        }

        info!("* announce successful with {self:?}");
        info!("res from announce {:?}", res);

        let peers = Self::parse_compact_peer_list(payload, self.socket.peer_addr()?.is_ipv6())?;
        debug!("got peers: {:#?}", peers);

        Ok(peers)
    }

    /// Connect is the first step in getting the file
    /// Create an UDP Socket for the given tracker address
    pub async fn new_udp_socket<A: ToSocketAddrs>(addr: A) -> Result<UdpSocket, Error> {
        // let socket = match addr {
        //     SocketAddr::V4(_) => UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await,
        //     SocketAddr::V6(_) => UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await,
        // };
        let socket = UdpSocket::bind("0.0.0.0:0").await;
        if let Ok(socket) = socket {
            if let Ok(_) = socket.connect(addr).await {
                return Ok(socket);
            }
            return Err(Error::TrackerSocketConnect);
        }
        Err(Error::TrackerSocketAddr)
    }

    fn parse_compact_peer_list(buf: &[u8], is_ipv6: bool) -> Result<Vec<Peer>, Error> {
        let mut peer_list = Vec::<SocketAddr>::new();

        // in ipv4 the addresses come in packets of 6 bytes,
        // first 4 for ip and 2 for port
        // in ipv6 its 16 bytes for port and 2 for port
        let stride = if is_ipv6 { 18 } else { 6 };

        let chunks = buf.chunks_exact(stride);
        if !chunks.remainder().is_empty() {
            return Err(Error::TrackerCompactPeerList);
        }

        for hostpost in chunks {
            let (ip, port) = hostpost.split_at(stride - 2);
            let ip = if is_ipv6 {
                let octets: [u8; 16] = ip[0..16]
                    .try_into()
                    .expect("iterator guarantees bounds are OK");
                IpAddr::from(std::net::Ipv6Addr::from(octets))
            } else {
                IpAddr::from(std::net::Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]))
            };

            let port =
                u16::from_be_bytes(port.try_into().expect("iterator guarantees bounds are OK"));

            peer_list.push((ip, port).into());
        }

        let peers: Vec<Peer> = peer_list.into_iter().map(|p| p.into()).collect();

        Ok(peers)
    }

    // the addr used to announce will be added, by the tracker,
    // as a peer to the list of peers. This means I need to
    // listen to handshake events with this addr here.
    // and this function needs a Sender to the `Torrent`
    #[tracing::instrument]
    pub async fn run(&self, _tx: Sender<TorrentMsg>) {
        info!("# listening to tracker events...");
        let mut tick_timer = interval(Duration::from_secs(1));

        let mut buf = [0; 1024];
        loop {
            select! {
                _ = tick_timer.tick() => {
                    debug!("tick tracker");
                }
                Ok(n) = self.socket.recv(&mut buf) => {
                    match n {
                        0 => {
                            warn!("peer closed");
                        }
                        n => {
                            info!("datagram {:?}", &buf[..n]);
                        }
                    }
                }
            }
        }
    }
}

// pub async fn connect_test() -> Result<(), Error> {
//     // Create a UDP socket bound to a specific local address and port
//     let socket = UdpSocket::bind("0.0.0.0:0").await?;
//
//     // Destination address and port for the tracker
//     socket.connect("tracker1.bt.moack.co.kr:80").await?;
//
//     // Build the connect request packet
//     let mut buf = [0; 16];
//     buf[..8].copy_from_slice(&0x41727101980u64.to_be_bytes()); // Connection ID: 0x41727101980
//     buf[8..12].copy_from_slice(&0x00u32.to_be_bytes()); // Action: 0 (connect)
//     buf[12..16].copy_from_slice(&(0x12345678u32).to_be_bytes()); // Transaction ID: 0x12345678
//
//     // Send the connect request packet
//     socket.send(&buf).await?;
//
//     // Receive the connect response
//     let mut connect_response = [0; 16];
//     socket.recv(&mut connect_response).await?;
//     let action = u32::from_be_bytes([
//         connect_response[0],
//         connect_response[1],
//         connect_response[2],
//         connect_response[3],
//     ]);
//     let transaction_id = u32::from_be_bytes([
//         connect_response[4],
//         connect_response[5],
//         connect_response[6],
//         connect_response[7],
//     ]);
//     let connection_id = u64::from_be_bytes([
//         connect_response[8],
//         connect_response[9],
//         connect_response[10],
//         connect_response[11],
//         connect_response[12],
//         connect_response[13],
//         connect_response[14],
//         connect_response[15],
//     ]);
//
//     // Check if the response action and transaction ID match the request
//     if action == 0 && transaction_id == 0x12345678 {
//         println!("Connection ID: {}", connection_id);
//
//         // Prepare the announce request data
//         // let info_hash = "0123456789abcdef0123456789abcdef01234567".to_owned();
//         // let peer_id = "ABCDEFGHIJKLMNOPQRST".to_owned();
//         // let port: u16 = 6881;
//         // let event = "started";
//         //
//         // // Build the announce request packet
//         // let mut announce_request = Vec::new();
//         // announce_request.extend_from_slice(&connection_id.to_be_bytes()); // Connection ID
//         // announce_request.extend_from_slice(&0x00u32.to_be_bytes()); // Action: 1 (announce)
//         // announce_request.extend_from_slice(&(0x87654321u32).to_be_bytes()); // Transaction ID: 0x87654321
//         // announce_request.extend_from_slice(info_hash.as_bytes()); // Info Hash
//         // announce_request.extend_from_slice(peer_id.as_bytes()); // Peer ID
//         // announce_request.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Downloaded: 0
//         // announce_request.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Left: 0
//         // announce_request.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Uploaded: 0
//         // announce_request.extend_from_slice(&[0x00, 0x00, 0x00, 0x02]); // Event: 2 (started)
//         // announce_request.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // IP Address: 0
//         // announce_request.extend_from_slice(&[0x00, 0x00, 0x1A, 0xE1]); // Key: 0x1AE1
//         // announce_request.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // Num Want: -1 (default)
//         // announce_request.extend_from_slice(&port.to_be_bytes()); // Port
//         //
//         // // Send the announce request packet
//         // socket.send_to(&announce_request, tracker_addr).await?;
//         //
//         // // Receive the announce response
//         // let mut announce_response = [0; 4096];
//         // let (bytes_read, _src_addr) = socket.recv_from(&mut announce_response).await?;
//         // let response = &announce_response[..bytes_read];
//         //
//         // // Process the response as needed
//         // println!("Received announce response: {:?}", response);
//     }
//
//     Ok(())
// }
