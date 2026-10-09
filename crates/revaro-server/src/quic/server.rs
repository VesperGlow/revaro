use axum::{Router, body::Body, extract::ConnectInfo};
use bytes::{Buf, Bytes};
use futures_util::StreamExt;
use quinn::{
    Runtime,
    congestion::{ControllerFactory, CubicConfig},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tower::ServiceExt;

use super::{
    CongestionMode, QuicConfig, TlsIdentity,
    controller::AggressiveFactory,
    pacing::{MAX_DATAGRAM, PacedSocket, PacingRegistry},
};
use crate::Config;

/// Owns optional native TLS/QUIC listeners. Ordinary HTTP remains available.
pub struct NativeTransport {
    tasks: tokio::task::JoinSet<()>,
    control: ShutdownControl,
    http: Option<Router>,
}

/// Cloneable shutdown signal for main's existing graceful-shutdown future.
#[derive(Clone, Default)]
pub struct ShutdownControl {
    endpoint: Option<quinn::Endpoint>,
    tls: Vec<axum_server::Handle<SocketAddr>>,
    cancel: tokio_util::sync::CancellationToken,
}
impl ShutdownControl {
    pub fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(endpoint) = &self.endpoint {
            endpoint.close(0_u32.into(), b"server shutdown");
        }
        for handle in &self.tls {
            handle.graceful_shutdown(Some(Duration::from_secs(10)));
        }
    }
}

impl NativeTransport {
    /// Bind all optional listeners before startup reports success.
    pub async fn start(config: &Config, app: Router) -> io::Result<Self> {
        let mut result = Self {
            tasks: tokio::task::JoinSet::new(),
            control: ShutdownControl::default(),
            http: None,
        };
        let Some(tls) = &config.tls else {
            return Ok(result);
        };
        let builder = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth();
        let mut acme_state = None;
        let mut tcp_crypto = match &tls.identity {
            TlsIdentity::Pem { cert, key } => {
                let cert_bytes = tokio::fs::read(cert).await?;
                let key_bytes = tokio::fs::read(key).await?;
                let certs = CertificateDer::pem_slice_iter(&cert_bytes)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(io::Error::other)?;
                let key = PrivateKeyDer::from_pem_slice(&key_bytes).map_err(io::Error::other)?;
                builder
                    .with_single_cert(certs, key)
                    .map_err(io::Error::other)?
            }
            TlsIdentity::Acme => {
                let settings = config
                    .acme
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing ACME settings"))?;
                let state = settings.state().await?;
                result.http = Some(crate::ingress::http_router(&state, &config.base_url));
                let crypto = builder.with_cert_resolver(state.resolver());
                acme_state = Some(state);
                crypto
            }
        };
        tcp_crypto.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        // Default 0-RTT stays disabled: mutating uploads must not be replayed.
        let tls_config =
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tcp_crypto.clone()));
        if let Some(quic) = &config.quic {
            tcp_crypto.alpn_protocols = vec![b"h3".to_vec()];
            let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tcp_crypto)
                .map_err(io::Error::other)?;
            let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
            server_config.migration(false);
            server_config
                .transport_config(Arc::new(transport_config(Arc::new(CubicConfig::default()))));
            let socket = std::net::UdpSocket::bind(quic.addr).map_err(|error| {
                io::Error::new(error.kind(), format!("UDP {}: {error}", quic.addr))
            })?;
            socket.set_nonblocking(true)?;
            let registry = PacingRegistry::new(quic.clone());
            let socket = Arc::new(PacedSocket::new(
                quinn::TokioRuntime.wrap_udp_socket(socket)?,
                registry.clone(),
            ));
            let endpoint = quinn::Endpoint::new_with_abstract_socket(
                quinn::EndpointConfig::default(),
                Some(server_config.clone()),
                socket,
                Arc::new(quinn::TokioRuntime),
            )?;
            result.control.endpoint = Some(endpoint.clone());
            let h3_app = app.clone();
            let settings = quic.clone();
            result.tasks.spawn(async move {
                accept_connections(endpoint, server_config, settings, registry, h3_app).await;
            });
            tracing::info!(addr=%quic.addr, mode=?quic.mode, target_mbps=?quic.target_mbps, max_mbps=quic.max_mbps,
                global_max_mbps=quic.global_max_mbps, "native HTTP/3 listening");
        }
        let advertised = config
            .quic_public_port
            .map(|port| format!("h3=\":{port}\"; ma=300"));
        let primary = app.clone().layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let advertised = advertised.clone();
                async move {
                    let mut response = next.run(request).await;
                    if let Some(value) = advertised {
                        response.headers_mut().insert(
                            http::header::ALT_SVC,
                            value.parse().expect("validated QUIC port"),
                        );
                    }
                    response
                }
            },
        ));
        result.add_tls(tls.addr, tls_config.clone(), primary)?;
        if let Some(addr) = tls.http2_addr {
            result.add_tls(addr, tls_config, app)?;
        }
        if let Some(mut state) = acme_state {
            let cancel = result.control.cancel.clone();
            result.tasks.spawn(async move {
                loop {
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        event = state.next() => match event {
                            Some(Ok(event)) => tracing::info!(?event, "ACME certificate lifecycle"),
                            Some(Err(error)) => tracing::warn!(%error, "ACME operation failed; automatic retry remains active"),
                            None => break,
                        }
                    }
                }
            });
            tracing::info!(domain=%config.acme.as_ref().expect("ACME settings").domain,
                "public HTTP-01 ingress enabled; certificates reload on new TLS/QUIC handshakes");
        }
        Ok(result)
    }

    fn add_tls(
        &mut self,
        addr: SocketAddr,
        config: axum_server::tls_rustls::RustlsConfig,
        app: Router,
    ) -> io::Result<()> {
        let socket = socket2::Socket::new(
            socket2::Domain::for_address(addr),
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        #[cfg(unix)]
        socket.set_reuse_address(true)?;
        socket
            .bind(&addr.into())
            .map_err(|error| io::Error::new(error.kind(), format!("TCP {addr}: {error}")))?;
        socket.listen(1024)?;
        let listener: std::net::TcpListener = socket.into();
        listener.set_nonblocking(true)?;
        let handle = axum_server::Handle::new();
        self.control.tls.push(handle.clone());
        self.tasks.spawn(async move {
            if let Err(error) = axum_server::from_tcp_rustls(listener, config)
                .expect("bound TLS listener")
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await
            {
                tracing::error!(%error,%addr,"native HTTPS listener failed");
            }
        });
        tracing::info!(%addr,"native HTTP/2 HTTPS listening");
        Ok(())
    }
    pub fn shutdown_control(&self) -> ShutdownControl {
        self.control.clone()
    }
    pub fn http_router(&self, app: Router) -> Router {
        self.http.clone().unwrap_or(app)
    }
    pub async fn finish(mut self) {
        self.control.shutdown();
        while self.tasks.join_next().await.is_some() {}
        if let Some(endpoint) = self.control.endpoint.take() {
            endpoint.wait_idle().await;
        }
    }
}
impl Drop for NativeTransport {
    fn drop(&mut self) {
        self.control.shutdown();
    }
}

