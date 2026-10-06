//! Datagram pacing covers retransmissions, control packets and every stream.
//! No application write can bypass the per-peer or endpoint-wide rate cap.

use super::{CongestionMode, QuicConfig, controller::SenderState};
use quinn::{
    AsyncUdpSocket, UdpPoller,
    udp::{RecvMeta, Transmit},
};
use std::{
    collections::HashMap,
    future::Future,
    io::{self, IoSliceMut},
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex, atomic::Ordering},
    task::{Context, Poll},
    time::{Duration, Instant},
};

pub(super) const MAX_DATAGRAM: u16 = 1452;
const MIN_BURST_BYTES: f64 = (MAX_DATAGRAM as u64 * 2 + 96) as f64;
const MAX_BURST_BYTES: f64 = 64.0 * 1024.0;

fn capacity(rate: u64) -> f64 {
    // Tokio timers have millisecond granularity. Permit at most four ms of
    // credits, capped at 64 KiB; never bank seconds of idle-time traffic.
    (rate as f64 * 0.004).clamp(MIN_BURST_BYTES, MAX_BURST_BYTES)
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
    rate: u64,
}
impl Bucket {
    fn new(rate: u64, now: Instant) -> Self {
        Self {
            tokens: MIN_BURST_BYTES,
            updated: now,
            rate,
        }
    }
    fn refill(&mut self, now: Instant, rate: u64) {
        self.tokens = (self.tokens
            + now.saturating_duration_since(self.updated).as_secs_f64() * self.rate as f64)
            .min(capacity(rate));
        self.updated = now;
        self.rate = rate.max(1);
    }
    fn wait(&self, bytes: usize) -> Duration {
        if self.tokens >= bytes as f64 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((bytes as f64 - self.tokens) / self.rate as f64)
        }
    }
}

#[derive(Debug)]
struct Peer {
    bucket: Bucket,
    sender: Arc<SenderState>,
    references: usize,
}
#[derive(Debug)]
struct Budgets {
    global: Bucket,
    unknown: Bucket,
    peers: HashMap<SocketAddr, Peer>,
    waiters: HashMap<tokio::task::Id, Instant>,
}

#[derive(Debug)]
pub(super) struct PacingRegistry {
    config: QuicConfig,
    budgets: Mutex<Budgets>,
}
impl PacingRegistry {
    pub fn new(config: QuicConfig) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            budgets: Mutex::new(Budgets {
                global: Bucket::new(u64::from(config.global_max_mbps) * 125_000, now),
                unknown: Bucket::new(125_000, now),
                peers: HashMap::new(),
                waiters: HashMap::new(),
            }),
            config,
        })
    }
    pub fn register(self: &Arc<Self>, address: SocketAddr) -> PeerGuard {
        let mut budgets = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let peer = budgets.peers.entry(address).or_insert_with(|| {
            let rate = if self.config.mode == CongestionMode::Aggressive {
                self.config.initial_rate()
            } else {
                self.config.max_bytes()
            };
            Peer {
                bucket: Bucket::new(rate, Instant::now()),
                sender: Arc::new(SenderState::new(rate)),
                references: 0,
            }
        });
        peer.references += 1;
        PeerGuard {
            registry: self.clone(),
            address,
            sender: peer.sender.clone(),
        }
    }
}

