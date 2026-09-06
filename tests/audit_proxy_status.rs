use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};
#[tokio::test]
async fn connect_rejection_preserves_proxy_status() {
    for code in [400, 403, 407, 429, 500, 502, 503, 504, 599, 631, 999] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
                assert!(header.len() < 8192);
            }
            assert!(header.starts_with(b"CONNECT "));
            socket.write_all(format!("HTTP/1.1 {code} Rejected\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
        });
        let client = wreq::Client::builder().proxy(wreq::Proxy::all(format!("http://{addr}")).unwrap()).build().unwrap();
        let error = client.get("https://example.invalid/").timeout(std::time::Duration::from_secs(5)).send().await.unwrap_err();
        let details = error.to_string();
        assert!(details.contains(&code.to_string()), "{details}");
        task.await.unwrap();
    }
}