fn transport_config(
    controller: Arc<dyn ControllerFactory + Send + Sync>,
) -> quinn::TransportConfig {
    let mut transport = quinn::TransportConfig::default();
    transport
        .congestion_controller_factory(controller)
        .initial_rtt(Duration::from_millis(100))
        .keep_alive_interval(Some(Duration::from_secs(5)))
        .max_idle_timeout(Some(
            Duration::from_secs(30)
                .try_into()
                .expect("valid idle timeout"),
        ))
        .max_concurrent_bidi_streams(32_u32.into())
        .max_concurrent_uni_streams(16_u32.into())
        .stream_receive_window((4_u32 << 20).into())
        .receive_window((8_u32 << 20).into())
        .send_window(4 << 20)
        .enable_segmentation_offload(false);
    let mut mtu = quinn::MtuDiscoveryConfig::default();
    mtu.upper_bound(MAX_DATAGRAM);
    transport.mtu_discovery_config(Some(mtu));
    // Bounded send buffering is independent of the controller's BDP window.
    transport
}

async fn accept_connections(
    endpoint: quinn::Endpoint,
    base: quinn::ServerConfig,
    config: QuicConfig,
    registry: Arc<PacingRegistry>,
    app: Router,
) {
    let permits = Arc::new(tokio::sync::Semaphore::new(config.max_connections as usize));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            incoming=endpoint.accept() => {
                let Some(incoming)=incoming else { break; };
                let Ok(permit)=permits.clone().try_acquire_owned() else { incoming.refuse(); continue; };
                let peer=registry.register(incoming.remote_address());
                let mut settings=base.clone();
                let controller:Arc<dyn ControllerFactory+Send+Sync>=if config.mode==CongestionMode::Aggressive {
                    Arc::new(AggressiveFactory { config:config.clone(), sender:peer.sender.clone() })
                } else { Arc::new(CubicConfig::default()) };
                settings.transport_config(Arc::new(transport_config(controller)));
                let app=app.clone();
                connections.spawn(async move {
                    let _permit=permit;
                    match incoming.accept_with(Arc::new(settings)) {
                        Ok(connecting) => match tokio::time::timeout(Duration::from_secs(10),connecting).await {
                            Ok(Ok(connection)) => {
                                let remote=connection.remote_address();
                                if let Err(error)=serve_connection(connection.clone(),app).await { tracing::debug!(%error,"HTTP/3 connection ended"); }
                                let stats=connection.stats();
                                tracing::info!(%remote, sent_bytes=peer.sender.sent_bytes.load(Ordering::Relaxed),
                                    acked_bytes=peer.sender.acked_bytes.load(Ordering::Relaxed), lost_bytes=peer.sender.lost_bytes.load(Ordering::Relaxed),
                                    loss_ppm=peer.sender.loss_ppm.load(Ordering::Relaxed), pacing_bps=peer.sender.rate.load(Ordering::Relaxed)*8,
                                    target_bps=peer.sender.target.load(Ordering::Relaxed)*8, cubic_fallback=peer.sender.fallback.load(Ordering::Relaxed), rtt_ms=stats.path.rtt.as_millis(),
                                    sent_packets=stats.path.sent_packets, lost_packets=stats.path.lost_packets,
                                    "QUIC connection transfer statistics");
                            }
                            _ => tracing::debug!("QUIC handshake failed or timed out"),
                        },
                        Err(error) => tracing::debug!(%error,"QUIC incoming connection rejected"),
                    }
                    drop(peer);
                });
            }
            Some(result)=connections.join_next(), if !connections.is_empty() => {
                if let Err(error)=result { tracing::warn!(%error,"HTTP/3 connection task failed"); }
            }
        }
    }
    while connections.join_next().await.is_some() {}
}

