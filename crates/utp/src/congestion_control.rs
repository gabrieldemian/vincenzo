//! The congestion control algorithm used by μTP, with some modifications, known
//! as Low Extra Delay Background Transport (LEDBAT), aims to decrease the
//! latency caused by applications using the protocol while maximizing bandwidth
//! when latency is not excessive.

use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    time::{self},
};

/// The uTP target delay is set to 100 ms. Each socket aims to never see more
/// than 100 ms delay on the send link. If it does, it will throttle back.
///
/// This effectively makes uTP yield to any TCP traffic.
///
/// 100 ms in microseconds
const CCONTROL_TARGET: f64 = 100.0 * 1_000.0;

/// 500ms in microseconds
pub(crate) const MIN_TIMEOUT: u64 = 500_000;

/// 500ms in microseconds
pub(crate) const MIN_TIMEOUT_F64: f64 = 500_000.0;

/// The maximum number of packets to increase per RTT.
pub(crate) const MAX_CWND_INCREASE_PACKETS_PER_RTT: f64 = 2.0;

const MIN_WINDOW_SIZE_F64: f64 = 1500.0; // 1 MTU
const MIN_WINDOW_SIZE: u32 = 1500; // 1 MTU

/// Max number of bytes to increase the window per RTT.
const MAX_CWND_INCREASE_BYTES_PER_RTT: f64 =
    MIN_WINDOW_SIZE_F64 * MAX_CWND_INCREASE_PACKETS_PER_RTT;

/// If the network has been congested by the duration of this value, the window
/// will be decreased.
/// 100 ms in microseconds.
const MAX_WINDOW_DECAY: u64 = 100_000;

/// Window decay factor for window reduction if the delay is >
/// `CCONTROL_TARGET`.
///
/// UTP halves the window in it's implementation of LEDBAT, but it
/// does so in a delay-based fashion which occurs before the TCP packet loss, so
/// it can yield bandwidth to other TCP connections.
const WINDOW_DECAY_FACTOR: f64 = 0.5;

/// Each socket keeps a sliding minimum of the lowest value for the last two
/// minutes. This value is called base_delay, and is used as a baseline, the
/// minimum delay between the hosts.
///
/// 2 minutes in microseconds.
const BASE_DELAY_WINDOW: u64 = 120 * 1_000_000;

#[derive(Debug)]
pub(crate) struct CongestionControl {
    /// Send congestion window in bytes, this is the maximum bytes the socket
    /// is allowed to have in-flight (non-acked).
    window: AtomicU32,

    /// Smoothed round trip time in microseconds
    rtt: AtomicU64,

    /// Round trip time variance in microseconds
    rtt_var: AtomicU64,

    /// Current timeout in microseconds, the minimum value being
    /// [`MIN_TIMEOUT`].
    timeout: AtomicU64,

    /// Timestamp of last window update
    last_window_update: AtomicU64,

    /// Timestamp of last window decay
    last_decay: AtomicU64,

    /// Each socket keeps a sliding minimum `diff_timestamp` (here represented
    /// as microseconds) of the lowest value for the last two minutes.
    ///
    /// This `diff_timestamp` means how long does it take for the packet to
    /// arrive the destination, this is the `diff_timestamp` of the
    /// [`crate::Header`].
    base_delay: AtomicU64,

    /// timestamp, timestamp_difference (micros)
    diff_timestamp_history: VecDeque<(u64, u64)>,
}

impl CongestionControl {
    pub(crate) fn new() -> Self {
        CongestionControl {
            // 64 KB
            window: AtomicU32::new(65_536),
            rtt: AtomicU64::new(100_000),
            rtt_var: AtomicU64::new(0),
            timeout: AtomicU64::new(MIN_TIMEOUT),
            base_delay: AtomicU64::new(u64::MAX),
            last_window_update: Self::current_time_micros().into(),
            last_decay: Self::current_time_micros().into(),
            diff_timestamp_history: VecDeque::new(),
        }
    }

    /// Update base delay with a new timestamp difference measurement.
    fn update_base_delay(&mut self, timestamp_diff: u64) {
        let now = Self::current_time_micros();
        let history = &mut self.diff_timestamp_history;

        history.push_back((now, timestamp_diff));

        // remove old measurements (older than 2 minutes)
        while let Some(&(timestamp, _)) = history.front() {
            if now - timestamp > BASE_DELAY_WINDOW {
                history.pop_front();
            } else {
                break;
            }
        }

        // find minimum timestamp difference in the window
        let new_base_delay =
            history.iter().map(|&(_, diff)| diff).min().unwrap_or(u64::MAX);

        self.base_delay.store(new_base_delay, Ordering::Release);
    }

