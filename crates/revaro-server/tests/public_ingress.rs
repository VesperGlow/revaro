//! Exercise the real ACME client against a signed HTTPS ACME fixture, including
//! HTTP-01 validation, renewal, shared TCP/QUIC hot loading and offline restart.
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::{Buf, Bytes};
use http::{Request as HttpRequest, StatusCode};
use rcgen::{CertificateParams, CertificateSigningRequestParams, IsCa, KeyPair};
use revaro_server::{Config, ingress::PrivateCache, quic::NativeTransport};
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, ServerName},
};
use rustls_acme::{AccountCache, CertCache};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Authority {
    origin: String,
    http: SocketAddr,
    issuer: rcgen::Issuer<'static, KeyPair>,
    root: rcgen::Certificate,
    public_key: Mutex<Vec<u8>>,
    thumbprint: Mutex<String>,
    nonces: Mutex<std::collections::HashSet<String>>,
    account_requests: AtomicUsize,
    fail_order: AtomicBool,
    orders: AtomicUsize,
    validated: AtomicBool,
    challenges: AtomicUsize,
    pem: Mutex<String>,
    renewal: tokio::sync::Semaphore,
}
impl Authority {
    fn reply(&self, status: StatusCode, body: String, location: Option<&str>) -> Response {
        let nonce = uuid::Uuid::new_v4().to_string();
        self.nonces.lock().unwrap().insert(nonce.clone());
        let mut response = Response::builder()
            .status(status)
            .header("replay-nonce", nonce);
        if let Some(location) = location {
            response = response.header("location", format!("{}{location}", self.origin));
        }
        response.body(Body::from(body)).unwrap()
    }
    fn order(&self, status: &str) -> String {
        json!({"status":status,"authorizations":[format!("{}/auth", self.origin)],
            "finalize":format!("{}/finalize", self.origin),"certificate":format!("{}/cert", self.origin)}).to_string()
    }
}

