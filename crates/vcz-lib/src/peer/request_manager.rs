use crate::peer::DEFAULT_REQUEST_QUEUE_LEN;
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
    hash::Hash,
    time::Duration,
};
use tokio::time::Instant;

/// A type that can be requested.
pub trait Requestable = Clone + Ord + Hash;

/// Struct to centralize the logic of requesting something,
/// used by BlockInfo and MetadataPiece (usize).
///
/// It also handles the calculation of timeouts with EMA (exponential moving
/// average) and a dynamic multiplier.
pub struct RequestManager<T: Requestable> {
    /// When there is no space in `requests` it will be added to the queue.
    queue: Vec<T>,
    timeouts: BinaryHeap<(Reverse<Instant>, T)>,
    requests: Vec<T>,

    /// Inverse index for requests
    index: HashMap<T, usize>,

    /// Same as index but used to identify duplicates, even if they were
    /// deleted.
    dupe_index: HashSet<T>,

    /// Max amount of inflight pending requests
    limit: usize,

    /// Last 10 response times
    recv_times: VecDeque<Duration>,

    /// Smoothed average
    avg_recv_time: Duration,

    /// Multiplier of the average (to calculate timeout), low on low variance
    /// requests and high otherwise.
    timeout_mult: f32,

    /// Track when each request was made
    req_start_times: HashMap<T, Instant>,
}

impl<T: Requestable> Default for RequestManager<T> {
    fn default() -> Self {
        Self {
            timeouts: BinaryHeap::with_capacity(
                DEFAULT_REQUEST_QUEUE_LEN as usize,
            ),
            requests: Vec::with_capacity(DEFAULT_REQUEST_QUEUE_LEN as usize),
            queue: Vec::with_capacity(DEFAULT_REQUEST_QUEUE_LEN as usize),
            index: HashMap::with_capacity(DEFAULT_REQUEST_QUEUE_LEN as usize),
            dupe_index: HashSet::with_capacity(
                DEFAULT_REQUEST_QUEUE_LEN as usize,
            ),
            recv_times: VecDeque::with_capacity(10),
            avg_recv_time: Duration::ZERO,
            timeout_mult: 3.0,
            req_start_times: HashMap::with_capacity(
                DEFAULT_REQUEST_QUEUE_LEN as usize,
            ),
            limit: DEFAULT_REQUEST_QUEUE_LEN as usize,
        }
    }
}

