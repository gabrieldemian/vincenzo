use tokio::time::Instant;

/// At any given time, a connection with a handshaked(connected) peer has 3
/// possible states. ConnectionState means TCP connection, Even if the peer is
/// choked they are still marked here as connected.
#[derive(Clone, Default, Copy, Debug, PartialEq)]
pub enum ConnectionState {
    /// The handshake just happened, probably computing choke algorithm and
    /// sending bitfield messages.
    #[default]
    Connecting,

    // Connected and downloading and uploading.
    Connected,

    /// This state is set when the program is gracefully shutting down,
    /// In this state, we don't send the outgoing blocks to the tracker on
    /// shutdown.
    Quitting,
}

/// Contains the state of both sides of the connection.
#[derive(Clone, Copy, Debug)]
pub struct CoreState {
    /// If we're choked, peer doesn't allow us to download pieces from them.
    pub am_choking: bool,

    /// If we're interested, peer has pieces that we don't have.
    pub am_interested: bool,

    /// If peer is choked, we don't allow them to download pieces from us.
    pub peer_choking: bool,

    /// If peer is interested in us, they mean to download pieces that we have.
    pub peer_interested: bool,
}

impl Default for CoreState {
    /// By default, both sides of the connection start off as choked and not
    /// interested in the other.
    fn default() -> Self {
        Self {
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
        }
    }
}

/// Holds and provides facilities to modify the state of a peer session.
#[derive(Debug)]
pub struct Session {
    /// The current state of the connection.
    pub connection: ConnectionState,

    /// The session state.
    // pub state: CoreState,
    // when the torrent is paused, those values will be set, so we can
    // assign them again when the torrent is resumed.
    // peer interested will be calculated by parsing the peers pieces
    pub prev_peer_choking: bool,

    /// Whether we're in endgame mode.
    pub in_endgame: bool,

    /// The target request queue size is the number of block requests we keep
    /// outstanding
    pub target_request_queue_len: u16,

    /// The last time some requests were sent to the peer.
    pub last_outgoing_request_time: Option<Instant>,

    /// Updated with the time of receipt of the most recently received
    /// requested block.
    pub last_incoming_block_time: Option<Instant>,

    /// Updated with the time of receipt of the most recently uploaded block.
    pub last_outgoing_block_time: Option<Instant>,

    pub timed_out_request_count: usize,

    /// The time the BitTorrent connection was established (i.e. after
    /// handshaking)
    pub connected_time: Option<Instant>,

    /// If the torrent was fully downloaded, all peers will become seed only.
    /// They will only seed but not download anything anymore.
    pub seed_only: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            connection: Default::default(),
            prev_peer_choking: true,
            in_endgame: false,
            target_request_queue_len: Session::DEFAULT_REQUEST_QUEUE_LEN,
            connected_time: None,
            timed_out_request_count: 0,
            last_incoming_block_time: None,
            last_outgoing_block_time: None,
            last_outgoing_request_time: None,
            seed_only: false,
        }
    }
}

impl Session {
    /// The value of outstanding blocks for a peer.
    ///
    /// Before we do an extended handshake,
    /// we do not have access to `reqq`.
    /// And so this value is initialized with a sane default,
    /// most clients support 250+ inflight requests.
    ///
    /// After the extended handshake, this value is not used
    /// in favour of the `reqq`, if the peer has it.
    pub const DEFAULT_REQUEST_QUEUE_LEN: u16 = 200;
}
