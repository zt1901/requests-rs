use bytes::{Buf, Bytes, BytesMut};
use http::{Response, StatusCode, Version};
use http2::{ext::Protocol, server};
use tokio::net::TcpListener;
use wreq::{Client, ws::message::Message};

fn parse_client_frame(buffer: &mut BytesMut) -> Option<(u8, Vec<u8>)> {
    if buffer.len() < 2 {
        return None;
    }
    let opcode = buffer[0] & 0x0f;
    let masked = buffer[1] & 0x80 != 0;
    let mut offset = 2;
    let mut length = usize::from(buffer[1] & 0x7f);
    if length == 126 {
        if buffer.len() < offset + 2 {
            return None;
        }
        length = usize::from(u16::from_be_bytes([buffer[offset], buffer[offset + 1]]));
        offset += 2;
    } else if length == 127 {
        if buffer.len() < offset + 8 {
            return None;
        }
        length = usize::try_from(u64::from_be_bytes(
            buffer[offset..offset + 8].try_into().unwrap(),
        ))
        .unwrap();
        offset += 8;
    }
    let mask = if masked {
        if buffer.len() < offset + 4 {
            return None;
        }
        let mask: [u8; 4] = buffer[offset..offset + 4].try_into().unwrap();
        offset += 4;
        Some(mask)
    } else {
        None
    };
    if buffer.len() < offset + length {
        return None;
    }
    let mut payload = buffer[offset..offset + length].to_vec();
    buffer.advance(offset + length);
    if let Some(mask) = mask {
        for (index, value) in payload.iter_mut().enumerate() {
            *value ^= mask[index % 4];
        }
    }
    Some((opcode, payload))
}

async fn run_server(listener: TcpListener) {
    let (socket, _) = listener.accept().await.unwrap();
    let mut builder = server::Builder::new();
    builder.enable_connect_protocol();
    let mut connection = builder.handshake::<_, Bytes>(socket).await.unwrap();
    let (request, mut respond) = connection.accept().await.unwrap().unwrap();
    assert_eq!(request.method(), http::Method::CONNECT);
    assert_eq!(request.version(), Version::HTTP_2);
    assert_eq!(request.uri().path(), "/socket");
    assert_eq!(
        request.extensions().get::<Protocol>(),
        Some(&Protocol::from_static("websocket"))
    );

    let mut stream_task = tokio::spawn(async move {
        let response = Response::builder().status(StatusCode::OK).body(()).unwrap();
        let mut outbound = respond.send_response(response, false).unwrap();
        outbound
            .send_data(Bytes::from_static(b"\x81\x07welcome"), false)
            .unwrap();

        let mut inbound = request.into_body();
        let mut buffer = BytesMut::new();
        while let Some(chunk) = inbound.data().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) if error.reason() == Some(http2::Reason::CANCEL) => return,
                Err(error) => panic!("HTTP/2 WebSocket接收失败: {error}"),
            };
            inbound
                .flow_control()
                .release_capacity(chunk.len())
                .unwrap();
            buffer.extend_from_slice(&chunk);
            while let Some((opcode, payload)) = parse_client_frame(&mut buffer) {
                match opcode {
                    1 => {
                        let mut frame = Vec::with_capacity(payload.len() + 2);
                        frame.extend_from_slice(&[0x81, u8::try_from(payload.len()).unwrap()]);
                        frame.extend_from_slice(&payload);
                        outbound.send_data(Bytes::from(frame), false).unwrap();
                    }
                    8 => {
                        outbound
                            .send_data(Bytes::from_static(b"\x88\x02\x03\xe8"), true)
                            .unwrap();
                        return;
                    }
                    _ => {}
                }
            }
        }
    });

    loop {
        tokio::select! {
            result = &mut stream_task => {
                result.unwrap();
                break;
            }
            incoming = connection.accept() => {
                if incoming.is_none() {
                    stream_task.await.unwrap();
                    break;
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn websocket_extended_connect_exchanges_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(run_server(listener));

    let response = Client::builder()
        .build()
        .unwrap()
        .websocket(format!("ws://{address}/socket"))
        .version(Version::HTTP_2)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.version(), Version::HTTP_2);
    let mut websocket = response.into_websocket().await.unwrap();

    assert_eq!(
        websocket.recv().await.unwrap().unwrap().to_text().unwrap(),
        "welcome"
    );
    websocket.send(Message::text("hello-h2")).await.unwrap();
    assert_eq!(
        websocket.recv().await.unwrap().unwrap().to_text().unwrap(),
        "hello-h2"
    );
    websocket.close(1000, "").await.unwrap();
    server.await.unwrap();
}
