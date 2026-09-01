use std::{
    env,
    fs::File,
    io::{self, BufReader, Write},
    net::{Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicU64, Ordering},
};

use bytes::{Buf, Bytes};
use http::{Request, Response, StatusCode, header};
use quinn::crypto::rustls::QuicServerConfig;
use serde_json::json;

static 连接序号: AtomicU64 = AtomicU64::new(1);

fn 读取证书(path: &str) -> io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::certs(&mut reader).collect()
}

fn 读取私钥(path: &str) -> io::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let mut reader = BufReader::new(File::open(path)?);
    rustls_pemfile::private_key(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "PEM文件中没有私钥"))
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
                if request.uri().path() == "/slow" {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
    let cert_path = env::var("HTTP3_CERT_FILE")?;
    let key_path = env::var("HTTP3_KEY_FILE")?;
    let port = env::var("HTTP3_TEST_PORT")?.parse::<u16>()?;

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(读取证书(&cert_path)?, 读取私钥(&key_path)?)?;
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