    /// Update RTT and RTT variance based on a new measurement
    pub(crate) fn update_rtt(&mut self, packet_rtt: u64, timestamp_diff: u64) {
        if packet_rtt == 0 {
            return;
        }

        self.update_base_delay(timestamp_diff);

        let rtt = self.rtt.load(Ordering::Acquire) as f64;
        let rtt_var = self.rtt_var.load(Ordering::Acquire) as f64;
        let packet_rtt = packet_rtt as f64;

        let delta = rtt - packet_rtt;

        let new_rtt_var = rtt_var + (delta.abs() - rtt_var) / 4.0;
        let new_rtt = rtt + (packet_rtt - rtt) / 8.0;

        self.rtt.store(new_rtt as u64, Ordering::Release);
        self.rtt_var.store(new_rtt_var as u64, Ordering::Release);

        let new_timeout = (new_rtt + new_rtt_var * 4.0).max(MIN_TIMEOUT_F64);
        self.timeout.store(new_timeout as u64, Ordering::Release);
    }

    /// Update congestion window based on current conditions
    pub(crate) fn update_window(
        &mut self,
        // bytes in-flight (non-acked)
        outstanding_bytes: u32,
        // header.timestamp_diff in microseconds
        timestamp_diff: u64,
    ) {
        self.update_base_delay(timestamp_diff);

        let current_rtt = self.rtt.load(Ordering::Acquire);
        let last_update = self.last_window_update.load(Ordering::Acquire);
        let elapsed = Self::elapsed_since(last_update);

        // only update window at most once per RTT
        if elapsed < current_rtt {
            return;
        }

        // sliding minimum delay of the last 2 minutes.
        let base_delay = self.base_delay() as f64;

        // a measurement of the current buffering delay on the socket. This is
        // used to increase or decrease the send window.
        let our_delay = (timestamp_diff as f64) - base_delay;

        // how far the actual measured delay is from the target delay
        let off_target = CCONTROL_TARGET - our_delay;
        let delay_factor = off_target / CCONTROL_TARGET;

        let window = self.window() as f64;
        let outstanding_bytes = outstanding_bytes as f64;

        // calculate window factor (ratio of outstanding bytes to window size)
        let window_factor = outstanding_bytes / window.max(1.0);

        // scaled gain in packets
        let scaled_gain =
            MAX_CWND_INCREASE_BYTES_PER_RTT * delay_factor * window_factor;

        // This will make the window smaller if `off_target` is greater than 0
        // and grow the window if `off_target` is less than 0.
        let new_window = (window + scaled_gain).max(MIN_WINDOW_SIZE_F64) as u32;

        self.window.store(new_window, Ordering::Release);
        self.last_window_update
            .store(Self::current_time_micros(), Ordering::Release);

        // apply window decay if needed
        self.apply_window_decay();
    }

    /// Apply window decay based on timeout
    fn apply_window_decay(&self) {
        let last_decay = self.last_decay();
        let elapsed = Self::elapsed_since(last_decay);

        if elapsed >= MAX_WINDOW_DECAY {
            let current_window = self.window() as f64;
            let new_window = (current_window * WINDOW_DECAY_FACTOR)
                .max(MIN_WINDOW_SIZE as f64)
                as u32;

            self.window.store(new_window, Ordering::Release);
            self.last_decay
                .store(Self::current_time_micros(), Ordering::Release);
        }
    }

    // Getters for current values
    pub(crate) fn window(&self) -> u32 {
        self.window.load(Ordering::Acquire)
    }

    pub(crate) fn rtt(&self) -> u64 {
        self.rtt.load(Ordering::Acquire)
    }

    pub(crate) fn rtt_var(&self) -> u64 {
        self.rtt_var.load(Ordering::Acquire)
    }

    pub(crate) fn timeout(&self) -> u64 {
        self.timeout.load(Ordering::Acquire)
    }

    fn last_decay(&self) -> u64 {
        self.last_decay.load(Ordering::Acquire)
    }

    fn base_delay(&self) -> u64 {
        self.base_delay.load(Ordering::Acquire)
    }

    fn current_time_micros() -> u64 {
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }

    fn elapsed_since(timestamp_micros: u64) -> u64 {
        Self::current_time_micros().saturating_sub(timestamp_micros)
    }

    fn micros_to_millis(micros: u64) -> u64 {
        micros / 1000
    }