async fn acme(State(ca): State<Arc<Authority>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    if path == "/directory" {
        return ca.reply(StatusCode::OK, json!({"newNonce":format!("{}/nonce", ca.origin),
            "newAccount":format!("{}/account", ca.origin),"newOrder":format!("{}/new-order", ca.origin)}).to_string(), None);
    }
    if path == "/nonce" {
        return ca.reply(StatusCode::OK, String::new(), None);
    }
    let jws: Value =
        serde_json::from_slice(&to_bytes(request.into_body(), 65536).await.unwrap()).unwrap();
    let protected: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(jws["protected"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(protected["url"], format!("{}{path}", ca.origin));
    assert_eq!(protected["alg"], "ES256");
    assert!(
        ca.nonces
            .lock()
            .unwrap()
            .remove(protected["nonce"].as_str().unwrap())
    );
    let public_key = if path == "/account" {
        let jwk = &protected["jwk"];
        let mut key = vec![4];
        key.extend(URL_SAFE_NO_PAD.decode(jwk["x"].as_str().unwrap()).unwrap());
        key.extend(URL_SAFE_NO_PAD.decode(jwk["y"].as_str().unwrap()).unwrap());
        *ca.public_key.lock().unwrap() = key.clone();
        let canonical =
            json!({"crv":jwk["crv"],"kty":jwk["kty"],"x":jwk["x"],"y":jwk["y"]}).to_string();
        *ca.thumbprint.lock().unwrap() = URL_SAFE_NO_PAD.encode(Sha256::digest(canonical));
        ca.account_requests.fetch_add(1, Ordering::SeqCst);
        key
    } else {
        assert_eq!(protected["kid"], format!("{}/account/1", ca.origin));
        ca.public_key.lock().unwrap().clone()
    };
    let message = format!(
        "{}.{}",
        jws["protected"].as_str().unwrap(),
        jws["payload"].as_str().unwrap()
    );
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, public_key)
        .verify(
            message.as_bytes(),
            &URL_SAFE_NO_PAD
                .decode(jws["signature"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
    let payload = URL_SAFE_NO_PAD
        .decode(jws["payload"].as_str().unwrap())
        .unwrap();
    match path.as_str() {
        "/account" => ca.reply(StatusCode::CREATED, "{}".into(), Some("/account/1")),
        "/new-order" => {
            if ca.fail_order.swap(false, Ordering::SeqCst) { return ca.reply(StatusCode::SERVICE_UNAVAILABLE, "temporary failure".into(), None); }
            let payload: Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(payload["identifiers"][0]["value"], "files.example.test");
            ca.orders.fetch_add(1, Ordering::SeqCst);
            ca.validated.store(false, Ordering::SeqCst);
            ca.reply(StatusCode::CREATED, ca.order("pending"), Some("/order"))
        }
        "/auth" => ca.reply(StatusCode::OK, json!({"status":if ca.validated.load(Ordering::SeqCst) {"valid"} else {"pending"},
            "identifier":{"type":"dns","value":"files.example.test"},
            "challenges":[{"type":"http-01","url":format!("{}/challenge", ca.origin),"token":"fixture-token"}]}).to_string(), None),
        "/challenge" => {
            let response = http_get(ca.http, "/.well-known/acme-challenge/fixture-token").await;
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            let key_auth = format!("fixture-token.{}", ca.thumbprint.lock().unwrap());
            assert_eq!(response.split_once("\r\n\r\n").unwrap().1, key_auth);
            ca.challenges.fetch_add(1, Ordering::SeqCst);
            ca.validated.store(true, Ordering::SeqCst);
            ca.reply(StatusCode::OK, "{}".into(), None)
        }
        "/order" => ca.reply(StatusCode::OK, ca.order("ready"), None),
        "/finalize" => {
            assert!(ca.validated.load(Ordering::SeqCst));
            let generation = ca.orders.load(Ordering::SeqCst);
            if generation >= 2 { ca.renewal.acquire().await.unwrap().forget(); }
            let payload: Value = serde_json::from_slice(&payload).unwrap();
            let der = URL_SAFE_NO_PAD.decode(payload["csr"].as_str().unwrap()).unwrap();
            let mut csr = CertificateSigningRequestParams::from_der(&der.into()).unwrap();
            let now = time::OffsetDateTime::now_utc();
            csr.params.not_before = now - time::Duration::seconds(120);
            // The first cert is still valid but already due for renewal.
            csr.params.not_after = now + if generation == 1 { time::Duration::seconds(30) } else { time::Duration::days(90) };
            let certificate = csr.signed_by(&ca.issuer).unwrap();
            *ca.pem.lock().unwrap() = format!("{}{}", certificate.pem(), ca.root.pem());
            ca.reply(StatusCode::OK, ca.order("valid"), None)
        }
        "/cert" => ca.reply(StatusCode::OK, ca.pem.lock().unwrap().clone(), None),
        _ => ca.reply(StatusCode::NOT_FOUND, String::new(), None),
    }
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: files.example.test\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

fn client_config(root: &CertificateDer<'static>, alpn: &[u8]) -> rustls::ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.add(root.clone()).unwrap();
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    crypto.alpn_protocols = vec![alpn.to_vec()];
    // Each observation must do a new certificate-authenticated handshake.
    crypto.resumption = rustls::client::Resumption::disabled();
    crypto
}

async fn tls(
    addr: SocketAddr,
    root: &CertificateDer<'static>,
    alpn: &[u8],
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, std::io::Error> {
    tokio_rustls::TlsConnector::from(Arc::new(client_config(root, alpn)))
        .connect(
            ServerName::try_from("files.example.test").unwrap(),
            tokio::net::TcpStream::connect(addr).await?,
        )
        .await
}

async fn h1(
    addr: SocketAddr,
    root: &CertificateDer<'static>,
) -> Result<(Vec<u8>, String), std::io::Error> {
    let mut stream = tls(addr, root, b"http/1.1").await?;
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    let leaf = stream.get_ref().1.peer_certificates().unwrap()[0].to_vec();
    stream
        .write_all(b"GET /readyz HTTP/1.1\r\nHost: files.example.test\r\nConnection: close\r\n\r\n")
        .await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("alt-svc: h3=\":443\"; ma=300")
    );
    Ok((leaf, response))
}

async fn h2(addr: SocketAddr, root: &CertificateDer<'static>) -> Vec<u8> {
    let stream = tls(addr, root, b"h2").await.unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let leaf = stream.get_ref().1.peer_certificates().unwrap()[0].to_vec();
    let (mut send, connection) = h2::client::handshake(stream).await.unwrap();
    let driver = tokio::spawn(connection);
    let (response, _) = send
        .send_request(
            HttpRequest::builder()
                .uri("https://files.example.test/readyz")
                .body(())
                .unwrap(),
            true,
        )
        .unwrap();
    assert_eq!(response.await.unwrap().status(), StatusCode::OK);
    driver.abort();
    leaf
}

fn quic_leaf(connection: &quinn::Connection) -> Vec<u8> {
    connection
        .peer_identity()
        .unwrap()
        .downcast::<Vec<CertificateDer<'static>>>()
        .unwrap()[0]
        .to_vec()
}

async fn h3_get(requests: &mut h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>) {
    let mut stream = requests
        .send_request(
            HttpRequest::builder()
                .uri("https://files.example.test/readyz")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    stream.finish().await.unwrap();
    assert_eq!(
        stream.recv_response().await.unwrap().status(),
        StatusCode::OK
    );
    let mut body = Vec::new();
    while let Some(mut bytes) = stream.recv_data().await.unwrap() {
        while bytes.has_remaining() {
            let chunk = bytes.chunk();
            body.extend_from_slice(chunk);
            let count = chunk.len();
            bytes.advance(count);
        }
    }
    assert!(!body.is_empty());
}

#[tokio::test]
async fn acme_http01_renews_tcp_and_quic_and_restarts_without_a_ca() {
    tokio::time::timeout(Duration::from_secs(30), lifecycle())
        .await
        .expect("ACME lifecycle timeout");
}

async fn lifecycle() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("revaro-ingress-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let public_ports = std::env::var_os("REVARO_INGRESS_TEST_PUBLIC_PORTS").is_some();
    let http_listener = tokio::net::TcpListener::bind(if public_ports {
        "127.0.0.1:80"
    } else {
        "127.0.0.1:0"
    })
    .await
    .unwrap();
    let http_addr = http_listener.local_addr().unwrap();
    let tls_listener = std::net::TcpListener::bind(if public_ports {
        "127.0.0.1:443"
    } else {
        "127.0.0.1:0"
    })
    .unwrap();
    let tls_addr = tls_listener.local_addr().unwrap();
    let ca_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ca_addr = ca_listener.local_addr().unwrap();
    ca_listener.set_nonblocking(true).unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let root = params.self_signed(&ca_key).unwrap();
    let root_der = root.der().clone();
    let issuer = rcgen::Issuer::new(params, ca_key);
    let https_key = KeyPair::generate().unwrap();
    let https_cert = CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .signed_by(&https_key, &issuer)
        .unwrap();
    let ca = Arc::new(Authority {
        origin: format!("https://{ca_addr}"),
        http: http_addr,
        issuer,
        root,
        public_key: Mutex::new(Vec::new()),
        thumbprint: Mutex::new(String::new()),
        nonces: Mutex::new(Default::default()),
        account_requests: AtomicUsize::new(0),
        fail_order: AtomicBool::new(true),
        orders: AtomicUsize::new(0),
        validated: AtomicBool::new(false),
        challenges: AtomicUsize::new(0),
        pem: Mutex::new(String::new()),
        renewal: tokio::sync::Semaphore::new(0),
    });
    let ca_file = scratch.0.join("ca.pem");
    std::fs::write(&ca_file, ca.root.pem()).unwrap();
    let ca_tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        https_cert.pem().into_bytes(),
        https_key.serialize_pem().into_bytes(),
    )
    .await
    .unwrap();
    let ca_handle = axum_server::Handle::new();
    let ca_task = tokio::spawn(
        axum_server::from_tcp_rustls(ca_listener, ca_tls)
            .unwrap()
            .handle(ca_handle.clone())
            .serve(
                Router::new()
                    .fallback(acme)
                    .with_state(ca.clone())
                    .into_make_service(),
            ),
    );
    let config = Config::from_lookup(&|key| match key {
        "APP_DOMAIN" => Some("files.example.test".into()),
        "APP_BASE_URL" => Some(format!("https://files.example.test:{}", tls_addr.port())),
        "APP_ADDR" => Some(http_addr.to_string()),
        "APP_TLS_ADDR" | "APP_QUIC_ADDR" => Some(tls_addr.to_string()),
        "APP_QUIC_PUBLIC_PORT" => Some("443".into()),
        "APP_DATA_DIR" => Some(scratch.0.to_string_lossy().into()),
        "ACME_DIRECTORY_URL" => Some(format!("{}/directory", ca.origin)),
        "ACME_CA_FILE" => Some(ca_file.to_string_lossy().into()),
        _ => None,
    })
    .unwrap();
    let app = Router::new().route("/readyz", axum::routing::get(|| async { "ready" }));
    drop(tls_listener);
    let server = NativeTransport::start(&config, app.clone()).await.unwrap();
    assert!(
        tls(tls_addr, &root_der, b"h2").await.is_err(),
        "no temporary production certificate before issuance"
    );
    let redirect = server
        .http_router(app.clone())
        .oneshot(
            HttpRequest::builder()
                .uri("//evil.example/a%20b?q=x%2Fy")
                .header("host", "evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(redirect.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        redirect.headers()["location"],
        format!("{}//evil.example/a%20b?q=x%2Fy", config.base_url)
    );
    let unknown = server
        .http_router(app.clone())
        .oneshot(
            HttpRequest::builder()
                .uri("/.well-known/acme-challenge/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let cancel_http = tokio_util::sync::CancellationToken::new();
    let shutdown = cancel_http.clone();
    let http_app = server.http_router(app.clone());
    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let first = loop {
        if let Ok((leaf, _)) = h1(tls_addr, &root_der).await {
            break leaf;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert_eq!(h2(tls_addr, &root_der).await, first);
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_config(&root_der, b"h3")).unwrap(),
    )));
    let old_connection = endpoint
        .connect(tls_addr, "files.example.test")
        .unwrap()
        .await
        .unwrap();
    assert_eq!(quic_leaf(&old_connection), first);
    let (mut driver, mut requests) =
        h3::client::new(h3_quinn::Connection::new(old_connection.clone()))
            .await
            .unwrap();
    let old_driver = tokio::spawn(async move { driver.wait_idle().await });
    h3_get(&mut requests).await;
    ca.renewal.add_permits(1);
    let second = loop {
        let (leaf, _) = h1(tls_addr, &root_der).await.unwrap();
        if leaf != first {
            break leaf;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    assert_eq!(h2(tls_addr, &root_der).await, second);
    let new_connection = endpoint
        .connect(tls_addr, "files.example.test")
        .unwrap()
        .await
        .unwrap();
    assert_eq!(quic_leaf(&new_connection), second);
    // Existing HTTP/3 streams continue to work while the resolver changes.
    h3_get(&mut requests).await;
    assert_eq!(ca.orders.load(Ordering::SeqCst), 2);
    assert_eq!(ca.challenges.load(Ordering::SeqCst), 2);
    assert!(ca.account_requests.load(Ordering::SeqCst) >= 2); // Includes the retried order's account lookup.
    let cache = PrivateCache::open(config.acme.as_ref().unwrap().cache_dir.clone())
        .await
        .unwrap();
    let settings = config.acme.as_ref().unwrap();
    let latest_chain = ca.pem.lock().unwrap().as_bytes().to_vec();
    while cache
        .load_cert(std::slice::from_ref(&settings.domain), &settings.directory)
        .await
        .unwrap()
        .is_none_or(|pem| !pem.windows(latest_chain.len()).any(|v| v == latest_chain))
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        cache
            .load_account(&[], &settings.directory)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .load_cert(&["other.example.test".into()], &settings.directory)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .load_cert(
                std::slice::from_ref(&settings.domain),
                "https://another-ca.test/directory"
            )
            .await
            .unwrap()
            .is_none()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&settings.cache_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for file in std::fs::read_dir(&settings.cache_dir).unwrap() {
            assert_eq!(
                file.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    old_connection.close(0u32.into(), b"restart");
    new_connection.close(0u32.into(), b"restart");
    old_driver.abort();
    drop(requests);
    server.finish().await;
    cancel_http.cancel();
    http_task.await.unwrap();
    ca_handle.shutdown();
    ca_task.await.unwrap().unwrap();
    let restarted = NativeTransport::start(&config, app).await.unwrap();
    loop {
        if let Ok((leaf, _)) = h1(tls_addr, &root_der).await {
            assert_eq!(leaf, second);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h2(tls_addr, &root_der).await, second);
    let connection = endpoint
        .connect(tls_addr, "files.example.test")
        .unwrap()
        .await
        .unwrap();
    assert_eq!(quic_leaf(&connection), second);
    connection.close(0u32.into(), b"done");
    restarted.finish().await;
    endpoint.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
}
