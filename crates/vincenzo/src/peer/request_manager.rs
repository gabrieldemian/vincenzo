use std::{
    collections::{BTreeMap, BinaryHeap},
    hash::Hash,
};

use bitvec::{bitvec, order::Msb0};
use hashbrown::HashMap;
use std::cmp::Reverse;
use tokio::time::Instant;

pub(crate) trait Managed =
    Eq + Default + Clone + Ord + Hash where for<'a> &'a Self: Into<usize>;

/// Struct to centralize the logic of requesting something.
/// This will be implemented by BlockInfo and usize (metadata pieces).
#[derive(Default)]
pub(crate) struct RequestManager<T: Managed> {
    timeouts: BinaryHeap<(Reverse<Instant>, T)>,
    requests: BTreeMap<usize, Vec<T>>,
    // reverse index for requests
    // `BlockInfo → index in Vec`
    index: HashMap<T, usize>,
}

impl<T: Managed> RequestManager<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drain(&mut self) -> BTreeMap<usize, Vec<T>> {
        self.index.clear();
        self.timeouts.clear();
        std::mem::take(&mut self.requests)
    }

    pub fn add_request(&mut self, block: T, timeout: Instant) {
        let i: usize = (&block).into();
        let req_entry = self.requests.entry(i).or_default();
        let pos = req_entry.len();

        req_entry.push(block.clone());
        self.index.insert(block.clone(), pos);
        self.timeouts.push((Reverse(timeout), block.clone()));
    }

    /// Return true if the request exists, and false otherwise.
    pub fn remove_request(&mut self, block: &T) -> bool {
        let Some(pos) = self.index.remove(block) else { return false };
        let i: usize = block.into();

        let Some(blocks) = self.requests.get_mut(&i) else { return false };
        blocks.remove(pos);

        self.timeouts.retain(|v| v.1 != *block);

        // update indices for remaining blocks in the same piece
        for (new_pos, remaining_block) in blocks.iter().enumerate().skip(pos) {
            self.index.insert(remaining_block.clone(), new_pos);
        }

        if blocks.is_empty() {
            self.requests.remove(&i);
        }

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
    pub fn get_timeout_blocks(&self, now: Instant) -> Vec<&T> {
        // assume around 25% of blocks are timed out
        let mut timed_out_blocks: Vec<&T> =
            Vec::with_capacity(self.timeouts.len() / 4);

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

    /// Get timed out blocks and mutate the timeout to now.
    pub fn get_timeout_blocks_and_update(
        &mut self,
        new_timeout: Instant,
    ) -> Vec<T> {
        // assume around 25% of blocks are timed out
        let mut timed_out_blocks: Vec<T> =
            Vec::with_capacity(self.timeouts.len() / 4);

        let now = Instant::now();

        for (mut timeout, block) in self.timeouts.iter().peekable() {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::BlockInfo;
    use std::time::Duration;
    use tokio::time::Instant;

    #[test]
    fn get_timedout_blocks_performance() {
        let mut manager = RequestManager::<BlockInfo>::new();
        let now = Instant::now();

        for i in 0..10_00 {
            let block =
                BlockInfo::new(i as u32 / 10, (i as u32 % 10) * 16384, 16384);
            let timeout = if i % 10 == 0 {
                now - Duration::from_secs(10) // 10% time out
            } else {
                now + Duration::from_secs(10) // 90% don't time out
            };
            manager.add_request(block.clone(), timeout);
        }

        let start = Instant::now();
        let _timed_out = manager.get_timeout_blocks(now);
        let duration = start.elapsed();
        println!("timedout_heap took {:?}", duration);
    }

    #[test]
    fn add_request() {
        let mut manager = RequestManager::<BlockInfo>::new();
        let block = BlockInfo::new(0, 0, 16384);
        let timeout = Instant::now();

        manager.add_request(block.clone(), timeout);

        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);

        assert!(manager.requests.contains_key(&0));
        assert_eq!(manager.timeouts.peek().unwrap().1, block);
        assert!(manager.index.contains_key(&block));

        assert_eq!(manager.requests[&0][0], block);
        assert_eq!(manager.timeouts.peek().unwrap().0 .0, timeout);
        assert_eq!(manager.index[&block], 0);
    }

    #[test]
    fn add_multiple_requests_same_piece() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);

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
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);

        assert!(manager.requests.contains_key(&0));
        assert!(manager.requests.contains_key(&1));
        assert_eq!(manager.timeouts.len(), 2);
    }

    #[test]
    fn remove_request() {
        let mut manager = RequestManager::new();
        let block = BlockInfo::new(0, 0, 16384);
        let timeout = Instant::now();

        manager.add_request(block.clone(), timeout);
        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);

        manager.remove_request(&block);
        assert!(manager.requests.is_empty());
        assert!(manager.timeouts.is_empty());
        assert!(manager.index.is_empty());
    }

    #[test]
    fn remove_request_from_multiple() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);
        assert_eq!(manager.requests[&0].len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);

        manager.remove_request(&block1);
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
        let now = Instant::now();

        // add a block that timed out in the past
        let timed_out_block = BlockInfo::new(0, 0, 16384);
        manager.add_request(
            timed_out_block.clone(),
            now - Duration::from_secs(10),
        );

        // add a block that hasn't timed out yet
        let not_timed_out_block = BlockInfo::new(0, 16384, 16384);
        manager.add_request(
            not_timed_out_block.clone(),
            now + Duration::from_secs(10),
        );

        let timed_out_blocks = manager.get_timeout_blocks(now);

        assert_eq!(timed_out_blocks.len(), 1);
        assert_eq!(*timed_out_blocks[0], timed_out_block);
        assert!(!timed_out_blocks.contains(&&not_timed_out_block));
    }

    #[tokio::test]
    async fn get_timeout_blocks_and_update() {
        let mut manager = RequestManager::new();

        let now = Reverse(Instant::now());

        let timed_out_block1 = BlockInfo::new(0, 0, 100);
        manager.add_request(
            timed_out_block1.clone(),
            now.0 - Duration::from_secs(15),
        );
        let timed_out_block2 = BlockInfo::new(0, 100, 200);
        manager.add_request(
            timed_out_block2.clone(),
            now.0 - Duration::from_secs(10),
        );

        let timed_out_blocks = manager
            .get_timeout_blocks_and_update(now.0 + Duration::from_secs(60));

        assert_eq!(timed_out_blocks.len(), 2);
        assert_eq!(timed_out_blocks[0], timed_out_block1);
        assert_eq!(timed_out_blocks[1], timed_out_block2);

        let timed_out_block = manager.timeouts.pop().unwrap();
        assert_eq!(timed_out_block.1, timed_out_block1);
        assert!(timed_out_block.0 > now);

        let timed_out_block = manager.timeouts.pop().unwrap();
        assert_eq!(timed_out_block.1, timed_out_block2);
        assert!(timed_out_block.0 > now);

        let timed_out_blocks = manager.get_timeout_blocks(now.0);
        assert!(timed_out_blocks.is_empty());
    }

    #[test]
    fn get_timedout_blocks_empty() {
        let manager = RequestManager::<BlockInfo>::new();
        let timed_out_blocks = manager.get_timeout_blocks(Instant::now());
        assert!(timed_out_blocks.is_empty());
    }

    #[test]
    fn get_blocks_for_piece() {
        let mut manager = RequestManager::new();
        let block1 = BlockInfo::new(0, 0, 16384);
        let block2 = BlockInfo::new(0, 16384, 16384);
        let block3 = BlockInfo::new(1, 0, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);
        manager.add_request(block3.clone(), timeout);

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
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);
        manager.add_request(block3.clone(), timeout);

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
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);
        manager.add_request(block3.clone(), timeout);

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
