//! Independent wire vectors from RFC 9204 Appendix B and section 4.
//! Exercise the actual public vendored decoder, not extracted helper code.
use quiche::h3::qpack::{Decoder, Error};
use quiche::h3::Header;

fn hex(input: &str) -> Vec<u8> {
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(compact.len() % 2, 0);
    (0..compact.len())
        .step_by(2)
        .map(|n| u8::from_str_radix(&compact[n..n + 2], 16).unwrap())
        .collect()
}

fn control(decoder: &mut Decoder, bytes: &[u8]) {
    decoder.control(&mut bytes.to_vec()).unwrap();
}

fn prefixed(value: u64, bits: u8, tag: u8) -> Vec<u8> {
    let max = (1u64 << bits) - 1;
    if value < max {
        return vec![tag | value as u8];
    }
    let mut out = vec![tag | max as u8];
    let mut value = value - max;
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

fn literal_insert(name: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = prefixed(name.len() as u64, 5, 0x40);
    out.extend_from_slice(name);
    out.extend(prefixed(value.len() as u64, 7, 0));
    out.extend_from_slice(value);
    out
}

const B2: &str = "3fbd01 c00f7777772e6578616d706c652e636f6d c10c2f73616d706c652f70617468";
const B3: &str = "4a637573746f6d2d6b65790c637573746f6d2d76616c7565";

#[test]
fn rfc_appendix_b_literal_static_name() {
    assert_eq!(
        Decoder::new()
            .decode(&hex("0000 510b2f696e6465782e68746d6c"), u64::MAX)
            .unwrap(),
        vec![Header::new(b":path", b"/index.html")]
    );
}

#[test]
fn rfc_appendix_b_dynamic_duplicate_and_eviction() {
    let mut dec = Decoder::with_capacity(220);
    control(&mut dec, &hex(B2));
    assert_eq!(dec.insert_count(), 2);
    assert_eq!(
        dec.decode(&hex("0381 1011"), u64::MAX).unwrap(),
        vec![
            Header::new(b":authority", b"www.example.com"),
            Header::new(b":path", b"/sample/path")
        ]
    );
    control(&mut dec, &hex(B3));
    control(&mut dec, &[2]); // Appendix B.4 duplicates absolute entry 0.
    assert_eq!(dec.insert_count(), 4);
    assert_eq!(
        dec.decode(&hex("0500 80c181"), u64::MAX).unwrap(),
        vec![
            Header::new(b":authority", b"www.example.com"),
            Header::new(b":path", b"/"),
            Header::new(b"custom-key", b"custom-value")
        ]
    );
    control(&mut dec, &hex("810d637573746f6d2d76616c756532"));
    assert_eq!(dec.insert_count(), 5);
    assert_eq!(
        dec.decode(&hex("0600 80"), u64::MAX).unwrap(),
        vec![Header::new(b"custom-key", b"custom-value2")]
    );
    assert!(
        dec.decode(&hex("0600 84"), u64::MAX).is_err(),
        "evicted absolute entry zero cannot be read"
    );
}

#[test]
fn encoder_instructions_can_be_fragmented_at_every_byte() {
    let mut dec = Decoder::with_capacity(220);
    let all = hex(&format!("{B2} {B3} 02"));
    for byte in all {
        control(&mut dec, &[byte]);
    }
    assert_eq!(dec.insert_count(), 4);
    assert_eq!(dec.decode(&hex("0500 80c181"), u64::MAX).unwrap().len(), 3);
}

#[test]
fn incomplete_insert_is_not_visible_until_its_last_byte() {
    let mut dec = Decoder::with_capacity(64);
    control(&mut dec, &prefixed(64, 5, 0x20));
    let instruction = literal_insert(b"x", b"abc");
    control(&mut dec, &instruction[..instruction.len() - 1]);
    assert_eq!(dec.insert_count(), 0);
    control(&mut dec, &instruction[instruction.len() - 1..]);
    assert_eq!(dec.insert_count(), 1);
    assert_eq!(
        dec.decode(&[2, 0, 0x80], 36).unwrap(),
        vec![Header::new(b"x", b"abc")]
    );
}

#[test]
fn huffman_encoder_name_and_value_use_real_hpack_code_table() {
    let mut dec = Decoder::with_capacity(128);
    control(&mut dec, &prefixed(128, 5, 0x20));
    // RFC 7541 C.4.1 Huffman representation of "www.example.com".
    control(&mut dec, &hex("c0 8c f1e3c2e5f23a6ba0ab90f4ff"));
    control(&mut dec, &hex("6c f1e3c2e5f23a6ba0ab90f4ff 0176"));
    assert_eq!(
        dec.decode(&[3, 0, 0x81, 0x80], u64::MAX).unwrap(),
        vec![
            Header::new(b":authority", b"www.example.com"),
            Header::new(b"www.example.com", b"v")
        ]
    );
}

#[test]
fn dynamic_name_references_work_before_and_after_base() {
    let mut dec = Decoder::with_capacity(64);
    control(&mut dec, &prefixed(64, 5, 0x20));
    control(&mut dec, &literal_insert(b"x", b"old"));
    assert_eq!(
        dec.decode(&hex("0200 40036e6577"), u64::MAX).unwrap(),
        vec![Header::new(b"x", b"new")]
    );
    assert_eq!(
        dec.decode(&hex("0280 00036e6577"), u64::MAX).unwrap(),
        vec![Header::new(b"x", b"new")]
    );
}

#[test]
fn blocked_section_uses_frozen_required_insert_count() {
    let mut dec = Decoder::with_capacity(128);
    let block = [4, 0, 0x80]; // RIC 3, Base 3, absolute entry 2.
    let required = dec.required_insert_count(&block).unwrap();
    assert_eq!(required, 3);
    assert_eq!(
        dec.decode_with_required(&block, u64::MAX, required),
        Err(Error::Blocked)
    );
    control(&mut dec, &prefixed(128, 5, 0x20));
    for v in [b'a', b'b', b'c'] {
        control(&mut dec, &literal_insert(b"x", &[v]));
    }
    assert_eq!(
        dec.decode_with_required(&block, u64::MAX, required)
            .unwrap(),
        vec![Header::new(b"x", b"c")]
    );
}

#[test]
fn required_insert_count_wraps_using_maximum_not_current_capacity() {
    let mut dec = Decoder::with_capacity(128); // MaxEntries 4, FullRange 8.
    control(&mut dec, &prefixed(64, 5, 0x20)); // Current capacity is only 64.
    for v in 0..10 {
        control(&mut dec, &literal_insert(b"x", &[b'a' + v]));
    }
    assert_eq!(dec.insert_count(), 10);
    assert_eq!(dec.required_insert_count(&[3, 0, 0x80]), Ok(10));
    assert_eq!(
        dec.decode(&[3, 0, 0x80], u64::MAX).unwrap(),
        vec![Header::new(b"x", b"j")]
    );
}

#[test]
fn frozen_required_count_is_not_reinterpreted_after_wrap() {
    let mut dec = Decoder::with_capacity(64);
    let block = [2, 0, 0x80];
    let frozen = dec.required_insert_count(&block).unwrap();
    assert_eq!(frozen, 1);
    control(&mut dec, &prefixed(64, 5, 0x20));
    for v in 0..5 {
        control(&mut dec, &literal_insert(b"x", &[b'a' + v]));
    }
    assert_eq!(dec.required_insert_count(&block), Ok(5));
    assert!(
        dec.decode_with_required(&block, u64::MAX, frozen).is_err(),
        "must not substitute newer absolute entry 4 for evicted entry 0"
    );
}

#[test]
fn capacity_starts_zero_and_cannot_exceed_settings() {
    let mut dec = Decoder::with_capacity(64);
    assert!(dec.control(&mut literal_insert(b"x", b"v")).is_err());
    let mut dec = Decoder::with_capacity(64);
    assert!(dec.control(&mut prefixed(65, 5, 0x20)).is_err());
    let mut dec = Decoder::new();
    assert!(dec.control(&mut prefixed(1, 5, 0x20)).is_err());
}

#[test]
fn oversized_insert_and_invalid_duplicate_are_rejected() {
    let mut dec = Decoder::with_capacity(64);
    control(&mut dec, &prefixed(64, 5, 0x20));
    assert!(dec.control(&mut literal_insert(b"x", &[b'v'; 32])).is_err()); // 65 bytes.
    let mut dec = Decoder::with_capacity(64);
    control(&mut dec, &prefixed(64, 5, 0x20));
    assert!(dec.control(&mut [0]).is_err());
}

#[test]
fn shrinking_capacity_evicts_entries_but_preserves_insert_count() {
    let mut dec = Decoder::with_capacity(64);
    control(&mut dec, &prefixed(64, 5, 0x20));
    control(&mut dec, &literal_insert(b"x", b"v"));
    control(&mut dec, &[0x20]);
    assert_eq!(dec.insert_count(), 1);
    assert!(dec.decode(&[2, 0, 0x80], u64::MAX).is_err());
}

#[test]
fn invalid_wire_indices_bases_and_huffman_are_rejected() {
    for wire in [
        hex("0000 ff24"),
        hex("0080"),
        hex("0000 80"),
        hex("0000 10"),
        hex("0000 2981ff00"),
    ] {
        assert!(
            Decoder::new().decode(&wire, u64::MAX).is_err(),
            "wire={wire:02x?}"
        );
    }
    let mut dec = Decoder::with_capacity(64);
    assert!(
        dec.required_insert_count(&[5, 0]).is_err(),
        "EncodedInsertCount cannot exceed FullRange"
    );
    control(&mut dec, &prefixed(64, 5, 0x20));
    assert!(
        dec.control(&mut hex("c081ff")).is_err(),
        "invalid Huffman padding"
    );
}

#[test]
fn malicious_prefixed_integer_overflow_is_rejected() {
    let overflow = hex("ff ffffffffffffffffff02");
    assert!(Decoder::new().decode(&overflow, u64::MAX).is_err());
    let mut dec = Decoder::with_capacity(64);
    assert!(dec.control(&mut hex("3f ffffffffffffffffff02")).is_err());
}

#[test]
fn dynamic_fields_count_full_uncompressed_size_with_overhead() {
    let mut dec = Decoder::with_capacity(64);
    control(&mut dec, &prefixed(64, 5, 0x20));
    control(&mut dec, &literal_insert(b"x", b"abc"));
    assert_eq!(
        dec.decode(&[2, 0, 0x80], 35),
        Err(Error::HeaderListTooLarge)
    );
    assert_eq!(
        dec.decode(&[2, 0, 0x80], 36).unwrap(),
        vec![Header::new(b"x", b"abc")]
    );
}

// Real QUIC connections with a deliberately hand-written peer. This keeps the
// lifecycle oracle independent from quiche's static outbound QPACK encoder.
struct H3Peer {
    pipe: quiche::test_utils::Pipe,
    h3: quiche::h3::Connection,
}

fn transport_config(uni_window: u64) -> quiche::Config {
    // Load embedded fixtures: BoringSSL's narrow fopen cannot open Unicode
    // Windows checkout paths reliably.
    let mut tls = btls::ssl::SslContextBuilder::new(btls::ssl::SslMethod::tls()).unwrap();
    let cert =
        btls::x509::X509::from_pem(include_bytes!("../vendor/quiche/quiche/examples/cert.crt"))
            .unwrap();
    let key = btls::pkey::PKey::private_key_from_pem(include_bytes!(
        "../vendor/quiche/quiche/examples/cert.key"
    ))
    .unwrap();
    tls.set_certificate(&cert).unwrap();
    tls.set_private_key(&key).unwrap();
    let mut config =
        quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, tls).unwrap();
    config
        .set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
        .unwrap();
    config.verify_peer(false);
    config.grease(false);
    config.set_initial_max_data(64 * 1024 * 1024);
    config.set_initial_max_stream_data_bidi_local(16 * 1024 * 1024);
    config.set_initial_max_stream_data_bidi_remote(16 * 1024 * 1024);
    config.set_initial_max_stream_data_uni(uni_window);
    config.set_initial_max_streams_bidi(100_000);
    config.set_initial_max_streams_uni(16);
    config.set_max_idle_timeout(30_000);
    config
}