pub(super) struct PeerGuard {
    registry: Arc<PacingRegistry>,
    address: SocketAddr,
    pub sender: Arc<SenderState>,
}
impl Drop for PeerGuard {
    fn drop(&mut self) {
        let mut budgets = self
            .registry
            .budgets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(peer) = budgets.peers.get_mut(&self.address) {
            peer.references -= 1;
            if peer.references == 0 {
                budgets.peers.remove(&self.address);
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct PacedSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    registry: Arc<PacingRegistry>,
}
impl PacedSocket {
    pub fn new(inner: Arc<dyn AsyncUdpSocket>, registry: Arc<PacingRegistry>) -> Self {
        Self { inner, registry }
    }
}

impl AsyncUdpSocket for PacedSocket {
    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        // Disabling GSO limits every admission decision to one datagram.
        let bytes = transmit.contents.len() + 48; // conservatively include IPv6/UDP headers
        if bytes > MAX_DATAGRAM as usize + 48 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "QUIC datagram exceeds pacing burst limit",
            ));
        }
        let now = Instant::now();
        let mut budgets = self
            .registry
            .budgets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let global_rate = u64::from(self.registry.config.global_max_mbps) * 125_000;
        budgets.global.refill(now, global_rate);
        let global_wait = budgets.global.wait(bytes);
        let peer_wait = if let Some(peer) = budgets.peers.get_mut(&transmit.destination) {
            peer.bucket.refill(
                now,
                peer.sender
                    .rate
                    .load(Ordering::Relaxed)
                    .min(self.registry.config.max_bytes()),
            );
            peer.bucket.wait(bytes)
        } else {
            budgets.unknown.refill(now, 125_000);
            budgets.unknown.wait(bytes)
        };
        let wait = global_wait.max(peer_wait);
        if !wait.is_zero() {
            // Quinn creates an independent poller for each connection driver.
            // A slow peer must not overwrite another driver's wakeup timer.
            // Stateless endpoint responses have no poller and are retried by
            // QUIC itself; do not allocate arbitrary address-based waiters.
            if budgets.peers.contains_key(&transmit.destination)
                && let Some(task_id) = tokio::task::try_id()
            {
                budgets.waiters.insert(task_id, now + wait);
            }
            return Err(io::ErrorKind::WouldBlock.into());
        }
        // Failed kernel sends do not spend credits: the real I/O poller still
        // handles socket backpressure. No busy loop or pretend successful send.
        self.inner.try_send(transmit)?;
        budgets.global.tokens -= bytes as f64;
        if let Some(peer) = budgets.peers.get_mut(&transmit.destination) {
            peer.bucket.tokens -= bytes as f64;
            peer.sender
                .sent_bytes
                .fetch_add(bytes as u64, Ordering::Relaxed);
        } else {
            budgets.unknown.tokens -= bytes as f64;
        }
        if let Some(task_id) = tokio::task::try_id() {
            budgets.waiters.remove(&task_id);
        }
        Ok(())
    }
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(PacedPoller {
            inner: self.inner.clone().create_io_poller(),
            socket: self,
            timer: None,
            deadline: None,
            task_id: None,
        })
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
    fn max_transmit_segments(&self) -> usize {
        1
    }
    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }
    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

#[derive(Debug)]
struct PacedPoller {
    inner: Pin<Box<dyn UdpPoller>>,
    socket: Arc<PacedSocket>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    deadline: Option<Instant>,
    task_id: Option<tokio::task::Id>,
}
impl UdpPoller for PacedPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.task_id = tokio::task::try_id();
        let deadline = this.task_id.and_then(|task_id| {
            this.socket
                .registry
                .budgets
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .waiters
                .get(&task_id)
                .copied()
        });
        if let Some(deadline) = deadline.filter(|d| *d > Instant::now()) {
            if this.deadline != Some(deadline) {
                this.deadline = Some(deadline);
                this.timer = Some(Box::pin(tokio::time::sleep_until(
                    tokio::time::Instant::from_std(deadline),
                )));
            }
            if this
                .timer
                .as_mut()
                .expect("pacing timer")
                .as_mut()
                .poll(cx)
                .is_pending()
            {
                return Poll::Pending;
            }
        }
        this.timer = None;
        this.deadline = None;
        this.inner.as_mut().poll_writable(cx)
    }
}

