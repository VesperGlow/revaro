//! Adaptive or fixed target goodput with bounded loss compensation. This changes
//! only the sender's congestion window; Quinn owns standard QUIC recovery.

use super::QuicConfig;
use super::bandwidth::{AutoBandwidth, DeliverySample};
use quinn::congestion::{Controller, ControllerFactory, ControllerMetrics, CubicConfig};
use quinn_proto::RttEstimator;
use std::{
    any::Any,
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const SAMPLE_PERIOD: Duration = Duration::from_millis(500);
const HISTORY: Duration = Duration::from_secs(5);
const SEVERE_LOSS: f64 = 0.35;
const RECOVERY_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_FALLBACK_EPISODES: u8 = 3;

#[derive(Debug)]
pub(super) struct SenderState {
    pub rate: AtomicU64,
    pub target: AtomicU64,
    pub loss_ppm: AtomicU64,
    pub fallback: AtomicBool,
    pub sent_bytes: AtomicU64,
    pub acked_bytes: AtomicU64,
    pub lost_bytes: AtomicU64,
}

impl SenderState {
    pub fn new(rate: u64) -> Self {
        Self {
            rate: AtomicU64::new(rate),
            target: AtomicU64::new(rate),
            loss_ppm: AtomicU64::new(0),
            fallback: AtomicBool::new(false),
            sent_bytes: AtomicU64::new(0),
            acked_bytes: AtomicU64::new(0),
            lost_bytes: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct AggressiveFactory {
    pub config: QuicConfig,
    pub sender: Arc<SenderState>,
}

impl ControllerFactory for AggressiveFactory {
    fn build(self: Arc<Self>, now: Instant, mtu: u16) -> Box<dyn Controller> {
        let auto = AutoBandwidth::new(self.config.initial_rate());
        Box::new(Aggressive {
            factory: self,
            standard: Arc::new(CubicConfig::default()).build(now, mtu),
            mtu,
            started: now,
            sampled: now,
            rtt: Duration::from_millis(100),
            min_rtt: Duration::from_secs(2),
            auto,
            sampled_sent: 0,
            pending_unlimited: 0,
            observations: VecDeque::new(),
            pending_ack: 0,
            pending_loss: 0,
            severe_windows: 0,
            fallback: false,
            fallback_since: None,
            fallback_episodes: 0,
        })
    }
}

struct Observation {
    time: Instant,
    ack: u64,
    loss: u64,
}

struct Aggressive {
    factory: Arc<AggressiveFactory>,
    standard: Box<dyn Controller>,
    mtu: u16,
    started: Instant,
    sampled: Instant,
    rtt: Duration,
    min_rtt: Duration,
    auto: AutoBandwidth,
    sampled_sent: u64,
    pending_unlimited: u64,
    observations: VecDeque<Observation>,
    pending_ack: u64,
    pending_loss: u64,
    severe_windows: u8,
    fallback: bool,
    fallback_since: Option<Instant>,
    fallback_episodes: u8,
}

impl Aggressive {
    fn record_rtt(&mut self, smoothed: Duration, minimum: Duration) {
        self.rtt = smoothed.clamp(Duration::from_millis(10), Duration::from_secs(2));
        // Quinn replaces its provisional initial RTT on the first measurement.
        // Keeping our own lifetime minimum would freeze that 100 ms guess and
        // mistake a healthy high-latency path for a permanently queued one.
        self.min_rtt = minimum.max(Duration::from_millis(10));
    }
    fn target(&self) -> u64 {
        self.factory
            .config
            .target_bytes()
            .unwrap_or(self.auto.target)
    }

    fn enter_fallback(&mut self, now: Instant, persistent: bool, reason: &'static str) {
        if self.fallback {
            return;
        }
        // Construct Cubic at this event, rather than training a hidden Cubic
        // window while aggressive sending ignored its admission decisions.
        self.standard = Arc::new(CubicConfig::default()).build(now, self.mtu);
        self.standard.on_congestion_event(now, now, persistent, 0);
        self.fallback = true;
        self.fallback_since = Some(now);
        self.fallback_episodes = self.fallback_episodes.saturating_add(1);
        self.observations.clear();
        self.pending_ack = 0;
        self.pending_loss = 0;
        self.sampled = now;
        self.factory.sender.fallback.store(true, Ordering::Relaxed);
        self.factory.sender.rate.store(
            self.target().min(self.factory.config.max_bytes()),
            Ordering::Relaxed,
        );
        tracing::info!(reason, "QUIC aggressive controller switched to Cubic");
    }

    fn update(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.sampled);
        // At high RTT a sample must cover multiple ACK rounds.
        if elapsed < SAMPLE_PERIOD.max(self.rtt * 2).min(Duration::from_secs(1)) {
            return;
        }
        let sent = self.factory.sender.sent_bytes.load(Ordering::Relaxed);
        let delivery = DeliverySample {
            elapsed,
            acked: self.pending_ack,
            sent: sent.saturating_sub(self.sampled_sent),
            unlimited: self.pending_unlimited,
            rtt: self.rtt,
            min_rtt: self.min_rtt,
        };
        self.sampled_sent = sent;
        self.pending_unlimited = 0;
        self.observations.push_back(Observation {
            time: now,
            ack: self.pending_ack,
            loss: self.pending_loss,
        });
        self.pending_ack = 0;
        self.pending_loss = 0;
        self.sampled = now;
        while self
            .observations
            .front()
            .is_some_and(|o| now.saturating_duration_since(o.time) >= HISTORY)
        {
            self.observations.pop_front();
        }
        let (ack, lost) = self.observations.iter().fold((0_u64, 0_u64), |(a, l), o| {
            (a.saturating_add(o.ack), l.saturating_add(o.loss))
        });
        let count = ack.saturating_add(lost);
        let loss = if count >= u64::from(self.mtu) * 50 {
            lost as f64 / count as f64
        } else {
            0.0
        };
        self.factory
            .sender
            .loss_ppm
            .store((loss * 1_000_000.0) as u64, Ordering::Relaxed);
        if self.fallback {
            let cooldown =
                RECOVERY_COOLDOWN * 2_u32.pow(u32::from(self.fallback_episodes.saturating_sub(1)));
            if self.fallback_episodes < MAX_FALLBACK_EPISODES
                && self
                    .fallback_since
                    .is_some_and(|start| now.saturating_duration_since(start) >= cooldown)
                && ack >= u64::from(self.mtu) * 50
                && loss < 0.25
            {
                // An acknowledged healthy path may probe the configured rate
                // again. Only two probes per connection, with longer cooldown
                // after repeated failure; a genuinely overloaded path stays
                // Cubic rather than repeatedly blasting at its target.
                self.fallback = false;
                self.fallback_since = None;
                self.started = now;
                self.severe_windows = 0;
                self.observations.clear();
                self.auto = AutoBandwidth::new(self.factory.config.initial_rate());
                self.factory.sender.fallback.store(false, Ordering::Relaxed);
                self.factory
                    .sender
                    .rate
                    .store(self.factory.config.initial_rate(), Ordering::Relaxed);
                tracing::info!(
                    episodes = self.fallback_episodes,
                    "QUIC healthy path restarting bounded bandwidth probe"
                );
            }
            return;
        }
        self.severe_windows = if loss >= SEVERE_LOSS {
            self.severe_windows.saturating_add(1)
        } else {
            0
        };
        if self.severe_windows >= 2 || loss >= 0.60 {
            self.enter_fallback(now, false, "high measured loss");
            return;
        }
        let compensation = (1.0 / (1.0 - loss).max(0.5))
            .min(f64::from(self.factory.config.max_compensation_percent) / 100.0);
        tracing::debug!(
            elapsed_s = elapsed.as_secs_f64(),
            acked = delivery.acked,
            sent = delivery.sent,
            unlimited = delivery.unlimited,
            rtt_ms = self.rtt.as_millis(),
            min_rtt_ms = self.min_rtt.as_millis(),
            target = self.target(),
            loss,
            "QUIC bandwidth sample"
        );
        if self.factory.config.target_mbps.is_none() {
            self.auto.observe(delivery, self.factory.config.max_bytes());
        }
        let target = self.target();
        self.factory.sender.target.store(target, Ordering::Relaxed);
        // Ramp with acknowledged progress over two seconds, avoiding an
        // immediate full-bandwidth burst on a newly validated path.
        let ramp = if self.factory.config.target_mbps.is_some() {
            (0.25 + now.saturating_duration_since(self.started).as_secs_f64() * 0.375).min(1.0)
        } else {
            1.0
        };
        let rate =
            ((target as f64 * compensation * ramp) as u64).min(self.factory.config.max_bytes());
        self.factory
            .sender
            .rate
            .store(rate.max(1), Ordering::Relaxed);
    }
}

impl Controller for Aggressive {
    fn on_sent(&mut self, now: Instant, bytes: u64, packet: u64) {
        if self.fallback {
            self.standard.on_sent(now, bytes, packet);
        }
    }
    fn on_ack(
        &mut self,
        now: Instant,
        sent: Instant,
        bytes: u64,
        limited: bool,
        rtt: &RttEstimator,
    ) {
        self.factory
            .sender
            .acked_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        self.record_rtt(rtt.get(), rtt.min());
        self.pending_ack = self.pending_ack.saturating_add(bytes);
        if !limited {
            self.pending_unlimited = self.pending_unlimited.saturating_add(bytes);
        }
        if self.fallback {
            self.standard.on_ack(now, sent, bytes, limited, rtt);
        }
    }
    fn on_end_acks(&mut self, now: Instant, in_flight: u64, limited: bool, largest: Option<u64>) {
        if self.fallback {
            self.standard.on_end_acks(now, in_flight, limited, largest);
        }
        self.update(now);
    }
    fn on_congestion_event(&mut self, now: Instant, sent: Instant, persistent: bool, lost: u64) {
        self.factory
            .sender
            .lost_bytes
            .fetch_add(lost, Ordering::Relaxed);
        self.pending_loss = self.pending_loss.saturating_add(lost);
        if self.fallback {
            if persistent || lost == 0 {
                self.fallback_since = Some(now);
            }
            if persistent {
                // A new persistent episode must collapse even when Cubic's
                // recovery-start guard would ignore an older lost packet.
                self.standard = Arc::new(CubicConfig::default()).build(now, self.mtu);
                self.standard.on_congestion_event(now, now, true, lost);
            } else {
                self.standard.on_congestion_event(now, sent, false, lost);
            }
            return;
        }
        if persistent || lost == 0 {
            // ECN is an explicit congestion signal, not random radio loss.
            self.enter_fallback(
                now,
                persistent,
                if persistent {
                    "persistent congestion"
                } else {
                    "ECN congestion"
                },
            );
        } else {
            self.update(now);
        }
    }
    fn on_mtu_update(&mut self, mtu: u16) {
        self.mtu = mtu;
        self.standard.on_mtu_update(mtu);
    }
    fn window(&self) -> u64 {
        if self.fallback {
            return self.standard.window();
        }
        let rate = self.factory.sender.rate.load(Ordering::Relaxed);
        // Two BDPs tolerate ACK gaps/RTT variation; the independent UDP pacer
        // caps transmission rate regardless of this window or retransmissions.
        ((rate as f64 * self.rtt.as_secs_f64() * 2.0) as u64).clamp(
            u64::from(self.mtu) * 10,
            u64::from(self.factory.config.max_window_mib) << 20,
        )
    }
    fn initial_window(&self) -> u64 {
        u64::from(self.mtu) * 10
    }
    fn metrics(&self) -> ControllerMetrics {
        let mut result = self.standard.metrics();
        result.congestion_window = self.window();
        result.pacing_rate = Some(
            self.factory
                .sender
                .rate
                .load(Ordering::Relaxed)
                .saturating_mul(8),
        );
        result
    }
    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(Self {
            factory: self.factory.clone(),
            standard: self.standard.clone_box(),
            mtu: self.mtu,
            started: self.started,
            sampled: self.sampled,
            rtt: self.rtt,
            min_rtt: self.min_rtt,
            auto: self.auto.clone(),
            sampled_sent: self.sampled_sent,
            pending_unlimited: self.pending_unlimited,
            observations: self
                .observations
                .iter()
                .map(|o| Observation {
                    time: o.time,
                    ack: o.ack,
                    loss: o.loss,
                })
                .collect(),
            pending_ack: self.pending_ack,
            pending_loss: self.pending_loss,
            severe_windows: self.severe_windows,
            fallback: self.fallback,
            fallback_since: self.fallback_since,
            fallback_episodes: self.fallback_episodes,
        })
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_factory() -> Arc<AggressiveFactory> {
        Arc::new(AggressiveFactory {
            config: QuicConfig {
                addr: "127.0.0.1:4433".parse().unwrap(),
                mode: super::super::CongestionMode::Aggressive,
                target_mbps: Some(10),
                max_mbps: 20,
                global_max_mbps: 40,
                max_compensation_percent: 125,
                max_window_mib: 8,
                max_connections: 8,
            },
            sender: Arc::new(SenderState::new(1_250_000)),
        })
    }
    #[test]
    fn random_loss_compensates_without_window_collapse_and_stays_bounded() {
        for loss_percent in [0_u64, 5, 10, 15, 25] {
            let factory = make_factory();
            let now = Instant::now();
            let mut controller = factory
                .clone()
                .build(now, 1200)
                .into_any()
                .downcast::<Aggressive>()
                .unwrap();
            controller.rtt = Duration::from_millis(150);
            let initial = controller.window();
            for n in 1..=8 {
                let time = now + Duration::from_millis(n * 500);
                controller.pending_ack += (100 - loss_percent) * 12000;
                if loss_percent > 0 {
                    controller.on_congestion_event(time, now, false, loss_percent * 12000);
                }
                controller.on_end_acks(time, 0, false, None);
            }
            let rate = factory.sender.rate.load(Ordering::Relaxed);
            assert!(rate >= factory.config.target_bytes().unwrap());
            assert!(rate <= factory.config.target_bytes().unwrap() * 125 / 100);
            assert!(controller.window() >= initial);
            assert!(!factory.sender.fallback.load(Ordering::Relaxed));
        }
    }
    #[test]
    fn persistent_congestion_and_extreme_loss_switch_to_standard() {
        let factory = make_factory();
        let now = Instant::now();
        let mut controller = factory.clone().build(now, 1200);
        controller.on_congestion_event(now, now, true, 12000);
        assert!(factory.sender.fallback.load(Ordering::Relaxed));
        assert_eq!(controller.window(), 2400);
        let factory = make_factory();
        let mut controller = factory.clone().build(now, 1200);
        controller.on_congestion_event(now + Duration::from_secs(1), now, false, 120_000);
        assert!(factory.sender.fallback.load(Ordering::Relaxed));
    }

    #[test]
    fn healthy_acknowledged_paths_recover_with_finite_increasing_cooldowns() {
        let factory = make_factory();
        let start = Instant::now();
        let mut controller = factory
            .clone()
            .build(start, 1200)
            .into_any()
            .downcast::<Aggressive>()
            .unwrap();
        let mut now = start;
        for episode in 1..=3 {
            controller.on_congestion_event(now, now, true, 12000);
            assert_eq!(controller.window(), 2400);
            let cooldown = RECOVERY_COOLDOWN * 2_u32.pow(episode - 1);
            controller.pending_ack = 100_000;
            controller.update(now + cooldown - Duration::from_millis(500));
            assert!(controller.fallback);
            controller.pending_ack = 100_000;
            now += cooldown;
            controller.update(now);
            assert_eq!(controller.fallback, episode == 3);
            if episode < 3 {
                assert_eq!(
                    factory.sender.rate.load(Ordering::Relaxed),
                    factory.config.initial_rate()
                );
                now += Duration::from_secs(3);
            }
        }
        controller.pending_ack = 1_000_000;
        controller.update(now + Duration::from_secs(120));
        assert!(
            controller.fallback,
            "repeated persistent congestion stays Cubic"
        );
    }

    #[test]
    fn auto_uses_measured_rtt_and_wire_delivery_instead_of_provisional_rtt() {
        let mut factory = make_factory();
        let config = &mut Arc::get_mut(&mut factory).unwrap().config;
        config.target_mbps = None;
        config.max_mbps = 250;
        let now = Instant::now();
        let mut controller = factory
            .clone()
            .build(now, 1200)
            .into_any()
            .downcast::<Aggressive>()
            .unwrap();
        controller.record_rtt(Duration::from_millis(100), Duration::from_millis(100));
        controller.record_rtt(Duration::from_millis(140), Duration::from_millis(130));
        assert_eq!(controller.min_rtt, Duration::from_millis(130));
        controller.update(now + Duration::from_secs(2));
        assert_eq!(
            controller.target(),
            500_000,
            "elapsed time alone cannot raise auto bandwidth"
        );
        for n in 1..=12 {
            let bytes = controller.target() / 2;
            factory
                .sender
                .sent_bytes
                .fetch_add(bytes, Ordering::Relaxed);
            controller.pending_ack = bytes * 85 / 100;
            controller.pending_unlimited = controller.pending_ack;
            controller.update(now + Duration::from_secs(2) + SAMPLE_PERIOD * n);
        }
        assert!(
            controller.target() > 6_250_000,
            "auto can exceed the old 50 Mbps cap"
        );
        assert!(!factory.sender.fallback.load(Ordering::Relaxed));
        assert!(factory.sender.rate.load(Ordering::Relaxed) <= factory.config.max_bytes());
    }
}