impl H3Peer {
    fn new(blocked: u64, feedback_window: u64) -> Self {
        let mut client = transport_config(65_536);
        let mut server = transport_config(feedback_window);
        let mut pipe =
            quiche::test_utils::Pipe::with_client_and_server_config(&mut client, &mut server)
                .unwrap();
        pipe.handshake().unwrap();
        let mut config = quiche::h3::Config::new().unwrap();
        config.set_max_field_section_size(32 * 1024 * 1024);
        config.set_qpack_max_table_capacity(64);
        config.set_qpack_blocked_streams(blocked);
        let h3 = quiche::h3::Connection::with_transport(&mut pipe.client, &config).unwrap();
        let mut peer = Self { pipe, h3 };
        // With GREASE disabled: server control=3, encoder=7, decoder=11.
        peer.send(3, &[0, 4, 0], false); // control type + empty SETTINGS
        peer.send(7, &[2], false);
        peer.pipe.advance().unwrap();
        peer.done();
        peer
    }

    fn request(&mut self) -> u64 {
        let id = self
            .h3
            .send_request(
                &mut self.pipe.client,
                &[
                    Header::new(b":method", b"GET"),
                    Header::new(b":scheme", b"https"),
                    Header::new(b":authority", b"example.com"),
                    Header::new(b":path", b"/"),
                ],
                true,
            )
            .unwrap();
        self.pipe.advance().unwrap();
        id
    }