impl Drop for PacedPoller {
    fn drop(&mut self) {
        if let Some(task_id) = self.task_id {
            self.socket
                .registry
                .budgets
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .waiters
                .remove(&task_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, Default)]
    struct TestSocket(std::sync::atomic::AtomicU64);
    #[derive(Debug)]
    struct ReadyPoller;
    impl UdpPoller for ReadyPoller {
        fn poll_writable(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    impl AsyncUdpSocket for TestSocket {
        fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
            self.0
                .fetch_add(transmit.contents.len() as u64 + 48, Ordering::Relaxed);
            Ok(())
        }
        fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
            Box::pin(ReadyPoller)
        }
        fn poll_recv(
            &self,
            _: &mut Context<'_>,
            _: &mut [IoSliceMut<'_>],
            _: &mut [RecvMeta],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok("127.0.0.1:4433".parse().unwrap())
        }
    }

    #[tokio::test]
    async fn multiple_connections_share_the_wire_cap_and_clean_up_driver_waiters() {
        let config = QuicConfig {
            addr: "127.0.0.1:4433".parse().unwrap(),
            mode: CongestionMode::Standard,
            target_mbps: Some(2),
            max_mbps: 2,
            global_max_mbps: 2,
            max_compensation_percent: 125,
            max_window_mib: 8,
            max_connections: 8,
        };
        let registry = PacingRegistry::new(config);
        let inner = Arc::new(TestSocket::default());
        let socket = Arc::new(PacedSocket::new(inner.clone(), registry.clone()));
        let mut tasks = tokio::task::JoinSet::new();
        let started = Instant::now();
        for port in 5000..5003 {
            let socket = socket.clone();
            let guard = registry.register(SocketAddr::from(([127, 0, 0, 1], port)));
            tasks.spawn(async move {
                let mut poller = socket.clone().create_io_poller();
                let data = [0_u8; 1200];
                let transmit = Transmit {
                    destination: guard.address,
                    ecn: None,
                    contents: &data,
                    segment_size: None,
                    src_ip: None,
                };
                for _ in 0..80 {
                    loop {
                        std::future::poll_fn(|cx| poller.as_mut().poll_writable(cx))
                            .await
                            .unwrap();
                        match socket.try_send(&transmit) {
                            Ok(()) => break,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Err(error) => panic!("{error}"),
                        }
                    }
                }
            });
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = tasks.join_next().await {
                result.unwrap();
            }
        })
        .await
        .expect("pacing must wake every driver");
        let bytes = inner.0.load(Ordering::Relaxed);
        assert_eq!(bytes, 240 * 1248);
        assert!(bytes as f64 <= 250_000.0 * started.elapsed().as_secs_f64() + capacity(250_000));
        let budgets = registry.budgets.lock().unwrap();
        assert!(budgets.peers.is_empty());
        assert!(budgets.waiters.is_empty());
    }
    #[test]
    fn pacing_bounds_elapsed_rate_and_burst_and_does_not_bank_idle_time() {
        let now = Instant::now();
        let mut bucket = Bucket::new(125_000, now);
        let mut admitted = 0;
        for ms in 0..1000 {
            bucket.refill(now + Duration::from_millis(ms), 125_000);
            while bucket.wait(1250).is_zero() {
                bucket.tokens -= 1250.0;
                admitted += 1250;
            }
        }
        assert!(admitted as f64 <= 125_000.0 + capacity(125_000));
        assert!(admitted >= 120_000);
        bucket.refill(now + Duration::from_secs(3600), 125_000);
        assert!(bucket.tokens <= capacity(125_000));
        bucket.tokens = 0.0;
        assert_eq!(bucket.wait(1250), Duration::from_millis(10));
    }

    #[test]
    fn high_rate_bursts_are_bounded_and_rate_reduction_removes_old_credits() {
        let now = Instant::now();
        let mut bucket = Bucket::new(125_000_000, now);
        bucket.refill(now + Duration::from_secs(1), 125_000_000);
        assert_eq!(bucket.tokens, MAX_BURST_BYTES);
        bucket.refill(now + Duration::from_secs(2), 125_000);
        assert_eq!(bucket.tokens, MIN_BURST_BYTES);
        assert_eq!(capacity(2_500_000), 10_000.0);
    }
}
