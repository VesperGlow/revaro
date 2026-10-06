//! Real TLS/QUIC sockets and standard HTTP/3 framing, through the product router.
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use bytes::{Buf, Bytes};
use http::{HeaderMap, Request, Response, StatusCode};
use quinn::crypto::rustls::QuicClientConfig;
use revaro_server::{
    auth, config::Config, db::Database, quic::NativeTransport, router, state::AppState,
    storage::LocalStore,
};
use sha2::{Digest, Sha256};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Client {
    requests: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    origin: String,
    cookie: String,
}
impl Client {
    async fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> (Response<()>, Bytes) {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.origin))
            .header("origin", &self.origin)
            .header("cookie", &self.cookie);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        let mut stream = self
            .requests
            .send_request(request.body(()).unwrap())
            .await
            .unwrap();
        if !body.is_empty() {
            stream.send_data(body).await.unwrap();
        }
        stream.finish().await.unwrap();
        let response = stream.recv_response().await.unwrap();
        let mut body = Vec::new();
        while let Some(mut data) = stream.recv_data().await.unwrap() {
            while data.has_remaining() {
                let chunk = data.chunk();
                body.extend_from_slice(chunk);
                let size = chunk.len();
                data.advance(size);
            }
        }
        (response, body.into())
    }

    async fn json(
        &mut self,
        method: &str,
        path: &str,
        value: serde_json::Value,
    ) -> (HeaderMap, serde_json::Value) {
        let (response, body) = self
            .request(
                method,
                path,
                &[("content-type", "application/json")],
                Bytes::from(value.to_string()),
            )
            .await;
        assert!(
            response.status().is_success(),
            "{}: {}",
            response.status(),
            String::from_utf8_lossy(&body)
        );
        (
            response.into_parts().0.headers,
            serde_json::from_slice(&body).unwrap(),
        )
    }
}

#[tokio::test]
async fn default_aggressive_auto_http3_preserves_verified_uploads_ranges_and_validators_for_every_mime()
 {
    tokio::time::timeout(Duration::from_secs(45), lifecycle())
        .await
        .expect("HTTP/3 lifecycle timeout");
}

async fn lifecycle() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("revaro-quic-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_path = scratch.0.join("cert.pem");
    let key_path = scratch.0.join("key.pem");
    std::fs::write(&cert_path, certificate.cert.pem()).unwrap();
    std::fs::write(&key_path, certificate.signing_key.serialize_pem()).unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr: SocketAddr = reservation.local_addr().unwrap();
    let origin = format!("https://localhost:{}", addr.port());
    let config = Config::from_lookup(&|key| match key {
        "APP_BASE_URL" => Some(origin.clone()),
        "APP_TLS_ADDR" | "APP_QUIC_ADDR" => Some(addr.to_string()),
        "APP_TLS_CERT" => Some(cert_path.display().to_string()),
        "APP_TLS_KEY" => Some(key_path.display().to_string()),
        "APP_DATA_DIR" => Some(scratch.0.join("database").display().to_string()),
        "APP_OBJECTS_DIR" => Some(scratch.0.join("objects").display().to_string()),
        "APP_CACHES_DIR" => Some(scratch.0.join("caches").display().to_string()),
        "APP_WEB_DIR" => Some("/nonexistent".into()),
        _ => None,
    })
    .unwrap();
    let quic = config.quic.as_ref().unwrap();
    assert_eq!(quic.mode, revaro_server::quic::CongestionMode::Aggressive);
    assert_eq!(quic.target_mbps, None);
    let db = Database::open_in_memory().unwrap();
    let store = LocalStore::open(config.objects_dir()).await.unwrap();
    let auth = auth::AuthService::new(db.clone());
    auth.initialize("admin", "quic-test-password")
        .await
        .unwrap();
    let state = AppState::new(Arc::new(config.clone()), db, store, auth);
    drop(reservation);
    let server = NativeTransport::start(&config, router::build(state.clone()))
        .await
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.cert.der().clone()).unwrap();
    let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).unwrap(),
    )));
    let connection = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut driver, requests) = h3::client::new(h3_quinn::Connection::new(connection.clone()))
        .await
        .unwrap();
    let task = tokio::spawn(async move { driver.wait_idle().await });
    let mut client = Client {
        requests,
        origin,
        cookie: String::new(),
    };
    let (response, _) = client.request("GET", "/readyz", &[], Bytes::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let (headers, _) = client
        .json(
            "POST",
            "/api/auth/login",
            serde_json::json!({
                "username": "admin", "password": "quic-test-password",
            }),
        )
        .await;
    client.cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into();
    let payload = Bytes::from((0..96 * 1024).map(|n| (n % 251) as u8).collect::<Vec<_>>());
    for (extension, mime) in [
        ("pdf", "application/pdf"),
        ("zip", "application/zip"),
        ("txt", "text/plain"),
        ("dat", "application/octet-stream"),
    ] {
        let (_, upload) = client
            .json(
                "POST",
                "/api/uploads",
                serde_json::json!({
                    "parent_id": "00000000-0000-0000-0000-000000000000",
                    "name": format!("quic.{extension}"), "size": payload.len(), "mime_type": mime,
                }),
            )
            .await;
        assert_eq!(upload["mode"], "multipart");
        let data_url = format!(
            "/api/uploads/{}/data/1",
            upload["upload_id"].as_str().unwrap()
        );
        let hash = hex::encode(Sha256::digest(&payload));
        let (response, _) = client
            .request(
                "PUT",
                &data_url,
                &[("x-content-sha256", "wrong")],
                payload.clone(),
            )
            .await;
        assert!(
            !response.status().is_success(),
            "QUIC must not bypass checksum verification"
        );
        let (response, _) = client
            .request(
                "PUT",
                &data_url,
                &[("x-content-sha256", &hash)],
                payload.clone(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let etag = response.headers()["etag"].to_str().unwrap().to_owned();
        let (repeated, _) = client
            .request(
                "PUT",
                &data_url,
                &[("x-content-sha256", &hash)],
                payload.clone(),
            )
            .await;
        assert_eq!(repeated.status(), StatusCode::NO_CONTENT);
        assert_eq!(repeated.headers()["etag"], etag);
        client
            .json(
                "POST",
                &format!(
                    "/api/uploads/{}/complete",
                    upload["upload_id"].as_str().unwrap()
                ),
                serde_json::json!({"parts": [{"part_number": 1, "etag": etag}]}),
            )
            .await;
        let path = format!(
            "/api/files/{}/download",
            upload["file_id"].as_str().unwrap()
        );
        let (response, bytes) = client
            .request("GET", &path, &[("range", "bytes=1234-4567")], Bytes::new())
            .await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()["content-range"],
            format!("bytes 1234-4567/{}", payload.len())
        );
        assert_eq!(&bytes[..], &payload[1234..4568]);
        let validator = response.headers()["etag"].to_str().unwrap().to_owned();
        let (response, bytes) = client
            .request(
                "GET",
                &path,
                &[("range", "bytes=4568-"), ("if-match", &validator)],
                Bytes::new(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(&bytes[..], &payload[4568..]);
        let (response, _) = client
            .request(
                "GET",
                &path,
                &[("range", "bytes=0-2"), ("if-match", "\"stale\"")],
                Bytes::new(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    }
    connection.close(0_u32.into(), b"test complete");
    drop(client);
    task.abort();
    server.finish().await;
    endpoint.wait_idle().await;
    state.cache.close().await;
}