    fn send(&mut self, stream: u64, bytes: &[u8], fin: bool) {
        assert_eq!(
            self.pipe.server.stream_send(stream, bytes, fin).unwrap(),
            bytes.len()
        );
    }

    fn headers(&mut self, stream: u64, block: &[u8], fin: bool) {
        assert!(block.len() < 64);
        let mut frame = vec![1, block.len() as u8];
        frame.extend_from_slice(block);
        self.send(stream, &frame, fin);
        self.pipe.advance().unwrap();
    }

    fn done(&mut self) {
        assert_eq!(
            self.h3.poll(&mut self.pipe.client),
            Err(quiche::h3::Error::Done)
        );
    }

    fn insert(&mut self, value: u8) {
        self.send(7, &literal_insert(b"x", &[value]), false);
        self.pipe.advance().unwrap();
    }

    fn capacity(&mut self) {
        self.send(7, &prefixed(64, 5, 0x20), false);
        self.pipe.advance().unwrap();
        self.done();
    }

    fn take_feedback(&mut self) -> Vec<u8> {
        self.pipe.advance().unwrap();
        let mut out = Vec::new();
        let mut buf = [0; 256];
        loop {
            match self.pipe.server.stream_recv(10, &mut buf) {
                Ok((n, fin)) => {
                    assert!(!fin, "QPACK decoder stream must remain open");
                    out.extend_from_slice(&buf[..n]);
                    if n == 0 {
                        break;
                    }
                }
                Err(quiche::Error::Done) => break,
                other => panic!("decoder feedback read: {other:?}"),
            }
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Feedback {
    Ack(u64),
    Cancel(u64),
    Increment(u64),
}

fn feedback(bytes: &[u8]) -> Vec<Feedback> {
    assert_eq!(bytes.first(), Some(&3), "decoder stream type: {bytes:02x?}");
    let mut pos = 1;
    let mut out = Vec::new();
    while pos < bytes.len() {
        let first = bytes[pos];
        pos += 1;
        let bits = if first & 0x80 != 0 { 7 } else { 6 };
        let mask = (1u8 << bits) - 1;
        let mut value = u64::from(first & mask);
        if value == u64::from(mask) {
            let mut shift = 0;
            loop {
                assert!(pos < bytes.len(), "truncated feedback: {bytes:02x?}");
                let byte = bytes[pos];
                pos += 1;
                value += u64::from(byte & 127) << shift;
                if byte & 128 == 0 {
                    break;
                }
                shift += 7;
                assert!(shift < 64);
            }
        }
        out.push(if first & 0x80 != 0 {
            Feedback::Ack(value)
        } else if first & 0x40 != 0 {
            Feedback::Cancel(value)
        } else {
            assert!(value > 0);
            Feedback::Increment(value)
        });
    }
    out
}

const DYNAMIC_RESPONSE: &[u8] = &[2, 0, 0xd9, 0x80]; // RIC 1, status 200, entry 0.

fn expect_dynamic_headers(peer: &mut H3Peer, id: u64, value: u8) {
    match peer.h3.poll(&mut peer.pipe.client).unwrap() {
        (stream, quiche::h3::Event::Headers { list, .. }) => {
            assert_eq!(stream, id);
            assert_eq!(
                list,
                vec![Header::new(b":status", b"200"), Header::new(b"x", &[value])]
            );
        }
        other => panic!("expected dynamic headers before body/FIN: {other:?}"),
    }
}

#[test]
fn h3_blocked_fin_waits_for_inserts_then_headers_ack_and_finished() {
    let mut peer = H3Peer::new(1, 4096);
    let id = peer.request();
    peer.headers(id, DYNAMIC_RESPONSE, true);
    peer.done(); // FIN must not escape while HEADERS are QPACK-blocked.
    peer.capacity();
    peer.insert(b'v');
    expect_dynamic_headers(&mut peer, id, b'v');
    assert_eq!(peer.h3.last_peer_header_block_raw(), DYNAMIC_RESPONSE);
    assert_eq!(
        peer.h3.poll(&mut peer.pipe.client),
        Ok((id, quiche::h3::Event::Finished))
    );
    peer.done();
    assert!(feedback(&peer.take_feedback()).contains(&Feedback::Ack(id)));
}

#[test]
fn h3_fragmented_encoder_instruction_does_not_unblock_prematurely() {
    let mut peer = H3Peer::new(1, 4096);
    let id = peer.request();
    peer.headers(id, DYNAMIC_RESPONSE, false);
    peer.done();
    peer.capacity();
    let insert = literal_insert(b"x", b"v");
    for byte in &insert[..insert.len() - 1] {
        peer.send(7, &[*byte], false);
        peer.pipe.advance().unwrap();
        peer.done();
    }
    peer.send(7, &insert[insert.len() - 1..], false);
    peer.pipe.advance().unwrap();
    expect_dynamic_headers(&mut peer, id, b'v');
    peer.done();
}

#[test]
fn h3_speculative_insert_is_acknowledged_without_a_header_section() {
    let mut peer = H3Peer::new(1, 4096);
    peer.capacity();
    peer.insert(b'v');
    peer.done();
    assert_eq!(
        feedback(&peer.take_feedback()),
        vec![Feedback::Increment(1)]
    );
}

#[test]
fn h3_zero_blocked_limit_is_enforced_without_changing_settings() {
    let mut peer = H3Peer::new(0, 4096);
    let id = peer.request();
    peer.headers(id, DYNAMIC_RESPONSE, false);
    assert_eq!(
        peer.h3.poll(&mut peer.pipe.client),
        Err(quiche::h3::Error::QpackDecompressionFailed)
    );
    assert_eq!(peer.pipe.client.local_error().unwrap().error_code, 0x200);
}

#[test]
fn h3_second_blocked_stream_exceeds_advertised_limit() {
    let mut peer = H3Peer::new(1, 4096);
    let first = peer.request();
    let second = peer.request();
    peer.headers(first, DYNAMIC_RESPONSE, false);
    peer.done();
    peer.headers(second, DYNAMIC_RESPONSE, false);
    assert_eq!(
        peer.h3.poll(&mut peer.pipe.client),
        Err(quiche::h3::Error::QpackDecompressionFailed)
    );
}

#[test]
fn h3_reset_cancels_blocked_section_and_releases_blocked_quota() {
    let mut peer = H3Peer::new(1, 4096);
    let id = peer.request();
    peer.headers(id, DYNAMIC_RESPONSE, false);
    peer.done();
    peer.pipe
        .server
        .stream_shutdown(id, quiche::Shutdown::Write, 0x10c)
        .unwrap();
    peer.pipe.advance().unwrap();
    assert_eq!(
        peer.h3.poll(&mut peer.pipe.client),
        Ok((id, quiche::h3::Event::Reset(0x10c)))
    );
    peer.done();
    let second = peer.request();
    peer.headers(second, DYNAMIC_RESPONSE, false);
    peer.done();
    peer.capacity();
    peer.insert(b'v');
    expect_dynamic_headers(&mut peer, second, b'v');
    let instructions = feedback(&peer.take_feedback());
    assert!(instructions.contains(&Feedback::Cancel(id)));
    assert!(!instructions.contains(&Feedback::Ack(id)));
    assert!(instructions.contains(&Feedback::Ack(second)));
}

#[test]
fn h3_explicit_read_abandon_cancels_blocked_section() {
    let mut peer = H3Peer::new(1, 4096);
    let id = peer.request();
    peer.headers(id, DYNAMIC_RESPONSE, false);
    peer.done();
    peer.h3
        .cancel_stream(&mut peer.pipe.client, id, 0x10c)
        .unwrap();
    peer.done();
    assert!(feedback(&peer.take_feedback()).contains(&Feedback::Cancel(id)));
}

#[test]
fn h3_body_is_not_delivered_ahead_of_blocked_headers() {
    let mut peer = H3Peer::new(1, 4096);
    let id = peer.request();
    peer.headers(id, DYNAMIC_RESPONSE, false);
    peer.send(id, &[0, 3, b'a', b'b', b'c'], true);
    peer.pipe.advance().unwrap();
    peer.done();
    peer.capacity();
    peer.insert(b'v');
    expect_dynamic_headers(&mut peer, id, b'v');
    assert_eq!(
        peer.h3.poll(&mut peer.pipe.client),
        Ok((id, quiche::h3::Event::Data))
    );
    let mut body = [0; 8];
    assert_eq!(
        peer.h3.recv_body(&mut peer.pipe.client, id, &mut body),
        Ok(3)
    );
    assert_eq!(&body[..3], b"abc");
}

#[test]
fn h3_feedback_survives_flow_control_and_partial_multibyte_instruction() {
    // Type + fourteen one-byte increments consume 15/16 bytes of stream credit.
    // Cancellation for stream 64 needs two bytes, forcing a partial write.
    let mut peer = H3Peer::new(1, 16);
    peer.capacity();
    for _ in 0..14 {
        peer.insert(b'v');
        peer.done();
    }
    let mut id = 0;
    for _ in 0..17 {
        id = peer.request();
    }
    assert_eq!(id, 64);
    peer.h3
        .cancel_stream(&mut peer.pipe.client, id, 0x10c)
        .unwrap();
    peer.done();
    for _ in 14..80 {
        peer.insert(b'v');
        peer.done();
    }
    let mut bytes = Vec::new();
    for _ in 0..64 {
        bytes.extend(peer.take_feedback());
        peer.pipe.advance().unwrap();
        peer.done();
    }
    let instructions = feedback(&bytes);
    assert_eq!(
        instructions
            .iter()
            .filter_map(|f| match f {
                Feedback::Increment(n) => Some(*n),
                _ => None,
            })
            .sum::<u64>(),
        80
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|f| **f == Feedback::Cancel(64))
            .count(),
        1
    );
}

#[test]
fn h3_invalid_encoder_capacity_uses_encoder_stream_error_code() {
    let mut peer = H3Peer::new(1, 4096);
    peer.send(7, &prefixed(65, 5, 0x20), false);
    peer.pipe.advance().unwrap();
    assert_eq!(
        peer.h3.poll(&mut peer.pipe.client),
        Err(quiche::h3::Error::QpackEncoderStreamError)
    );
    assert_eq!(peer.pipe.client.local_error().unwrap().error_code, 0x201);
}

#[test]
fn h3_impossible_ack_for_static_encoder_is_not_silently_discarded() {
    let mut peer = H3Peer::new(1, 4096);
    let id = peer.request();
    assert_eq!(id, 0);
    peer.send(11, &[3, 0x80], false); // ACK stream 0 despite zero dynamic refs.
    peer.pipe.advance().unwrap();
    assert_eq!(
        peer.h3.poll(&mut peer.pipe.client),
        Err(quiche::h3::Error::QpackDecoderStreamError)
    );
    assert_eq!(peer.pipe.client.local_error().unwrap().error_code, 0x202);
}
// Large, valid field section: it blocks on entry zero before allocating the
// decoded literal. Wire length is exact so +/- one-byte accounting is testable.
fn padded_dynamic_section(size: usize) -> Vec<u8> {
    let mut value_len = size - 6;
    loop {
        let encoded_len = prefixed(value_len as u64, 7, 0);
        let total = 6 + encoded_len.len() + value_len;
        if total == size {
            let mut out = DYNAMIC_RESPONSE.to_vec();
            out.extend_from_slice(&[0x21, b'z']);
            out.extend(encoded_len);
            out.resize(size, b'a');
            return out;
        }
        value_len = size - 6 - encoded_len.len();
    }
}

fn send_large_block(peer: &mut H3Peer, id: u64, block: &[u8]) -> quiche::h3::Result<()> {
    let len = u32::try_from(block.len()).unwrap();
    assert!(len < (1 << 30));
    let mut frame = vec![1];
    frame.extend_from_slice(&(len | 0x8000_0000).to_be_bytes());
    frame.extend_from_slice(block);
    // Keep real packet processing bounded and allow H3 to incrementally consume
    // bytes, rather than bypassing stream framing or mutating private counters.
    let mut offset = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while offset < frame.len() {
        assert!(
            std::time::Instant::now() < deadline,
            "large frame transport stalled"
        );
        let end = (offset + 16_384).min(frame.len());
        match peer.pipe.server.stream_send(id, &frame[offset..end], false) {
            Ok(n) => offset += n,
            Err(quiche::Error::Done) => (),
            Err(e) => panic!("large frame send: {e:?}"),
        }
        peer.pipe.advance().unwrap();
        match peer.h3.poll(&mut peer.pipe.client) {
            Err(quiche::h3::Error::Done) => (),
            Err(e) => return Err(e),
            Ok(event) => panic!("unexpected event before insert: {event:?}"),
        }
        peer.pipe.advance().unwrap();
    }
    Ok(())
}

#[test]
fn h3_blocked_bytes_accept_exact_limit_reject_next_section() {
    const HALF: usize = 8 * 1024 * 1024;
    let mut peer = H3Peer::new(3, 4096);
    let block = padded_dynamic_section(HALF);
    for _ in 0..2 {
        let id = peer.request();
        send_large_block(&mut peer, id, &block).unwrap();
    }
    assert_eq!(peer.h3.qpack_resource_usage().0, 2);
    assert_eq!(peer.h3.qpack_resource_usage().1, 2 * HALF);
    let third = peer.request();
    assert_eq!(
        send_large_block(&mut peer, third, DYNAMIC_RESPONSE),
        Err(quiche::h3::Error::ExcessiveLoad)
    );
    assert_eq!(peer.pipe.client.local_error().unwrap().error_code, 0x107);
    assert_eq!(peer.h3.qpack_resource_usage().1, 2 * HALF);
}

#[test]
fn h3_blocked_byte_quota_is_released_by_cancel_and_success() {
    const HALF: usize = 8 * 1024 * 1024;
    let mut peer = H3Peer::new(3, 4096);
    let block = padded_dynamic_section(HALF);
    let first = peer.request();
    let second = peer.request();
    send_large_block(&mut peer, first, &block).unwrap();
    send_large_block(&mut peer, second, &block).unwrap();
    peer.h3
        .cancel_stream(&mut peer.pipe.client, first, 0x10c)
        .unwrap();
    assert_eq!(peer.h3.qpack_resource_usage().1, HALF);
    let third = peer.request();
    send_large_block(&mut peer, third, &block).unwrap();
    assert_eq!(peer.h3.qpack_resource_usage().1, 2 * HALF);
    peer.capacity();
    peer.insert(b'v');
    let mut received = Vec::new();
    for remaining in [HALF, 0] {
        match peer.h3.poll(&mut peer.pipe.client).unwrap() {
            (id, quiche::h3::Event::Headers { list, .. }) => {
                assert_eq!(list.len(), 3);
                received.push(id);
            }
            event => panic!("expected resumed large section: {event:?}"),
        }
        assert_eq!(peer.h3.qpack_resource_usage().1, remaining);
    }
    received.sort_unstable();
    assert_eq!(received, vec![second, third]);
    peer.done();
}

fn queue_cancel_without_feedback_consumption(peer: &mut H3Peer) -> quiche::h3::Result<()> {
    let id = peer.h3.send_request(
        &mut peer.pipe.client,
        &[
            Header::new(b":method", b"GET"),
            Header::new(b":scheme", b"https"),
            Header::new(b":authority", b"example.com"),
            Header::new(b":path", b"/"),
        ],
        true,
    )?;
    // Deliver and consume request bytes to replenish transport credit without
    // consuming decoder-stream feedback (stream 10), whose limit we test.
    peer.pipe.advance().unwrap();
    let mut request = [0; 1024];
    loop {
        match peer.pipe.server.stream_recv(id, &mut request) {
            Ok((_, true)) | Err(quiche::Error::Done) => break,
            Ok((_, false)) => (),
            Err(e) => panic!("request drain: {e:?}"),
        }
    }
    peer.pipe.advance().unwrap();
    let result = peer.h3.cancel_stream(&mut peer.pipe.client, id, 0x10c);
    if result.is_ok() {
        peer.pipe.advance().unwrap();
    }
    result
}

#[test]
fn h3_feedback_queue_limit_fails_closed_without_partial_instruction() {
    const LIMIT: usize = 64 * 1024;
    let mut peer = H3Peer::new(1, 16);
    // Do not read decoder stream: transport accepts its first 16 bytes only.
    // Cancellations create real instructions without needing tens of MB of
    // speculative encoder inserts. 100k stream credit is pre-negotiated.
    let mut previous = 0;
    for _ in 0..30_000 {
        match queue_cancel_without_feedback_consumption(&mut peer) {
            Ok(()) => {
                previous = peer.h3.qpack_resource_usage().2;
                assert!(previous <= LIMIT);
            }
            Err(quiche::h3::Error::ExcessiveLoad) => {
                assert_eq!(peer.h3.qpack_resource_usage().2, previous);
                assert!(previous >= LIMIT - 3);
                assert_eq!(peer.pipe.client.local_error().unwrap().error_code, 0x107);
                return;
            }
            Err(e) => panic!("unexpected queue result: {e:?}"),
        }
    }
    panic!("feedback limit was never enforced");
}

#[test]
fn h3_feedback_queue_recovers_after_peer_consumption() {
    let mut peer = H3Peer::new(1, 16);
    for _ in 0..10_000 {
        queue_cancel_without_feedback_consumption(&mut peer).unwrap();
    }
    let pending = peer.h3.qpack_resource_usage().2;
    assert!(pending > 20_000);
    // Draining a tiny receive window exercises actual MAX_STREAM_DATA credit,
    // partial instruction retries, and queue accounting all the way to zero.
    let mut wire = Vec::new();
    for _ in 0..10_000 {
        wire.extend(peer.take_feedback());
        peer.pipe.advance().unwrap();
        peer.done();
        if peer.h3.qpack_resource_usage().2 == 0 {
            wire.extend(peer.take_feedback());
            break;
        }
    }
    assert_eq!(peer.h3.qpack_resource_usage().2, 0);
    let instructions = feedback(&wire);
    assert_eq!(instructions.len(), 10_000);
    for (n, instruction) in instructions.iter().enumerate() {
        assert_eq!(*instruction, Feedback::Cancel(n as u64 * 4));
    }
    queue_cancel_without_feedback_consumption(&mut peer).unwrap();
    assert!(peer.pipe.client.local_error().is_none());
}
