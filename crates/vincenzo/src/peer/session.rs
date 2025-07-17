use std::time::Duration;

use tokio::time::Instant;

use crate::{avg::SlidingDurationAvg, counter::ThruputCounters};

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
    /// Measures various transfer statistics.
    pub counters: ThruputCounters,
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
    /// This is the average network round-trip-time between the last issued
    /// a request and receiving the next block.
    ///
    /// Note that it doesn't have to be the same block since peers are not
    /// required to serve our requests in order, so this is more of a general
    /// approximation.
    pub avg_request_rtt: SlidingDurationAvg,
    pub request_timed_out: bool,
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
            counters: ThruputCounters::default(),
            in_endgame: false,
            target_request_queue_len: Session::DEFAULT_REQUEST_QUEUE_LEN,
            connected_time: None,
            avg_request_rtt: SlidingDurationAvg::default(),
            request_timed_out: false,
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
    pub const DEFAULT_REQUEST_QUEUE_LEN: u16 = 150;

    /// The smallest timeout value we can give a peer. Very fast peers will have
    /// an average round-trip-times, so a slight deviation would punish them
    /// unnecessarily. Therefore we use a somewhat larger minimum threshold for
    /// timeouts.
    const MIN_TIMEOUT: Duration = Duration::from_secs(2);

    /// Returns the current request timeout value, based on the running average
    /// of past request round trip times.
    pub fn request_timeout(&self) -> Duration {
        // we allow up to four times the average deviation from the mean
        // let t = self.avg_request_rtt.mean() + 4 *
        // self.avg_request_rtt.deviation(); t.max(Self::MIN_TIMEOUT)
        Self::MIN_TIMEOUT
    }

    /// Updates state to reflect that peer was timed out.
    pub fn register_request_timeout(&mut self) {
        // peer has timed out, only allow a single outstanding request
        // from now until peer hasn't timed out
        // self.target_request_queue_len -= 1;
        self.timed_out_request_count += 1;
        self.request_timed_out = true;
    }

    /// Updates various statistics around a block download.
    /// This should be called every time a block is received.
    pub fn update_download_stats(&mut self, block_len: u32) {
        let now = Instant::now();

        // update request time
        if let Some(last_outgoing_request_time) =
            &mut self.last_outgoing_request_time
        {
            // Due to what is presumed to be inconsistencies with the
            // `Instant::now()` API, it happens in rare circumstances that using
            // the regular `duration_since` here panics (#48). I suspect this
            // happens when requests are made a very short interval before this
            // function is called, which is likely in very fast downloads.
            // Either way, we guard against this by defaulting to 0.
            let elapsed_since_last_request =
                now.saturating_duration_since(*last_outgoing_request_time);

            // If we timed out before, check if this request arrived within the
            // timeout window, or outside of it. If it arrived within the
            // window, we can mark peer as having recovered from the timeout.
            if self.request_timed_out
                && elapsed_since_last_request <= self.request_timeout()
            {
                self.request_timed_out = false;
            }

            let request_rtt = elapsed_since_last_request;
            self.avg_request_rtt.update(request_rtt);
        }

        self.counters.payload.down += block_len as u64;
        self.last_incoming_block_time = Some(now);
    }

    pub fn record_waste(&mut self, block_len: u32) {
        self.counters.waste += block_len as u64;
    }

    pub fn update_upload_stats(&mut self, block_len: u32) {
        self.last_outgoing_block_time = Some(Instant::now());
        self.counters.payload.up += block_len as u64;
    }
}
