use std::{
    env,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const 最大请求头字节数: usize = 64 * 1024;
const 代理密码: &str = "local-ipipgo-password";
const 用户名前缀: &str = "customer-local-zone-residential-session-";

#[derive(Default)]
struct 统计 {
    accept: AtomicU64,
    header_error: AtomicU64,
    auth_error: AtomicU64,
    upstream_error: AtomicU64,
    tunnel_success: AtomicU64,
    client_to_upstream: AtomicU64,
    upstream_to_client: AtomicU64,
}

#[derive(Serialize)]
struct 统计快照 {
    accept: u64,
    header_error: u64,
    auth_error: u64,
    upstream_error: u64,
    tunnel_success: u64,
    client_to_upstream: u64,
    upstream_to_client: u64,
}

impl 统计 {
    fn 快照(&self) -> 统计快照 {
        统计快照 {
            accept: self.accept.load(Ordering::Relaxed),
            header_error: self.header_error.load(Ordering::Relaxed),
            auth_error: self.auth_error.load(Ordering::Relaxed),
            upstream_error: self.upstream_error.load(Ordering::Relaxed),
            tunnel_success: self.tunnel_success.load(Ordering::Relaxed),
            client_to_upstream: self.client_to_upstream.load(Ordering::Relaxed),
            upstream_to_client: self.upstream_to_client.load(Ordering::Relaxed),
        }
    }

    fn 重置(&self) {
        self.accept.store(0, Ordering::Relaxed);
        self.header_error.store(0, Ordering::Relaxed);
        self.auth_error.store(0, Ordering::Relaxed);
        self.upstream_error.store(0, Ordering::Relaxed);
        self.tunnel_success.store(0, Ordering::Relaxed);
        self.client_to_upstream.store(0, Ordering::Relaxed);
        self.upstream_to_client.store(0, Ordering::Relaxed);
    }
}

async fn 读取请求头(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&chunk[..read]);
        if data.windows(4).any(|value| value == b"\r\n\r\n") {
            return Ok(data);
        }
        if data.len() > 最大请求头字节数 {
            return Err(std::io::Error::other("请求头超过64KiB"));
        }
    }
}

fn 解析认证(headers: &str) -> bool {
    let Some(value) = headers.lines().find_map(|line| {
        line.split_once(':').and_then(|(name, value)| {
            name.eq_ignore_ascii_case("proxy-authorization")
                .then(|| value.trim())
        })
    }) else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(credentials) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((username, password)) = credentials.split_once(':') else {
        return false;
    };
    username.starts_with(用户名前缀) && password == 代理密码
}

async fn 处理连接(mut client: TcpStream, stats: Arc<统计>) -> std::io::Result<()> {
    stats.accept.fetch_add(1, Ordering::Relaxed);
    let data = match 读取请求头(&mut client).await {
        Ok(data) => data,
        Err(error) => {
            stats.header_error.fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
    };
    let request = String::from_utf8_lossy(&data);
    let first_line = request.lines().next().unwrap_or_default();
    if matches!(first_line, "GET /stats HTTP/1.1" | "GET /reset HTTP/1.1") {
        if first_line == "GET /reset HTTP/1.1" {
            stats.重置();
        }
        let body = serde_json::to_vec(&stats.快照()).unwrap_or_default();
        client
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await?;
        client.write_all(&body).await?;
        return Ok(());
    }
    let Some(authority) = first_line
        .strip_prefix("CONNECT ")
        .and_then(|value| value.strip_suffix(" HTTP/1.1"))
    else {
        stats.header_error.fetch_add(1, Ordering::Relaxed);
        client
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    };
    if !解析认证(&request) {
        stats.auth_error.fetch_add(1, Ordering::Relaxed);
        client
            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    let mut upstream = match TcpStream::connect(authority).await {
        Ok(stream) => stream,
        Err(error) => {
            stats.upstream_error.fetch_add(1, Ordering::Relaxed);
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await?;
            return Err(error);
        }
    };
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    stats.tunnel_success.fetch_add(1, Ordering::Relaxed);
    let (up, down) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    stats.client_to_upstream.fetch_add(up, Ordering::Relaxed);
    stats.upstream_to_client.fetch_add(down, Ordering::Relaxed);
    Ok(())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = env::var("BENCHMARK_PROXY_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(18080);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(address).await?;
    let stats = Arc::new(统计::default());
    loop {
        let (stream, _) = listener.accept().await?;
        let stats = stats.clone();
        tokio::spawn(async move {
            let _ = 处理连接(stream, stats).await;
        });
    }
}