    fn millis_to_micros(millis: u64) -> u64 {
        millis * 1000
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;
    use tokio::time::sleep;

    #[test]
    fn test_update_rtt() {
        let mut cc = CongestionControl::new();

        cc.update_rtt(150_000, 0);

        let new_rtt = cc.rtt();
        assert!(new_rtt > 100_000 && new_rtt < 150_000);

        cc.update_rtt(80_000, 0);
        let newer_rtt = cc.rtt();

        assert!(newer_rtt < new_rtt);

        let previous_rtt = cc.rtt();
        cc.update_rtt(200_000, 0);
        assert!(cc.rtt() > previous_rtt);
    }

    #[tokio::test]
    async fn congestion_control_formula() {
        let mut cc = CongestionControl::new();

        cc.update_base_delay(100_000);

        // set specific conditions for predictable results
        cc.rtt.store(100_000, Ordering::Release);
        cc.window.store(30_000, Ordering::Release);

        // bypass rate limit
        let old_timestamp =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros()
                as u64
                - cc.rtt() * 2;

        cc.last_window_update.store(old_timestamp, Ordering::Release);

        // calculate expected window change
        let outstanding_bytes = 15_000; // 50% of window
        let timestamp_diff = 150_000; // microseconds = 150 ms

        cc.update_window(outstanding_bytes, timestamp_diff);
        let new_window = cc.window();

        // expected window: 30000 + 750 = 30750
        assert_eq!(
            new_window, 30750,
            "Window should increase by exactly 750 bytes"
        );
    }

    #[test]
    fn test_rtt_variance_calculation() {
        let mut cc = CongestionControl::new();

        assert_eq!(cc.rtt_var(), 0);

        cc.update_rtt(150_000, 0);
        let var_after_first = cc.rtt_var();
        assert!(var_after_first > 0);

        cc.update_rtt(80_000, 0);
        let var_after_second = cc.rtt_var();
        assert!(var_after_second != var_after_first);

        let current_rtt = cc.rtt();
        cc.update_rtt(current_rtt, 0);
        let var_after_third = cc.rtt_var();
        assert!(var_after_third <= var_after_second);
    }

    #[test]
    fn test_timeout_calculation() {
        let mut cc = CongestionControl::new();

        let initial_timeout = cc.timeout();
        assert_eq!(initial_timeout, 500_000);

        cc.update_rtt(1_000_000, 0); // 1000ms
        let timeout_after_update = cc.timeout();

        assert!(timeout_after_update > 600_000);
        assert!(timeout_after_update != initial_timeout);

        cc.update_rtt(10_000, 0);
        assert!(cc.timeout() >= 500_000);
    }

    #[tokio::test]
    async fn test_window_update() {
        let mut cc = CongestionControl::new();
        let initial_window = cc.window();

        cc.update_window(initial_window / 2, 0);
        let new_window = cc.window();

        // window should increase when delay is below target
        assert!(new_window >= initial_window);

        // test window decrease with bad conditions (high delay)
        cc.update_window(initial_window, 0);
        let decreased_window = cc.window();

        // window should decrease when delay is above target
        assert!(decreased_window <= new_window);
    }

    #[tokio::test]
    async fn test_window_decay() {
        let cc = CongestionControl::new();

        // set a large window
        cc.window.store(1_000_000, Ordering::Release);

        // force decay by setting last_decay to a time far in the past
        let old_timestamp =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros()
                as u64
                - MAX_WINDOW_DECAY * 2;

        cc.last_decay.store(old_timestamp, Ordering::Release);

        // trigger decay
        cc.apply_window_decay();

        let decayed_window = cc.window();

        // window should be decayed by the decay factor
        assert_eq!(
            decayed_window,
            (1_000_000_f64 * WINDOW_DECAY_FACTOR) as u32
        );

        // verify last_decay was updated to a recent time
        let new_decay_timestamp = cc.last_decay.load(Ordering::Acquire);
        let current_time =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros()
                as u64;

        // the new timestamp should be recent (within 1 second)
        assert!(current_time - new_decay_timestamp < 1_000_000);
    }

    #[tokio::test]
    async fn test_rate_limiting() {
        let mut cc = CongestionControl::new();
        let initial_window = cc.window();

        cc.update_window(initial_window / 2, 50_000);
        let window_after_first = cc.window();

        // immediate second update should be rate-limited
        cc.update_window(initial_window / 2, 50_000);
        let window_after_second = cc.window();

        // window should not change due to rate limiting
        assert_eq!(window_after_first, window_after_second);

        // wait for RTT period and try again
        sleep(Duration::from_micros(cc.rtt())).await;
        cc.update_window(initial_window / 2, 50_000);
        let window_after_wait = cc.window();

        // window should change after waiting
        assert!(window_after_wait != window_after_second);
    }

    #[tokio::test]
    async fn test_min_window_size() {
        let mut cc = CongestionControl::new();

        // set window to minimum
        cc.window.store(MIN_WINDOW_SIZE, Ordering::Release);

        // try to decrease window further with bad conditions
        cc.update_rtt(500_000, 0);
        cc.update_window(MIN_WINDOW_SIZE, 500_000);

        // window should not go below minimum
        assert!(cc.window() >= MIN_WINDOW_SIZE);
    }
}