async fn serve_connection(
    connection: quinn::Connection,
    app: Router,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let remote = connection.remote_address();
    let mut h3 = h3::server::Connection::new(h3_quinn::Connection::new(connection)).await?;
    let mut requests = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            request=h3.accept() => {
                let Some(resolver)=request? else { break; };
                let app=app.clone();
                requests.spawn(async move {
                    if let Err(error)=serve_request(resolver,app,remote).await { tracing::debug!(%error,"HTTP/3 stream ended"); }
                });
            }
            Some(result)=requests.join_next(), if !requests.is_empty() => {
                if let Err(error)=result { tracing::warn!(%error,"HTTP/3 request task failed"); }
            }
        }
    }
    while requests.join_next().await.is_some() {}
    Ok(())
}

async fn serve_request(
    resolver: h3::server::RequestResolver<h3_quinn::Connection, Bytes>,
    app: Router,
    remote: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (request, stream) = resolver.resolve_request().await?;
    let (mut send, receive) = stream.split();
    let body = futures_util::stream::try_unfold(receive, |mut receive| async move {
        match receive.recv_data().await? {
            Some(mut data) => {
                let bytes = data.copy_to_bytes(data.remaining());
                Ok(Some((bytes, receive)))
            }
            None => Ok::<_, h3::error::StreamError>(None),
        }
    });
    let (mut parts, ()) = request.into_parts();
    parts.version = http::Version::HTTP_3;
    parts.extensions.insert(ConnectInfo(remote));
    let request = http::Request::from_parts(parts, Body::from_stream(body));
    let response = app
        .oneshot(request)
        .await
        .expect("Axum router is infallible");
    let (mut parts, body) = response.into_parts();
    parts.version = http::Version::HTTP_3;
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "transfer-encoding",
        "upgrade",
    ] {
        parts.headers.remove(name);
    }
    send.send_response(http::Response::from_parts(parts, ()))
        .await?;
    let data = body.into_data_stream();
    tokio::pin!(data);
    while let Some(chunk) = data.next().await {
        let mut chunk = chunk?;
        while !chunk.is_empty() {
            // Large buffered responses are measured by progress too.
            let bytes = chunk.split_to(chunk.len().min(16 * 1024));
            match tokio::time::timeout(Duration::from_secs(8), send.send_data(bytes)).await {
                Ok(result) => result?,
                Err(_) => {
                    // Reset only the stuck HTTP stream. The shared browser transport
                    // retries its Range/upload block; unrelated streams keep running.
                    send.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "HTTP/3 stream write stalled",
                    )
                    .into());
                }
            }
        }
    }
    send.finish().await?;
    Ok(())
}
