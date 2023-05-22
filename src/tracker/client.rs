use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    time::Duration,
};

use log::debug;

use crate::error::Error;

use super::connect::{self, Request, Response};

#[derive(Debug)]
pub struct Client {
    pub peer_id: [u8; 20],
    pub tracker_addr: SocketAddr,
    /// UDP Socket of the `tracker_addr`
    pub sock: UdpSocket,
    pub connection_id: u64,
    pub transaction_id: u32,
}

impl Client {
    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    pub fn connect<A: ToSocketAddrs>(trackers: Vec<A>) -> Result<Self, Error> {
        for tracker in trackers {
            let addrs = tracker
                .to_socket_addrs()
                .map_err(Error::TrackerSocketAddrs)?;

            for tracker_addr in addrs {
                println!("what? {:#?}", tracker_addr);
                let sock = match Self::new_udp_socket(tracker_addr) {
                    Ok(sock) => sock,
                    Err(_) => continue,
                };
                let mut client = Client {
                    peer_id: rand::random(),
                    tracker_addr,
                    sock,
                    transaction_id: 0,
                    connection_id: 0,
                };
                println!("do lado de fora");
                if let Ok(()) = client.connect_exchange() {
                    println!("calling return now");
                    return Ok(client);
                }
            }
        }
        Err(Error::TrackerNoHosts)
    }

    /// Create an UDP Socket for the given tracker address
    pub fn new_udp_socket(addr: SocketAddr) -> Result<UdpSocket, Error> {
        let sock = match addr {
            SocketAddr::V4(_) => UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)),
        }
        .expect("Failed to bind udp socket");
        sock.connect(addr).expect("Failed to connect to udp socket");
        sock.set_read_timeout(Some(Duration::new(3, 0)))
            .expect("Failed to set a read timeout to udp socket");

        Ok(sock)
    }

    fn connect_exchange(&mut self) -> Result<(), Error> {
        let req = connect::Request::new();
        let res = Response::new();

        let req = bincode::serialize(&req).unwrap();
        let mut res = bincode::serialize(&res).unwrap();

        // will try to connect up to 3 times
        // breaking if the first one happens
        for _ in 0..=2 {
            println!("sending...");

            self.sock.send(&req)?;

            if let Ok(len) = self.sock.recv(&mut res) {
                if len == 0 {
                    println!("--- len 0");
                    return Err(Error::TrackerResponse);
                }
                break;
            }
        }

        let req = Request::deserialize(&req.as_slice()).unwrap();
        let res = Response::deserialize(&res).unwrap();

        println!("req {:#?}", req);
        println!("received res {:#?}", res);

        if res.transaction_id != req.transaction_id || res.action != req.action {
            return Err(Error::TrackerResponse);
        }

        self.connection_id = res.connection_id;
        self.transaction_id = res.transaction_id;

        println!("new conn {:?}", self.connection_id);

        Ok(())
    }
}
