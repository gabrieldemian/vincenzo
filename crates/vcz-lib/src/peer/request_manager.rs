use std::{
    collections::{BTreeMap, BinaryHeap, VecDeque},
    hash::Hash,
    time::Duration,
};

use crate::{extensions::BlockInfo, peer::DEFAULT_REQUEST_QUEUE_LEN};
use bitvec::{bitvec, order::Msb0};
use hashbrown::HashMap;
use std::cmp::Reverse;
use tokio::time::Instant;

/// A type that can be requested.
/// The `Into<usize>` bound refers to the ability of a requestable type to have
/// a key to be grouped into.
pub trait Requestable =
    Eq + Default + Clone + Ord + Hash where for<'a> &'a Self: Into<usize>;

/// Struct to centralize the logic of requesting something,
/// used by BlockInfo and MetadataPiece (usize).
///
/// It also handles the calculation of timeouts with EMA (exponential moving
/// average) and a dynamic multiplier.
pub struct RequestManager<T: Requestable> {
    timeouts: BinaryHeap<(Reverse<Instant>, T)>,

    requests: BTreeMap<usize, Vec<T>>,

    /// Inverse index for requests
    index: HashMap<T, usize>,

    /// How many requests wered made in total.
    pub req_count: u64,

    /// How many requests were responded.
    pub downloaded_count: u64,

    /// Max amount of inflight pending requests
    limit: usize,

    /// Last 10 response times
    response_times: VecDeque<Duration>,

    /// Smoothed average
    avg_response_time: Duration,

    /// Multiplier of the average (to calculate timeout), low on low variance
    /// requests and high otherwise.
    timeout_multiplier: f32,

    /// Track when each request was made
    request_start_times: HashMap<T, Instant>,
}

impl<T: Requestable> Default for RequestManager<T> {
    fn default() -> Self {
        Self {
            req_count: 0,
            downloaded_count: 0,
            timeouts: BinaryHeap::with_capacity(
                DEFAULT_REQUEST_QUEUE_LEN as usize,
            ),
            requests: BTreeMap::default(),
            index: HashMap::with_capacity(DEFAULT_REQUEST_QUEUE_LEN as usize),
            response_times: VecDeque::with_capacity(10),
            avg_response_time: Duration::ZERO,
            timeout_multiplier: 3.0,
            request_start_times: HashMap::with_capacity(
                DEFAULT_REQUEST_QUEUE_LEN as usize,
            ),
            limit: DEFAULT_REQUEST_QUEUE_LEN as usize,
        }
    }
}

impl RequestManager<BlockInfo> {
    pub fn get_requests(&self) -> BTreeMap<usize, Vec<BlockInfo>> {
        self.requests.clone()
    }
}

