//! ACK-clocked bandwidth probing, independent of random-loss compensation.
//! Sample whole pacing intervals rather than individual compressed ACK bursts.

use std::time::Duration;

#[derive(Clone)]
pub(super) struct AutoBandwidth {
    pub target: u64,
    startup: bool,
    constrained: u8,
}

impl AutoBandwidth {
    pub fn new(initial: u64) -> Self {
        Self {
            target: initial,
            startup: true,
            constrained: 0,
        }
    }

    pub fn observe(&mut self, sample: DeliverySample, cap: u64) {
        let DeliverySample {
            elapsed,
            acked,
            sent,
            unlimited,
            rtt,
            min_rtt,
        } = sample;
        let seconds = elapsed.as_secs_f64();
        if seconds <= 0.0 || acked < 60_000 {
            return;
        }
        let sent_rate = sent as f64 / seconds;
        // The wire rate also bounds an ACK burst for packets from an earlier
        // interval. Idle/application-limited intervals cannot raise the target.
        let delivered = (acked as f64 / seconds).min(sent_rate);
        let utilized = sent_rate >= self.target as f64 * 0.70;
        // Minimum RTT includes the fastest jitter outlier; allow normal
        // cross-border variation before treating sustained delay as a queue.
        let queued = rtt > min_rtt + (min_rtt / 2).max(Duration::from_millis(50));
        let falling = delivered < self.target as f64 * 0.70;
        // Congestion/flow-control may prevent fully using the pacer after a
        // capacity drop. Those intervals must still lower the estimate when
        // application demand remains; idle data supply must not lower it.
        if !utilized && unlimited < acked / 4 {
            return;
        }
        self.constrained = if queued || falling {
            self.constrained.saturating_add(1)
        } else {
            0
        };
        if self.constrained >= 2 {
            self.startup = false;
            // Respond to sustained queue/capacity signals, not each lost packet
            // or single RTT excursion. Ordinary 5–15% loss keeps its budget.
            let decrease = if delivered < self.target as f64 * 0.5 {
                0.5
            } else {
                0.8
            };
            self.target = ((self.target as f64 * decrease).max(delivered * 1.05) as u64)
                .min(self.target)
                .max(125_000)
                .min(cap);
            self.constrained = 0;
        } else if !queued
            && !falling
            && utilized
            && (unlimited >= acked / 4 || sent_rate >= self.target as f64 * 0.90)
        {
            let gain = if self.startup { 1.5 } else { 1.08 };
            self.target = ((self.target as f64 * gain) as u64).min(cap);
        }
    }
}

pub(super) struct DeliverySample {
    pub elapsed: Duration,
    pub acked: u64,
    pub sent: u64,
    pub unlimited: u64,
    pub rtt: Duration,
    pub min_rtt: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(acked: u64, sent: u64, queued: bool) -> DeliverySample {
        DeliverySample {
            elapsed: Duration::from_millis(500),
            acked,
            sent,
            unlimited: acked,
            rtt: Duration::from_millis(if queued { 200 } else { 120 }),
            min_rtt: Duration::from_millis(120),
        }
    }
    #[test]
    fn probes_past_old_fixed_target_with_random_loss_and_hard_cap() {
        let mut auto = AutoBandwidth::new(500_000);
        for _ in 0..16 {
            let sent = auto.target / 2;
            auto.observe(sample(sent * 85 / 100, sent, false), 25_000_000);
        }
        assert_eq!(auto.target, 25_000_000);
    }
    #[test]
    fn idle_compressed_acks_and_application_limits_do_not_inflate_estimate() {
        let mut auto = AutoBandwidth::new(1_000_000);
        auto.observe(sample(8_000_000, 10_000, false), 25_000_000);
        let mut limited = sample(400_000, 400_000, false);
        limited.unlimited = 0;
        auto.observe(limited, 25_000_000);
        assert_eq!(auto.target, 1_000_000);
    }
    #[test]
    fn sustained_capacity_drop_converges_then_reprobes() {
        let mut auto = AutoBandwidth::new(10_000_000);
        auto.observe(sample(1_000_000, 5_000_000, true), 25_000_000);
        assert_eq!(auto.target, 10_000_000, "one RTT excursion is tolerated");
        for _ in 0..20 {
            auto.observe(sample(1_000_000, auto.target / 2, true), 25_000_000);
        }
        assert!((2_000_000..=2_200_000).contains(&auto.target));
        let previous = auto.target;
        auto.observe(sample(previous / 2, previous / 2, false), 25_000_000);
        assert!(auto.target > previous);
    }

    #[test]
    fn congestion_limited_sender_still_reduces_its_stale_target() {
        let mut auto = AutoBandwidth::new(10_000_000);
        for _ in 0..6 {
            auto.observe(sample(1_000_000, 1_100_000, true), 25_000_000);
        }
        assert!(auto.target <= 2_500_000);
    }
}
