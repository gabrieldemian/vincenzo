use std::collections::BTreeMap;

use hashbrown::HashMap;
use tokio::time::Instant;

use crate::extensions::BlockInfo;

/// Struct to centralize the logic of requesting block infos.
#[derive(Default)]
pub struct RequestManager {
    requests: BTreeMap<usize, Vec<BlockInfo>>,
    timeouts: BTreeMap<usize, Vec<Instant>>,
    // reverse index for requests and timeouts.
    // : BlockInfo → index in Vec
    index: HashMap<BlockInfo, usize>,
    // timeout_queue: BinaryHeap<(Reverse<Instant>, BlockInfo)>,
}

impl RequestManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drain(&mut self) -> BTreeMap<usize, Vec<BlockInfo>> {
        self.index.clear();
        self.timeouts.clear();
        std::mem::take(&mut self.requests)
    }

    pub fn add_request(&mut self, block: BlockInfo, timeout: Instant) {
        let i = block.index as usize;
        let req_entry = self.requests.entry(i).or_default();
        let timeouts_entry = self.timeouts.entry(i).or_default();
        let pos = req_entry.len();

        req_entry.push(block.clone());
        timeouts_entry.push(timeout);
        self.index.insert(block, pos);
    }

    /// Return true if the request exists, and false otherwise.
    pub fn remove_request(&mut self, block: &BlockInfo) -> bool {
        let Some(pos) = self.index.remove(block) else { return false };
        let i = block.index as usize;

        let Some(blocks) = self.requests.get_mut(&i) else { return false };
        blocks.remove(pos);

        let Some(timeouts) = self.timeouts.get_mut(&i) else { return false };
        timeouts.remove(pos);

        // update indices for remaining blocks in the same piece
        for (new_pos, remaining_block) in blocks.iter().enumerate().skip(pos) {
            self.index.insert(remaining_block.clone(), new_pos);
        }

        if blocks.is_empty() {
            self.requests.remove(&i);
            self.timeouts.remove(&i);
        }

        true
    }

    pub fn get_timedout_blocks(&self, now: Instant) -> Vec<BlockInfo> {
        self.timeouts
            .iter()
            .flat_map(|(piece_index, timeouts)| {
                self.requests[piece_index]
                    .iter()
                    .zip(timeouts.iter())
                    .filter_map(|(block, timeout)| {
                        if *timeout < now {
                            Some(block.clone())
                        } else {
                            None
                        }
                    })
            })
            .collect()
    }

    pub fn get_blocks_for_piece(
        &self,
        piece_index: usize,
    ) -> Option<&[BlockInfo]> {
        self.requests.get(&piece_index).map(|v| v.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::Instant;

    fn create_test_block(piece_index: u32, begin: u32, len: u32) -> BlockInfo {
        BlockInfo { index: piece_index, begin, len }
    }

    #[test]
    fn test_new_manager_is_empty() {
        let manager = RequestManager::new();
        assert!(manager.requests.is_empty());
        assert!(manager.timeouts.is_empty());
        assert!(manager.index.is_empty());
    }

    #[test]
    fn test_add_request() {
        let mut manager = RequestManager::new();
        let block = create_test_block(0, 0, 16384);
        let timeout = Instant::now();

        manager.add_request(block.clone(), timeout);

        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 1);

        assert!(manager.requests.contains_key(&0));
        assert!(manager.timeouts.contains_key(&0));
        assert!(manager.index.contains_key(&block));

        assert_eq!(manager.requests[&0][0], block);
        assert_eq!(manager.timeouts[&0][0], timeout);
        assert_eq!(manager.index[&block], 0);
    }

    #[test]
    fn test_add_multiple_requests_same_piece() {
        let mut manager = RequestManager::new();
        let block1 = create_test_block(0, 0, 16384);
        let block2 = create_test_block(0, 16384, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);

        assert_eq!(manager.requests.len(), 1);
        assert_eq!(manager.timeouts.len(), 1);
        assert_eq!(manager.index.len(), 2);

        assert_eq!(manager.requests[&0].len(), 2);
        assert_eq!(manager.timeouts[&0].len(), 2);
        assert_eq!(manager.index[&block1], 0);
        assert_eq!(manager.index[&block2], 1);
    }

    #[test]
    fn test_add_multiple_requests_different_pieces() {
        let mut manager = RequestManager::new();
        let block1 = create_test_block(0, 0, 16384);
        let block2 = create_test_block(1, 0, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 2);

        assert!(manager.requests.contains_key(&0));
        assert!(manager.requests.contains_key(&1));
        assert!(manager.timeouts.contains_key(&0));
        assert!(manager.timeouts.contains_key(&1));
    }

    #[test]
    fn test_remove_request() {
        let mut manager = RequestManager::new();
        let block = create_test_block(0, 0, 16384);
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
    fn test_remove_request_from_multiple() {
        let mut manager = RequestManager::new();
        let block1 = create_test_block(0, 0, 16384);
        let block2 = create_test_block(0, 16384, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);
        assert_eq!(manager.requests[&0].len(), 2);
        assert_eq!(manager.timeouts[&0].len(), 2);
        assert_eq!(manager.index.len(), 2);

        manager.remove_request(&block1);
        assert_eq!(manager.requests[&0].len(), 1);
        assert_eq!(manager.timeouts[&0].len(), 1);
        assert_eq!(manager.index.len(), 1);
        assert_eq!(manager.requests[&0][0], block2);
    }

    #[test]
    fn test_remove_nonexistent_request() {
        let mut manager = RequestManager::new();
        let block = create_test_block(0, 0, 16384);

        // Should not panic
        manager.remove_request(&block);
    }

    #[test]
    fn test_get_timedout_blocks() {
        let mut manager = RequestManager::new();
        let now = Instant::now();

        // Add a block that timed out in the past
        let timed_out_block = create_test_block(0, 0, 16384);
        manager.add_request(
            timed_out_block.clone(),
            now - Duration::from_secs(10),
        );

        // Add a block that hasn't timed out yet
        let not_timed_out_block = create_test_block(0, 16384, 16384);
        manager.add_request(
            not_timed_out_block.clone(),
            now + Duration::from_secs(10),
        );

        let timed_out_blocks = manager.get_timedout_blocks(now);

        assert_eq!(timed_out_blocks.len(), 1);
        assert_eq!(timed_out_blocks[0], timed_out_block);
        assert!(!timed_out_blocks.contains(&not_timed_out_block));
    }

    #[test]
    fn test_get_timedout_blocks_empty() {
        let manager = RequestManager::new();
        let timed_out_blocks = manager.get_timedout_blocks(Instant::now());
        assert!(timed_out_blocks.is_empty());
    }

    #[test]
    fn test_get_blocks_for_piece() {
        let mut manager = RequestManager::new();
        let block1 = create_test_block(0, 0, 16384);
        let block2 = create_test_block(0, 16384, 16384);
        let block3 = create_test_block(1, 0, 16384);
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
    fn test_drain() {
        let mut manager = RequestManager::new();
        let block1 = create_test_block(0, 0, 16384);
        let block2 = create_test_block(0, 16384, 16384);
        let block3 = create_test_block(1, 0, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);
        manager.add_request(block3.clone(), timeout);

        assert_eq!(manager.requests.len(), 2);
        assert_eq!(manager.timeouts.len(), 2);
        assert_eq!(manager.index.len(), 3);

        let drained = manager.drain();

        // Check that manager is empty
        assert!(manager.requests.is_empty());
        assert!(manager.timeouts.is_empty());
        assert!(manager.index.is_empty());

        // Check that drained data is correct
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[&0].len(), 2);
        assert_eq!(drained[&1].len(), 1);

        assert!(drained[&0].contains(&block1));
        assert!(drained[&0].contains(&block2));
        assert!(drained[&1].contains(&block3));
    }

    #[test]
    fn test_drain_empty() {
        let mut manager = RequestManager::new();
        let drained = manager.drain();
        assert!(drained.is_empty());
    }

    #[test]
    fn test_remove_request_updates_index_correctly() {
        let mut manager = RequestManager::new();
        let block1 = create_test_block(0, 0, 16384);
        let block2 = create_test_block(0, 16384, 16384);
        let block3 = create_test_block(0, 32768, 16384);
        let timeout = Instant::now();

        manager.add_request(block1.clone(), timeout);
        manager.add_request(block2.clone(), timeout);
        manager.add_request(block3.clone(), timeout);

        // Verify initial indices
        assert_eq!(manager.index[&block1], 0);
        assert_eq!(manager.index[&block2], 1);
        assert_eq!(manager.index[&block3], 2);

        // Remove the middle block
        manager.remove_request(&block2);

        // Verify indices are updated correctly
        assert_eq!(manager.index[&block1], 0);
        assert_eq!(manager.index[&block3], 1);
        assert!(!manager.index.contains_key(&block2));

        // Verify requests and timeouts are updated
        assert_eq!(manager.requests[&0].len(), 2);
        assert_eq!(manager.timeouts[&0].len(), 2);
        assert_eq!(manager.requests[&0][0], block1);
        assert_eq!(manager.requests[&0][1], block3);
    }
}