impl<T: Requestable> RequestManager<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    /// How many requests the client can make right now.
    pub fn get_available_request_len(&self) -> usize {
        self.limit.saturating_sub(self.index.len())
    }

    pub fn get_avg(&self) -> Duration {
        self.avg_response_time
    }

    pub fn last_response(&self) -> Option<&Duration> {
        self.response_times.iter().last()
    }

    pub fn get_timeout(&self) -> Duration {
        if self.avg_response_time == Duration::ZERO {
            return Duration::from_secs(3);
        }

        let timeout = self.avg_response_time.mul_f32(self.timeout_multiplier);

        // todo: if the timeout is higher than the maximum (10 seconds)
        // what should the client do? choke the peer?

        timeout.clamp(Duration::from_millis(100), Duration::from_secs(10))
    }

    fn update_response_time(&mut self, response_time: Duration) {
        // keep only the last 10 response times
        if self.response_times.len() >= 10 {
            self.response_times.pop_front();
        }
        self.response_times.push_back(response_time);

        // calculate exponential moving average
        let alpha = 0.2; // smoothing factor
        if self.avg_response_time == Duration::ZERO {
            self.avg_response_time = response_time;
        } else {
            let current_avg = self.avg_response_time.as_micros() as f64;
            let new_time = response_time.as_micros() as f64;
            let new_avg = alpha * new_time + (1.0 - alpha) * current_avg;
            self.avg_response_time = Duration::from_micros(new_avg as u64);
        }

        self.adjust_timeout_multiplier();
    }

    /// Adjust timeout multiplier based on response time variance
    fn adjust_timeout_multiplier(&mut self) {
        if self.response_times.len() < 5 {
            return;
        }

        // calculate variance of response times
        let avg = self.avg_response_time.as_millis() as f64;
        let variance = self
            .response_times
            .iter()
            .map(|d| {
                let time_in_millis = d.as_millis() as f64;
                (time_in_millis - avg).powi(2)
            })
            .sum::<f64>()
            / self.response_times.len() as f64;

        // adjust multiplier based on variance
        if variance > 100.0 {
            // high variance
            self.timeout_multiplier = (self.timeout_multiplier * 1.1).min(4.0);
        } else {
            // low variance
            self.timeout_multiplier = (self.timeout_multiplier * 0.9).max(2.0);
        }
    }

    pub fn drain(&mut self) -> BTreeMap<usize, Vec<T>> {
        let r = std::mem::take(&mut self.requests);
        self.clear();
        r
    }

    pub fn clear(&mut self) {
        *self = Self {
            limit: self.limit,
            downloaded_count: self.downloaded_count,
            req_count: self.req_count,
            ..Default::default()
        };
    }

    pub fn extend(&mut self, blocks: BTreeMap<usize, Vec<T>>) {
        for block in blocks.into_values().flatten().collect::<Vec<_>>() {
            self.add_request(block);
        }
    }

    /// Return true if the item was inserted, false if duplicate.
    pub fn add_request(&mut self, block: T) -> bool {
        let i: usize = (&block).into();

        // avoid duplicates
        if self.index.contains_key(&block) {
            return false;
        }

        let timeout = Instant::now() + self.get_timeout();
        self.request_start_times.insert(block.clone(), Instant::now());

        let req_entry = self.requests.entry(i).or_default();

        let pos = req_entry.len();
        req_entry.push(block.clone());
        self.index.insert(block.clone(), pos);
        self.timeouts.push((Reverse(timeout), block));
        self.req_count += 1;
        true
    }

    /// Clone requests by `qnt`.
    pub fn clone_requests(&mut self, qnt: usize) -> Vec<T> {
        self.requests.values().flatten().take(qnt).cloned().collect::<Vec<_>>()
    }

    /// Return true if the request exists, and false otherwise.
    pub fn remove_request(&mut self, block: &T) -> bool {
        let Some(pos) = self.index.remove(block) else { return false };
        let i: usize = block.into();

        if let Some(start_time) = self.request_start_times.remove(block) {
            let response_time = Instant::now().duration_since(start_time);
            self.update_response_time(response_time);
        }

        let Some(requests) = self.requests.get_mut(&i) else { return false };
        requests.remove(pos);

        self.timeouts.retain(|v| v.1 != *block);

        // update indices for remaining blocks in the same piece
        for (new_pos, remaining_block) in requests.iter().enumerate().skip(pos)
        {
            self.index.insert(remaining_block.clone(), new_pos);
        }

        if requests.is_empty() {
            self.requests.remove(&i);
        }

        self.downloaded_count += 1;

        true
    }

    pub fn delete_timeout_blocks(&mut self, now: Instant) {
        let mut to_retain = bitvec![u8, Msb0; 1; self.timeouts.len()];

        for (i, (timeout, _block)) in
            self.timeouts.iter().peekable().enumerate()
        {
            if timeout.0 <= now {
                {
                    unsafe {
                        to_retain.set_unchecked(i, false);
                    }
                }
            } else {
                break;
            }
        }

        let mut retain_iter = to_retain.into_iter();
        self.timeouts.retain(|_| retain_iter.next().unwrap());
    }

    /// Get timed out blocks without mutating the timeout.
    pub fn get_timeout_blocks(&self) -> Vec<&T> {
        // assume around 25% of blocks are timed out
        let mut timed_out_blocks: Vec<&T> =
            Vec::with_capacity(self.timeouts.len() / 4);

        let now = Instant::now();

        for (timeout, block) in self.timeouts.iter().peekable() {
            if timeout.0 <= now {
                {
                    timed_out_blocks.push(block);
                }
            } else {
                break;
            }
        }

        timed_out_blocks
    }

    /// Get timed out blocks and calculate a new timeout.
    pub fn get_timeout_blocks_and_update(&mut self) -> Vec<T> {
        // assume around 25% of blocks are timed out
        let mut timed_out_blocks: Vec<T> =
            Vec::with_capacity(self.timeouts.len() / 4);

        let now = Instant::now();
        let new_timeout = now + self.get_timeout();

        for &(mut timeout, ref block) in self
            .timeouts
            .iter()
            .peekable()
            .take(self.get_available_request_len())
        {
            if timeout.0 <= now {
                {
                    timed_out_blocks.push(block.clone());
                    timeout.0 = new_timeout;
                }
            } else {
                break;
            }
        }

        timed_out_blocks
    }

    pub fn get_blocks_for_piece(&self, piece_index: usize) -> Option<&[T]> {
        self.requests.get(&piece_index).map(|v| v.as_slice())
    }

    pub fn contains(&self, v: &T) -> bool {
        self.index.contains_key(v)
    }

    /// How many in-flight items there are.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn len_pieces(&self) -> usize {
        self.requests.len()
    }

    /// If there are no more items to request.
    pub fn is_requests_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// If there are no items in-flight.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::BlockInfo;
    use std::time::Duration;

    #[test]
    fn update_response_time() {
        let mut manager = RequestManager::<BlockInfo>::new();

        manager.update_response_time(Duration::from_millis(100));
        manager.update_response_time(Duration::from_millis(200));
        manager.update_response_time(Duration::from_millis(300));

        assert_eq!(manager.avg_response_time, Duration::from_millis(156));
        assert!(manager.get_timeout() < Duration::from_millis(500));

        println!("avg     {:?}", manager.avg_response_time);
        println!("timeout {:?}", manager.get_timeout());

        // should keep only the last 10 response times
        for i in 4..24 {
            manager.update_response_time(Duration::from_millis(i * 100));
        }

        assert_eq!(manager.response_times.len(), 10);
    }

    #[test]
    fn adjust_timeout_multiplier() {
        let mut manager = RequestManager::<BlockInfo>::new();

        manager.response_times.extend(vec![
            Duration::from_millis(100),
            Duration::from_millis(110),
            Duration::from_millis(105),
            Duration::from_millis(95),
            Duration::from_millis(100),
            Duration::from_millis(100),
            Duration::from_millis(100),
            Duration::from_millis(100),
            Duration::from_millis(100),
            Duration::from_millis(100),
        ]);

        manager.avg_response_time = Duration::from_millis(100);

        let initial_multiplier = manager.timeout_multiplier;
        manager.adjust_timeout_multiplier();
        println!("multi {:?}", manager.timeout_multiplier);

        // multiplier should decrease for low variance
        assert!(manager.timeout_multiplier < initial_multiplier);
        assert!(manager.timeout_multiplier >= 2.0);

        // test with high variance
        manager.response_times.extend(vec![
            Duration::from_millis(100),
            Duration::from_millis(1000),
            Duration::from_millis(50),
            Duration::from_millis(2000),
            Duration::from_millis(200),
            Duration::from_millis(9000),
            Duration::from_millis(7000),
            Duration::from_millis(10),
        ]);
        manager.avg_response_time = Duration::from_millis(500);

        let current_multiplier = manager.timeout_multiplier;
        manager.adjust_timeout_multiplier();
        println!("multi {:?}", manager.timeout_multiplier);

        // multiplier should increase for high variance
        assert!(manager.timeout_multiplier > current_multiplier);
        assert!(manager.timeout_multiplier <= 4.0);
    }

    #[test]
    fn add_request() {
        let mut manager = RequestManager::<BlockInfo>::new();
        let block = BlockInfo::new(0, 0, 16384);

        assert!(manager.add_request(block.clone()));
        // never add duplicates
        assert!(!manager.add_request(block.clone()));
        assert!(!manager.add_request(block.clone()));

        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);

        assert!(manager.requests.contains_key(&0));
        assert_eq!(manager.timeouts.peek().unwrap().1, block);
        assert!(manager.index.contains_key(&block));

        assert_eq!(manager.requests[&0][0], block);
        assert_eq!(manager.index[&block], 0);
    }

    #[test]
    fn add_multiple_requests_same_piece() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);

        manager.add_request(block1.clone());
        manager.add_request(block2.clone());

        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);

        assert_eq!(manager.requests[&0].len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index[&block1], 0);
        assert_eq!(manager.index[&block2], 1);
    }

    #[test]
    fn test_add_multiple_requests_different_pieces() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(1, 0, 16384);

        manager.add_request(block1.clone());
        manager.add_request(block2.clone());

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);

        assert!(manager.requests.contains_key(&0));
        assert!(manager.requests.contains_key(&1));
        assert_eq!(manager.timeouts.len(), 2);
    }

    #[test]
    fn banana() {
        let mut manager = RequestManager::new();
        let block = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(1, 0, 16384);

        assert!(manager.add_request(block.clone()));
        assert!(manager.add_request(block2.clone()));

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);

        assert!(manager.remove_request(&block));
        assert!(!manager.remove_request(&block));
        assert!(!manager.remove_request(&block));

        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);

        assert!(manager.remove_request(&block2));
        assert!(!manager.remove_request(&block2));
        assert!(!manager.remove_request(&block2));

        assert!(manager.requests.is_empty());
        assert!(manager.timeouts.is_empty());
        assert!(manager.index.is_empty());
    }

    #[test]
    fn remove_request_from_multiple() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);

        assert!(manager.add_request(block1.clone()));
        assert!(manager.add_request(block2.clone()));
        assert_eq!(manager.requests[&0].len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);

        assert!(manager.remove_request(&block1));
        assert_eq!(manager.requests[&0].len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);
        assert_eq!(manager.requests[&0][0], block2);
    }

    #[test]
    fn remove_nonexistent_request() {
        let mut manager = RequestManager::new();
        let block = BlockInfo::new(0, 0, 16384);

        // Should not panic
        manager.remove_request(&block);
    }

    #[test]
    fn get_timeout_blocks() {
        let mut manager = RequestManager::new();

        // add a block that timed out in the past
        let timed_out_block = BlockInfo::new(0, 0, 16384);
        manager.add_request(timed_out_block.clone());

        let past_timeout = Instant::now() - Duration::from_secs(10);
        manager.timeouts.push((Reverse(past_timeout), timed_out_block.clone()));
        manager.request_start_times.insert(
            timed_out_block.clone(),
            Instant::now() - Duration::from_secs(10),
        );

        // add a block that hasn't timed out yet
        let not_timed_out_block = BlockInfo::new(0, 16384, 16384);
        manager.add_request(not_timed_out_block.clone());

        let timed_out_blocks = manager.get_timeout_blocks();

        assert_eq!(timed_out_blocks.len(), 1);
        assert_eq!(*timed_out_blocks, [&timed_out_block]);
    }

    #[tokio::test]
    async fn get_timeout_blocks_and_update() {
        let mut manager = RequestManager::new();

        let timed_out_block1 = BlockInfo::new(0, 0, 100);
        manager.add_request(timed_out_block1.clone());

        let timed_out_block2 = BlockInfo::new(0, 100, 200);
        manager.add_request(timed_out_block2.clone());

        let past_timeout = Instant::now() - Duration::from_secs(10);
        manager
            .timeouts
            .push((Reverse(past_timeout), timed_out_block1.clone()));
        manager.request_start_times.insert(
            timed_out_block1.clone(),
            Instant::now() - Duration::from_secs(10),
        );

        let past_timeout = Instant::now() - Duration::from_secs(10);
        manager
            .timeouts
            .push((Reverse(past_timeout), timed_out_block2.clone()));
        manager.request_start_times.insert(
            timed_out_block2.clone(),
            Instant::now() - Duration::from_secs(10),
        );

        let timed_out_blocks = manager.get_timeout_blocks_and_update();

        assert_eq!(timed_out_blocks.len(), 2);
        assert_eq!(timed_out_blocks[0], timed_out_block1);
        assert_eq!(timed_out_blocks[1], timed_out_block2);

        let timed_out_block = manager.timeouts.pop().unwrap();
        assert_eq!(timed_out_block.1, timed_out_block1);

        let timed_out_block = manager.timeouts.pop().unwrap();
        assert_eq!(timed_out_block.1, timed_out_block2);

        let timed_out_blocks = manager.get_timeout_blocks();
        assert!(timed_out_blocks.is_empty());
    }

    #[test]
    fn get_timedout_blocks_empty() {
        let manager = RequestManager::<BlockInfo>::new();
        let timed_out_blocks = manager.get_timeout_blocks();
        assert!(timed_out_blocks.is_empty());
    }

    #[test]
    fn get_blocks_for_piece() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);
        let block3 = BlockInfo::new(1, 0, 16384);

        manager.add_request(block1.clone());
        manager.add_request(block2.clone());
        manager.add_request(block3.clone());

        let blocks_for_piece_0 = manager.get_blocks_for_piece(0).unwrap();
        assert_eq!(blocks_for_piece_0.len(), 2);
        assert!(blocks_for_piece_0.contains(&block1));
        assert!(blocks_for_piece_0.contains(&block2));

        let blocks_for_piece_1 = manager.get_blocks_for_piece(1).unwrap();
        assert_eq!(blocks_for_piece_1.len(), 1);
        assert!(blocks_for_piece_1.contains(&block3));

        assert!(manager.get_blocks_for_piece(2).is_none());
    }

    #[test]
    fn drain() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);
        let block3 = BlockInfo::new(1, 0, 16384);

        manager.add_request(block1.clone());
        manager.add_request(block2.clone());
        manager.add_request(block3.clone());

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 3);
        assert_eq!(manager.index.len(), 3);

        let drained = manager.drain();

        assert!(manager.requests.is_empty());
        assert!(manager.timeouts.is_empty());
        assert!(manager.index.is_empty());

        assert_eq!(drained.len(), 2);
        assert_eq!(drained[&0].len(), 2);
        assert_eq!(drained[&1].len(), 1);

        assert!(drained[&0].contains(&block1));
        assert!(drained[&0].contains(&block2));
        assert!(drained[&1].contains(&block3));

        let drained = manager.drain();
        assert!(drained.is_empty());
    }

    #[test]
    fn remove_request_updates_index_correctly() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);
        let block3 = BlockInfo::new(0, 32768, 16384);

        manager.add_request(block1.clone());
        manager.add_request(block2.clone());
        manager.add_request(block3.clone());

        assert_eq!(manager.index[&block1], 0);
        assert_eq!(manager.index[&block2], 1);
        assert_eq!(manager.index[&block3], 2);

        manager.remove_request(&block2);

        assert_eq!(manager.index[&block1], 0);
        assert_eq!(manager.index[&block3], 1);
        assert!(!manager.index.contains_key(&block2));

        assert_eq!(manager.requests[&0].len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.requests[&0][0], block1);
        assert_eq!(manager.requests[&0][1], block3);
    }
}