impl<T: Requestable> RequestManager<T> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(DEFAULT_REQUEST_QUEUE_LEN as usize);
    }

    #[inline]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// How many requests the client can make right now.
    pub fn get_available_request_len(&self) -> usize {
        self.limit.saturating_sub(self.len())
    }

    #[inline]
    pub fn get_avg(&self) -> Duration {
        self.avg_recv_time
    }

    pub fn get_timeout(&self) -> Duration {
        if self.avg_recv_time == Duration::ZERO {
            return Duration::from_secs(3);
        }

        let timeout = self.avg_recv_time.mul_f32(self.timeout_mult);

        // if the timeout is higher than the maximum (10 seconds)
        // what should the client do? choke the peer?

        timeout.clamp(Duration::from_millis(100), Duration::from_secs(10))
    }

    fn update_response_time(&mut self, response_time: Duration) {
        // keep only the last 10 response times
        if self.recv_times.len() >= 10 {
            self.recv_times.pop_front();
        }
        self.recv_times.push_back(response_time);

        // calculate exponential moving average
        let alpha = 0.2; // smoothing factor
        if self.avg_recv_time == Duration::ZERO {
            self.avg_recv_time = response_time;
        } else {
            let current_avg = self.avg_recv_time.as_micros() as f64;
            let new_time = response_time.as_micros() as f64;
            let new_avg = alpha * new_time + (1.0 - alpha) * current_avg;
            self.avg_recv_time = Duration::from_micros(new_avg as u64);
        }

        self.adjust_timeout_multiplier();
    }

    /// Adjust timeout multiplier based on response time variance
    fn adjust_timeout_multiplier(&mut self) {
        if self.recv_times.len() < 5 {
            return;
        }

        // calculate variance of response times
        let avg = self.avg_recv_time.as_millis() as f64;
        let variance = self
            .recv_times
            .iter()
            .map(|d| {
                let time_in_millis = d.as_millis() as f64;
                (time_in_millis - avg).powi(2)
            })
            .sum::<f64>()
            / self.recv_times.len() as f64;

        // adjust multiplier based on variance
        if variance > 100.0 {
            // high variance
            self.timeout_mult = (self.timeout_mult * 1.1).min(4.0);
        } else {
            // low variance
            self.timeout_mult = (self.timeout_mult * 0.9).max(2.0);
        }
    }

    pub fn drain(&mut self) -> Vec<T> {
        let mut r = std::mem::take(&mut self.requests);
        let q = std::mem::take(&mut self.queue);
        r.extend(q);
        self.clear();
        r
    }

    pub fn clear(&mut self) {
        *self = Self {
            limit: self.limit,
            timeout_mult: self.timeout_mult,
            ..Default::default()
        };
    }

    /// Drain `qnt` requests and mark them as fulfilled.
    pub fn steal_qnt(&mut self, qnt: usize) -> Vec<T> {
        self.try_move_queue(self.limit);
        let qnt = qnt.min(self.len());
        let mut r = Vec::with_capacity(qnt);
        for _ in 0..qnt {
            if let Some(v) = self.pop() {
                r.push(v);
            }
        }
        self.try_move_queue(self.limit);
        r
    }

    /// Delete the most recent item
    fn pop(&mut self) -> Option<T> {
        if let Some(v) = self.requests.last().cloned() {
            self.fulfill_request(&v);
            return Some(v);
        }
        None
    }

    pub fn extend(&mut self, blocks: Vec<T>) {
        for block in blocks {
            self.add_request(block);
        }
    }

    /// Return true if the item was inserted, false if duplicate.
    pub fn add_request(&mut self, block: T) -> bool {
        // avoid duplicates
        if self.index.contains_key(&block) || self.dupe_index.contains(&block) {
            return false;
        }

        if self.is_full() {
            self.queue.push(block);
            return false;
        }

        let timeout = Instant::now() + self.get_timeout();
        self.req_start_times.insert(block.clone(), Instant::now());
        self.requests.push(block.clone());
        self.index.insert(block.clone(), self.requests.len() - 1);
        self.dupe_index.insert(block.clone());
        self.timeouts.push((Reverse(timeout), block));
        true
    }

    /// Clone requests by `qnt`.
    #[inline]
    pub fn clone_requests(&mut self) -> Vec<T> {
        self.requests.to_vec()
    }

    /// Clone queue by `qnt`.
    #[inline]
    pub fn clone_queue(&mut self) -> Vec<T> {
        self.queue.to_vec()
    }

    /// Mark request as completed. Return true if the request exists, and false
    /// otherwise.
    pub fn fulfill_request(&mut self, req: &T) -> bool {
        if let Some(pos) = self.queue.iter().position(|v| *v == *req) {
            self.queue.swap_remove(pos);
            return true;
        }

        if self.dupe_index.contains(req) && !self.index.contains_key(req) {
            return true;
        }
        let Some(pos) = self.index.remove(req) else { return false };

        if let Some(start_time) = self.req_start_times.remove(req) {
            let response_time = Instant::now().duration_since(start_time);
            self.update_response_time(response_time);
        }

        self.requests.remove(pos);
        self.timeouts.retain(|v| v.1 != *req);

        // update indices for remaining blocks in the same piece
        for (new_pos, remaining_block) in
            self.requests.iter().enumerate().skip(pos)
        {
            self.index.insert(remaining_block.clone(), new_pos);
        }

        if let Some(idx) = self.queue.iter().position(|v| *v == *req) {
            self.queue.remove(idx);
        }

        self.try_move_queue(self.limit);

        true
    }

    pub fn try_move_queue(&mut self, n: usize) -> bool {
        let n = n.min(self.queue.len());
        let n = self.get_available_request_len().min(n);
        if n == 0 {
            return false;
        };
        for _ in 0..n {
            let v = self.queue.pop().unwrap();
            self.add_request(v);
        }
        true
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

        for &(mut timeout, ref block) in
            self.timeouts.iter().take(self.get_available_request_len())
        {
            // timeout.0 is read in other places, shut up linter.
            #[allow(unused_assignments)]
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

    /// How many in-flight items there are.
    #[inline]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.index.len() >= self.limit
    }

    #[inline]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// If there are no items in-flight.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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

        assert_eq!(manager.avg_recv_time, Duration::from_millis(156));
        assert!(manager.get_timeout() < Duration::from_millis(500));

        println!("avg     {:?}", manager.avg_recv_time);
        println!("timeout {:?}", manager.get_timeout());

        // should keep only the last 10 response times
        for i in 4..24 {
            manager.update_response_time(Duration::from_millis(i * 100));
        }

        assert_eq!(manager.recv_times.len(), 10);
    }

    #[test]
    fn adjust_timeout_multiplier() {
        let mut manager = RequestManager::<BlockInfo>::new();

        manager.recv_times.extend(vec![
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

        manager.avg_recv_time = Duration::from_millis(100);

        let initial_multiplier = manager.timeout_mult;
        manager.adjust_timeout_multiplier();
        println!("multi {:?}", manager.timeout_mult);

        // multiplier should decrease for low variance
        assert!(manager.timeout_mult < initial_multiplier);
        assert!(manager.timeout_mult >= 2.0);

        // test with high variance
        manager.recv_times.extend(vec![
            Duration::from_millis(100),
            Duration::from_millis(1000),
            Duration::from_millis(50),
            Duration::from_millis(2000),
            Duration::from_millis(200),
            Duration::from_millis(9000),
            Duration::from_millis(7000),
            Duration::from_millis(10),
        ]);
        manager.avg_recv_time = Duration::from_millis(500);

        let current_multiplier = manager.timeout_mult;
        manager.adjust_timeout_multiplier();
        println!("multi {:?}", manager.timeout_mult);

        // multiplier should increase for high variance
        assert!(manager.timeout_mult > current_multiplier);
        assert!(manager.timeout_mult <= 4.0);
    }

    #[test]
    fn add_request() {
        let mut manager = RequestManager::<BlockInfo>::new();
        let block_info = BlockInfo::new(0, 0, 16384);

        assert!(manager.add_request(block_info.clone()));
        // never add duplicates
        assert!(!manager.add_request(block_info.clone()));
        assert!(!manager.add_request(block_info.clone()));
        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);
        assert_eq!(manager.timeouts.peek().unwrap().1, block_info);
        assert!(manager.index.contains_key(&block_info));
        assert_eq!(manager.requests[0], block_info);
        assert_eq!(manager.index[&block_info], 0);
    }

    #[test]
    fn add_multiple_requests_same_piece() {
        let mut manager = RequestManager::new();
        let block0 = BlockInfo::new(0, 0, 16384);
        let block1 = BlockInfo::new(0, 16384, 16384);

        manager.add_request(block0.clone());
        manager.add_request(block1.clone());

        assert_eq!(manager.index.len(), 2);
        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index[&block0], 0);
        assert_eq!(manager.index[&block1], 1);
    }

    #[test]
    fn test_add_multiple_requests_different_pieces() {
        let mut manager = RequestManager::new();
        let block0 = BlockInfo::new(0, 0, 16384);
        let block1 = BlockInfo::new(1, 0, 16384);

        manager.add_request(block0.clone());
        manager.add_request(block1.clone());

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);
        assert_eq!(manager.requests[0], block0);
        assert_eq!(manager.requests[1], block1);
    }

    #[test]
    fn banana() {
        let mut manager = RequestManager::new();
        let block0 = BlockInfo::new(0, 0, 16384);
        let block1 = BlockInfo::new(1, 0, 16384);

        assert!(manager.add_request(block0.clone()));
        assert!(manager.add_request(block1.clone()));

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);
        assert_eq!(manager.len(), 2);

        assert!(manager.fulfill_request(&block0));
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.dupe_index.len(), 2);
        assert_eq!(manager.index.len(), 1);

        assert!(manager.fulfill_request(&block0));
        assert!(manager.fulfill_request(&block0));

        assert_eq!(manager.len(), 1);
        assert_eq!(manager.dupe_index.len(), 2);
        assert_eq!(manager.index.len(), 1);

        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.dupe_index.len(), 2);

        assert!(manager.fulfill_request(&block1));
        assert!(manager.fulfill_request(&block1));
        assert!(manager.fulfill_request(&block1));

        assert!(manager.requests.is_empty());
        assert!(manager.timeouts.is_empty());
        assert!(manager.index.is_empty());
        assert!(manager.is_empty());
        assert_eq!(manager.requests.len(), 0);
        assert_eq!(manager.timeouts.len(), 0);
        assert_eq!(manager.index.len(), 0);
        assert_eq!(manager.len(), 0);
        assert_eq!(manager.dupe_index.len(), 2);
    }

    #[test]
    fn remove_request_from_multiple() {
        let mut manager = RequestManager::new();
        let block0 = BlockInfo::new(0, 0, 16384);
        let block1 = BlockInfo::new(0, 16384, 16384);

        assert!(manager.add_request(block0.clone()));
        assert!(manager.add_request(block1.clone()));

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);
        assert_eq!(manager.len(), 2);

        assert!(manager.fulfill_request(&block0));
        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);
        assert_eq!(manager.len(), 1);
        assert_eq!(manager.requests[0], block1);
    }

    #[test]
    fn remove_nonexistent_request() {
        let mut manager = RequestManager::new();
        let block = BlockInfo::new(0, 0, 16384);
        assert!(!manager.fulfill_request(&block));
        manager.add_request(block.clone());
        assert!(manager.fulfill_request(&block));
    }

    #[test]
    fn get_timeout_blocks() {
        let mut manager = RequestManager::new();

        // add a block that timed out in the past
        let timed_out_block = BlockInfo::new(0, 0, 16384);
        manager.add_request(timed_out_block.clone());

        let past_timeout = Instant::now() - Duration::from_secs(10);
        manager.timeouts.push((Reverse(past_timeout), timed_out_block.clone()));
        manager.req_start_times.insert(
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
        manager.req_start_times.insert(
            timed_out_block1.clone(),
            Instant::now() - Duration::from_secs(10),
        );

        let past_timeout = Instant::now() - Duration::from_secs(10);
        manager
            .timeouts
            .push((Reverse(past_timeout), timed_out_block2.clone()));
        manager.req_start_times.insert(
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
    fn drain() {
        let mut manager = RequestManager::new();
        let block0 = BlockInfo::new(0, 0, 16384);
        let block1 = BlockInfo::new(0, 16384, 16384);
        let block2 = BlockInfo::new(1, 0, 16384);

        manager.add_request(block0.clone());
        manager.add_request(block1.clone());
        manager.add_request(block2.clone());

        assert_eq!(manager.requests.len(), 3);
        assert_eq!(manager.timeouts.len(), 3);
        assert_eq!(manager.index.len(), 3);

        let drained = manager.drain();

        assert!(manager.is_empty());
        assert!(manager.requests.is_empty());
        assert!(manager.timeouts.is_empty());
        assert!(manager.index.is_empty());

        assert_eq!(drained.len(), 3);

        assert_eq!(drained[0], block0);
        assert_eq!(drained[1], block1);
        assert_eq!(drained[2], block2);

        let drained = manager.drain();
        assert!(drained.is_empty());
        assert!(manager.is_empty());
    }

    #[test]
    fn remove_request_updates_index_correctly() {
        let mut manager = RequestManager::new();
        let block0 = BlockInfo::new(0, 0, 16384);
        let block1 = BlockInfo::new(0, 16384, 16384);
        let block2 = BlockInfo::new(0, 32768, 16384);
        assert!(manager.is_empty());

        manager.add_request(block0.clone());
        manager.add_request(block1.clone());
        manager.add_request(block2.clone());
        assert!(!manager.is_empty());

        assert_eq!(manager.index[&block0], 0);
        assert_eq!(manager.index[&block1], 1);
        assert_eq!(manager.index[&block2], 2);

        manager.fulfill_request(&block1);

        assert_eq!(manager.index.len(), 2);
        assert_eq!(manager.index[&block0], 0);
        assert_eq!(manager.index[&block2], 1);
        assert!(manager.index.contains_key(&block0));
        assert!(!manager.index.contains_key(&block1));
        assert!(manager.index.contains_key(&block2));

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.requests[0], block0);
        assert_eq!(manager.requests[1], block2);

        manager.fulfill_request(&block0);
        manager.fulfill_request(&block1);
        assert!(!manager.is_empty());
    }

    #[test]
    fn no_dupes() {
        let mut manager = RequestManager::new();
        let block0 = BlockInfo::new(0, 0, 16384);
        let block1 = BlockInfo::new(0, 16384, 16384);
        let block2 = BlockInfo::new(0, 32768, 16384);

        assert!(!manager.fulfill_request(&block2));

        assert!(manager.add_request(block0.clone()));
        assert!(manager.add_request(block1.clone()));
        assert!(manager.add_request(block2.clone()));

        assert!(!manager.add_request(block2.clone()));
        assert_eq!(manager.len(), 3);
        assert_eq!(manager.index.len(), 3);
        assert_eq!(manager.dupe_index.len(), 3);
        assert!(!manager.add_request(block0.clone()));
        assert_eq!(manager.len(), 3);
        assert_eq!(manager.index.len(), 3);
        assert_eq!(manager.dupe_index.len(), 3);

        assert!(manager.fulfill_request(&block2));
        assert!(!manager.add_request(block2.clone()));

        // don't add this request as it was already added and fulfilled
        assert!(!manager.add_request(block2.clone()));

        // but return true because it was already fulfilled
        assert!(manager.fulfill_request(&block2));

        assert_eq!(manager.len(), 2);
        assert_eq!(manager.index.len(), 2);
        assert_eq!(manager.dupe_index.len(), 3);
    }
}
