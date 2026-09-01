use std::{
    env,
    io::{self, Write},
    net::{Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicU64, Ordering},
};
use std::fs;

use bytes::{Buf, Bytes};
use http::{Request, Response, StatusCode, header};
use quinn::crypto::rustls::QuicServerConfig;
use serde_json::json;

static 连接序号: AtomicU64 = AtomicU64::new(1);
fn 查询参数<'a>(request: &'a Request<()>, name: &str) -> Option<&'a str> {
    request.uri().query()?.split('&').find_map(|item| {
        let (key, value) = item.split_once('=')?;
        (key == name).then_some(value)
    })
}

fn 构造响应(
    request: &Request<()>,
    request_body: &[u8],
    connection_id: u64,
) -> Response<Bytes> {
    let path = request.uri().path();
    let builder = Response::builder()
        .status(StatusCode::OK)
        .header("x-http3-connection-id", connection_id.to_string())
        .header(header::CONTENT_TYPE, "application/json");
    if path == "/redirect" {
        return builder
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/final")
            .header(header::SET_COOKIE, "redirected=1; Path=/")
            .body(Bytes::new())
            .expect("重定向响应有效");
    }
    if path == "/benchmark" {
        let size = 查询参数(request, "size")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1024)
            .min(8 * 1024 * 1024);
        let body = Bytes::from(vec![b'x'; size]);
        return builder
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, body.len().to_string())
            .body(body)
            .expect("基准响应有效");
    }
    let body = json!({
        "method": request.method().as_str(),
        "path": path,
        "query": request.uri().query().unwrap_or(""),
        "body": String::from_utf8_lossy(request_body),
        "x_echo": request
            .headers()
            .get("x-echo")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
        "cookie": request
            .headers()
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
        "connection_id": connection_id,
    })
    .to_string();
    builder
        .header(header::CONTENT_LENGTH, body.len().to_string())
        .body(Bytes::from(body))
        .expect("JSON响应有效")
}

async fn 处理连接(connection: quinn::Connection, connection_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut h3_connection =
        h3::server::Connection::new(h3_quinn::Connection::new(connection)).await?;
    while let Some(resolver) = h3_connection.accept().await? {
        tokio::spawn(async move {
            let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async move {
                let (request, mut stream) = resolver.resolve_request().await?;
                let mut body = Vec::new();
                while let Some(mut chunk) = stream.recv_data().await? {
                    body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
                }
                let response = 构造响应(&request, &body, connection_id);
                let (parts, body) = response.into_parts();
                let delay_ms = if request.uri().path() == "/slow" {
                    500
                } else {
                    查询参数(&request, "delay_ms")
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(0)
                };
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                stream.send_response(Response::from_parts(parts, ())).await?;
                if !body.is_empty() {
                    stream.send_data(body).await?;
                }
                stream.finish().await?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                eprintln!("HTTP/3请求处理失败: {error}");
            }
        });
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let port = env::var("HTTP3_TEST_PORT")?.parse::<u16>()?;
    let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ])?;
    if let Ok(path) = env::var("HTTP3_TEST_CERT") {
        fs::write(path, cert.pem())?;
    }
    if let Ok(path) = env::var("HTTP3_TEST_KEY") {
        fs::write(path, signing_key.serialize_pem())?;
    }
    let certs = vec![cert.der().clone()];
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into();
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let server_config = quinn::ServerConfig::with_crypto(std::sync::Arc::new(
        QuicServerConfig::try_from(tls)?,
    ));
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let endpoint = quinn::Endpoint::server(server_config, address)?;
    println!("HTTP3_READY={}", endpoint.local_addr()?);
    io::stdout().flush()?;

    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(async move {
            match incoming.await {
                Ok(connection) => {
                    let connection_id = 连接序号.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = 处理连接(connection, connection_id).await {
                        eprintln!("HTTP/3连接失败: {error}");
                    }
                }
                Err(error) => eprintln!("QUIC握手失败: {error}"),
            }
        });
    }
    Ok(())
}
