use std::{
    cell::Cell,
    collections::HashSet,
    io::{Read, Write},
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use btls::ssl::{
    CertificateCompressionAlgorithm, CertificateCompressor, ExtensionType, KeyShare,
    SslContextBuilder, SslMethod, SslVerifyMode, SslVersion,
};
use btls::x509::{X509, store::X509Store, store::X509StoreBuilder, verify::X509CheckFlags};
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use quiche::h3::NameValue;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::{Host, Url};

/// Shared with the async owner: dropping its future stops a running worker.
#[derive(Clone)]
pub(crate) struct H3Control {
    cancelled: Arc<AtomicBool>,
    started: Instant,
    timeout: Duration,
}

impl H3Control {
    pub(crate) fn new(started: Instant, timeout: Duration) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            started,
            timeout,
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            bail!("HTTP/3 request cancelled")
        }
        if self.started.elapsed() >= self.timeout {
            bail!("HTTP/3请求超过timeout")
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ProfileHttp3 {
    pub tls: H3TlsCapture,
    pub quic: H3QuicCapture,
    pub http: H3HttpCapture,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3TlsCapture {
    pub client_hello_length: usize,
    pub cipher_suites: Vec<u16>,
    pub extensions: Vec<u16>,
    #[serde(default)]
    pub extension_wire: Vec<H3TlsExtensionWire>,
    pub supported_groups: Vec<u16>,
    #[serde(default)]
    pub key_share_groups: Vec<u16>,
    #[serde(default)]
    pub signature_algorithms: Vec<u16>,
    #[serde(default)]
    pub certificate_compression_algorithms: Vec<u16>,
    #[serde(default)]
    pub alpn: Vec<String>,
    #[serde(default)]
    pub has_ech: bool,
    #[serde(default)]
    pub uses_grease: bool,
    pub wire: H3TlsWire,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3TlsWire {
    pub client_hello_base64: String,
    pub client_hello_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3TlsExtensionWire {
    pub id: u16,
    pub length: usize,
    pub payload_base64: String,
    pub payload_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3QuicCapture {
    pub transport_parameters: H3TransportParameters,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3TransportParameters {
    pub max_idle_timeout: u64,
    pub max_udp_payload_size: usize,
    pub initial_max_data: u64,
    pub initial_max_stream_data_bidi_local: u64,
    pub initial_max_stream_data_bidi_remote: u64,
    pub initial_max_stream_data_uni: u64,
    pub initial_max_streams_bidi: u64,
    pub initial_max_streams_uni: u64,
    pub ack_delay_exponent: u64,
    pub max_ack_delay: u64,
    pub disable_active_migration: bool,
    pub active_connection_id_limit: u64,
    pub max_datagram_frame_size: u64,
    pub wire: H3TransportWire,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3TransportWire {
    pub length: usize,
    pub sha256: String,
    pub base64: String,
    pub parameters: Vec<H3TransportWireParameter>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3TransportWireParameter {
    pub id: u64,
    pub length: usize,
    pub value_hex: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3HttpCapture {
    pub protocol: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub header_order: Vec<String>,
    #[serde(default)]
    pub settings: Vec<(u64, u64)>,
    pub qpack: H3QpackCapture,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct H3QpackCapture {
    pub raw_capture: bool,
    pub header_block_length: usize,
    pub header_block_base64: String,
    pub header_block_sha256: String,
}

pub(crate) struct H3Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
    pub verify: bool,
    pub max_response_bytes: usize,
    pub peer: Option<SocketAddr>,
}

pub(crate) struct H3Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct H3RequestError {
    pub error: anyhow::Error,
    pub response_received: bool,
}

impl std::fmt::Display for H3RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.error)
    }
}

impl std::error::Error for H3RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

pub(crate) struct H3Client {
    profile: ProfileHttp3,
    origin: String,
    socket: UdpSocket,
    local: SocketAddr,
    conn: quiche::Connection,
    h3_config: quiche::h3::Config,
    h3: Option<quiche::h3::Connection>,
    out: Vec<u8>,
    input: Vec<u8>,
}

fn is_grease(value: u16) -> bool {
    value & 0x0f0f == 0x0a0a
}

fn decode_wire(value: &str, scope: &str, length: usize) -> Result<Vec<u8>> {
    let decoded = BASE64
        .decode(value)
        .with_context(|| format!("{scope}不是有效Base64"))?;
    if decoded.len() != length {
        bail!("{scope}声明长度{length}，实际长度{}", decoded.len());
    }
    Ok(decoded)
}

fn verify_sha256(bytes: &[u8], expected: &str, scope: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("{scope} SHA-256不匹配")
    }
    Ok(())
}

fn decode_hex(value: &str, scope: &str) -> Result<Vec<u8>> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{scope} is not valid hexadecimal")
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("{scope} is not valid hexadecimal"))
}

fn decode_varint(input: &[u8], offset: &mut usize, scope: &str) -> Result<u64> {
    let first = *input
        .get(*offset)
        .with_context(|| format!("{scope} truncated QUIC varint"))?;
    let length = 1_usize << (first >> 6);
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= input.len())
        .with_context(|| format!("{scope} truncated QUIC varint"))?;
    let mut value = u64::from(first & 0x3f);
    for byte in &input[*offset + 1..end] {
        value = (value << 8) | u64::from(*byte);
    }
    *offset = end;
    Ok(value)
}

fn decode_parameter_value(bytes: &[u8], id: u64) -> Result<u64> {
    let mut offset = 0;
    let value = decode_varint(bytes, &mut offset, &format!("QUIC TP {id}"))?;
    if offset != bytes.len() {
        bail!("QUIC TP {id} value is not one canonical varint")
    }
    Ok(value)
}

fn validate_transport_parameters(tp: &H3TransportParameters, raw: &[u8]) -> Result<()> {
    const MAX_VARINT: u64 = (1_u64 << 62) - 1;
    // These values configure both the transport state and its captured wire
    // override. Reject invalid values instead of relying on quiche's setters
    // to clamp them and silently disagree with the advertised fingerprint.
    if !(1200..=65_527).contains(&tp.max_udp_payload_size)
        || tp.initial_max_streams_bidi > (1_u64 << 60)
        || tp.initial_max_streams_uni > (1_u64 << 60)
        || tp.ack_delay_exponent > 20
        || tp.max_ack_delay >= (1_u64 << 14)
        || tp.active_connection_id_limit < 2
        || [
            tp.max_idle_timeout,
            tp.initial_max_data,
            tp.initial_max_stream_data_bidi_local,
            tp.initial_max_stream_data_bidi_remote,
            tp.initial_max_stream_data_uni,
            tp.active_connection_id_limit,
            tp.max_datagram_frame_size,
        ]
        .iter()
        .any(|value| *value > MAX_VARINT)
        || Instant::now()
            .checked_add(Duration::from_millis(tp.max_idle_timeout))
            .is_none()
    {
        bail!("QUIC Transport Parameters exceed supported numeric bounds")
    }
    let mut offset = 0;
    let mut seen = HashSet::new();
    for item in &tp.wire.parameters {
        if !seen.insert(item.id) || item.length.checked_mul(2) != Some(item.value_hex.len()) {
            bail!("QUIC Transport Parameters wire.parameters is invalid")
        }
        let expected = decode_hex(&item.value_hex, "QUIC TP value_hex")?;
        let id = decode_varint(raw, &mut offset, "QUIC TP id")?;
        let length = decode_varint(raw, &mut offset, "QUIC TP length")?;
        if id != item.id || length != item.length as u64 {
            bail!("QUIC Transport Parameters structured order/length disagrees with wire")
        }
        let end = offset
            .checked_add(item.length)
            .filter(|end| *end <= raw.len())
            .context("QUIC Transport Parameters value is truncated")?;
        if raw[offset..end] != expected {
            bail!("QUIC Transport Parameters structured value disagrees with wire")
        }
        offset = end;
    }
    if offset != raw.len() {
        bail!("QUIC Transport Parameters wire contains undeclared trailing bytes")
    }

    let semantic = [
        (1, tp.max_idle_timeout),
        (3, tp.max_udp_payload_size as u64),
        (4, tp.initial_max_data),
        (5, tp.initial_max_stream_data_bidi_local),
        (6, tp.initial_max_stream_data_bidi_remote),
        (7, tp.initial_max_stream_data_uni),
        (8, tp.initial_max_streams_bidi),
        (9, tp.initial_max_streams_uni),
        (10, tp.ack_delay_exponent),
        (11, tp.max_ack_delay),
        (14, tp.active_connection_id_limit),
        (32, tp.max_datagram_frame_size),
    ];
    for (id, expected) in semantic {
        if let Some(item) = tp.wire.parameters.iter().find(|item| item.id == id) {
            let bytes = decode_hex(&item.value_hex, "QUIC TP value_hex")?;
            if decode_parameter_value(&bytes, id)? != expected {
                bail!("QUIC TP {id} disagrees with its structured semantic value")
            }
        } else {
            let default = match id {
                10 => Some(3),
                11 => Some(25),
                14 => Some(2),
                32 => Some(0),
                _ => None,
            };
            if default != Some(expected) {
                bail!("QUIC TP {id} is required by its structured semantic value")
            }
        }
    }
    let migration = tp.wire.parameters.iter().find(|item| item.id == 12);
    if tp.disable_active_migration != migration.is_some()
        || migration.is_some_and(|item| item.length != 0)
    {
        bail!("QUIC TP 12 disagrees with disable_active_migration")
    }
    if tp
        .wire
        .parameters
        .iter()
        .any(|item| matches!(item.id, 0 | 2 | 13 | 16))
    {
        bail!("HTTP/3 client template contains a server-only QUIC transport parameter")
    }
    let source_cid = tp
        .wire
        .parameters
        .iter()
        .find(|item| item.id == 15)
        .context("HTTP/3 template is missing initial_source_connection_id")?;
    if source_cid.length != 0 {
        bail!("HTTP/3 replay requires the captured zero-length source connection ID")
    }
    Ok(())
}

fn validate_settings(profile: &ProfileHttp3) -> Result<()> {
    for &(id, value) in &profile.http.settings {
        if id >= (1_u64 << 62) || value >= (1_u64 << 62) {
            bail!("HTTP/3 SETTINGS exceeds QUIC varint range")
        }
        // quiche omits these settings when disabled. Advertising a captured
        // zero would therefore be a silent wire mismatch, not exact replay.
        if matches!(id, 8 | 51) && value != 1 {
            if id == 51 {
                bail!("HTTP/3 SETTINGS H3_DATAGRAM cannot reproduce this boolean value")
            }
            bail!("HTTP/3 SETTINGS {id} cannot reproduce this boolean value")
        }
    }
    let grease_ids = profile
        .http
        .settings
        .iter()
        .filter(|(id, _)| *id >= 33 && (*id - 33) % 31 == 0)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if grease_ids.len() != 1 {
        bail!("HTTP/3 SETTINGS must contain exactly one GREASE placeholder")
    }
    let mut expected_order = [1_u64, 6, 7, 8, 51]
        .into_iter()
        .filter(|expected| profile.http.settings.iter().any(|(id, _)| id == expected))
        .collect::<Vec<_>>();
    expected_order.extend(grease_ids);
    let actual_order = profile
        .http
        .settings
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    if actual_order != expected_order {
        bail!("HTTP/3 SETTINGS order or identifier cannot be reproduced exactly")
    }
    let datagram = profile.http.settings.iter().find(|(id, _)| *id == 51);
    if (profile.quic.transport_parameters.max_datagram_frame_size > 0)
        != datagram.is_some_and(|(_, value)| *value == 1)
    {
        bail!("HTTP/3 SETTINGS H3_DATAGRAM disagrees with QUIC DATAGRAM support")
    }
    Ok(())
}

pub(crate) fn validate(profile: &ProfileHttp3) -> Result<()> {
    if profile.http.protocol != "HTTP/3" {
        bail!("http3.http.protocol必须为HTTP/3")
    }
    if profile.tls.alpn != ["h3"] {
        bail!("http3.tls.alpn必须严格为[h3]")
    }
    let client_hello = decode_wire(
        &profile.tls.wire.client_hello_base64,
        "HTTP/3 TLS ClientHello",
        profile.tls.client_hello_length,
    )?;
    verify_sha256(
        &client_hello,
        &profile.tls.wire.client_hello_sha256,
        "HTTP/3 TLS ClientHello",
    )?;
    for (scope, ids) in [
        ("cipher_suites", profile.tls.cipher_suites.as_slice()),
        ("extensions", profile.tls.extensions.as_slice()),
        ("supported_groups", profile.tls.supported_groups.as_slice()),
        ("key_share_groups", profile.tls.key_share_groups.as_slice()),
        (
            "signature_algorithms",
            profile.tls.signature_algorithms.as_slice(),
        ),
    ] {
        let mut seen = HashSet::new();
        if ids.iter().any(|id| !is_grease(*id) && !seen.insert(*id)) {
            bail!("http3.tls.{scope}包含重复ID")
        }
    }
    let unsupported_ciphers = profile
        .tls
        .cipher_suites
        .iter()
        .copied()
        .filter(|id| !is_grease(*id) && cipher_name(*id).is_none())
        .collect::<Vec<_>>();
    let unsupported_groups = profile
        .tls
        .supported_groups
        .iter()
        .chain(&profile.tls.key_share_groups)
        .copied()
        .filter(|id| !is_grease(*id) && group_name(*id).is_none())
        .collect::<Vec<_>>();
    let unsupported_signatures = profile
        .tls
        .signature_algorithms
        .iter()
        .copied()
        .filter(|id| !is_grease(*id) && signature_name(*id).is_none())
        .collect::<Vec<_>>();
    let unsupported_extensions = profile
        .tls
        .extensions
        .iter()
        .copied()
        .filter(|id| !is_grease(*id) && extension_type(*id).is_none())
        .collect::<Vec<_>>();
    if !unsupported_ciphers.is_empty()
        || !unsupported_groups.is_empty()
        || !unsupported_signatures.is_empty()
        || !unsupported_extensions.is_empty()
    {
        bail!(
            "HTTP/3 TLS模板包含不能等价回放的ID: ciphers={unsupported_ciphers:?}, groups={unsupported_groups:?}, signatures={unsupported_signatures:?}, extensions={unsupported_extensions:?}"
        )
    }
    if profile
        .tls
        .certificate_compression_algorithms
        .iter()
        .any(|id| *id != 2)
    {
        bail!("HTTP/3当前只支持线级验证过的Brotli证书压缩算法")
    }
    let wire_ids = profile
        .tls
        .extension_wire
        .iter()
        .filter(|item| !is_grease(item.id))
        .map(|item| item.id)
        .collect::<Vec<_>>();
    if wire_ids != profile.tls.extensions {
        bail!("http3.tls.extension_wire与extensions顺序不一致")
    }
    for item in &profile.tls.extension_wire {
        let payload = decode_wire(
            &item.payload_base64,
            "HTTP/3 TLS extension payload",
            item.length,
        )?;
        verify_sha256(
            &payload,
            &item.payload_sha256,
            &format!("HTTP/3 TLS extension {}", item.id),
        )?;
    }
    let tp = &profile.quic.transport_parameters;
    let tp_wire = decode_wire(
        &tp.wire.base64,
        "QUIC Transport Parameters wire",
        tp.wire.length,
    )?;
    verify_sha256(&tp_wire, &tp.wire.sha256, "QUIC Transport Parameters")?;
    if tp_wire.is_empty() || tp.max_udp_payload_size < 1200 || tp.active_connection_id_limit < 2 {
        bail!("QUIC Transport Parameters包含非法边界值")
    }
    validate_transport_parameters(tp, &tp_wire)?;
    let mut tp_ids = HashSet::new();
    for item in &tp.wire.parameters {
        if !tp_ids.insert(item.id)
            || item.length.checked_mul(2) != Some(item.value_hex.len())
            || !item.value_hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("QUIC Transport Parameters wire.parameters无效")
        }
    }
    let required_tp = [1_u64, 3, 4, 5, 6, 7, 8, 9, 17];
    if required_tp.iter().any(|id| !tp_ids.contains(id)) {
        bail!("QUIC Transport Parameters缺少浏览器必需参数")
    }
    let actual_order = profile
        .http
        .headers
        .iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    let declared_order = profile.http.header_order.iter().collect::<Vec<_>>();
    if actual_order != declared_order {
        bail!("http3.http.header_order与headers顺序不一致")
    }
    let mut pseudo_headers = HashSet::new();
    let mut regular_seen = false;
    for (name, value) in &profile.http.headers {
        validate_header(name, value)?;
        if name.starts_with(':') {
            if regular_seen
                || !matches!(
                    name.as_str(),
                    ":method" | ":authority" | ":scheme" | ":path"
                )
                || !pseudo_headers.insert(name.as_str())
            {
                bail!("HTTP/3 template contains invalid or misplaced pseudo-headers")
            }
        } else {
            regular_seen = true;
        }
    }
    if pseudo_headers.len() != 4 {
        bail!("HTTP/3 template is missing required pseudo-headers")
    }
    let mut setting_ids = HashSet::new();
    if profile
        .http
        .settings
        .iter()
        .any(|(id, _)| !setting_ids.insert(*id))
    {
        bail!("HTTP/3 SETTINGS包含重复ID")
    }
    if !setting_ids.contains(&1) || !setting_ids.contains(&6) || !setting_ids.contains(&7) {
        bail!("HTTP/3 SETTINGS缺少QPACK或字段上限")
    }
    validate_settings(profile)?;
    if !profile.http.qpack.raw_capture {
        bail!("HTTP/3 QPACK必须包含原始捕获")
    }
    let qpack = decode_wire(
        &profile.http.qpack.header_block_base64,
        "HTTP/3 QPACK header block",
        profile.http.qpack.header_block_length,
    )?;
    verify_sha256(
        &qpack,
        &profile.http.qpack.header_block_sha256,
        "HTTP/3 QPACK header block",
    )?;
    Ok(())
}

fn cipher_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x1301 => "TLS_AES_128_GCM_SHA256",
        0x1302 => "TLS_AES_256_GCM_SHA384",
        0x1303 => "TLS_CHACHA20_POLY1305_SHA256",
        _ => return None,
    })
}

fn group_name(id: u16) -> Option<&'static str> {
    Some(match id {
        23 => "P-256",
        24 => "P-384",
        29 => "X25519",
        4588 => "X25519MLKEM768",
        _ => return None,
    })
}

fn signature_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0403 => "ecdsa_secp256r1_sha256",
        0x0804 => "rsa_pss_rsae_sha256",
        0x0401 => "rsa_pkcs1_sha256",
        0x0503 => "ecdsa_secp384r1_sha384",
        0x0805 => "rsa_pss_rsae_sha384",
        0x0501 => "rsa_pkcs1_sha384",
        0x0806 => "rsa_pss_rsae_sha512",
        0x0601 => "rsa_pkcs1_sha512",
        0x0201 => "rsa_pkcs1_sha1",
        _ => return None,
    })
}

fn extension_type(id: u16) -> Option<ExtensionType> {
    Some(match id {
        0 => ExtensionType::SERVER_NAME,
        10 => ExtensionType::SUPPORTED_GROUPS,
        13 => ExtensionType::SIGNATURE_ALGORITHMS,
        16 => ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION,
        27 => ExtensionType::CERT_COMPRESSION,
        43 => ExtensionType::SUPPORTED_VERSIONS,
        45 => ExtensionType::PSK_KEY_EXCHANGE_MODES,
        51 => ExtensionType::KEY_SHARE,
        57 => ExtensionType::QUIC_TRANSPORT_PARAMETERS_STANDARD,
        0xca34 => ExtensionType::TRUST_ANCHORS,
        17613 => ExtensionType::APPLICATION_SETTINGS,
        0xfe0d => ExtensionType::ENCRYPTED_CLIENT_HELLO,
        _ if is_grease(id) => return None,
        _ => return None,
    })
}

#[derive(Clone)]
struct BrotliCert;
impl CertificateCompressor for BrotliCert {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::BROTLI;
    const CAN_COMPRESS: bool = true;
    const CAN_DECOMPRESS: bool = true;
    fn compress<W: Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        let mut encoder = brotli::CompressorWriter::new(output, 4096, 11, 22);
        encoder.write_all(input)
    }
    fn decompress<W: Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        let mut decoder = brotli::Decompressor::new(input, 4096);
        std::io::copy(&mut decoder, output).map(|_| ())
    }
}

fn tls_builder(tls: &H3TlsCapture, verify: bool) -> Result<SslContextBuilder> {
    let mut builder = SslContextBuilder::new(SslMethod::tls())?;
    builder.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    builder.set_max_proto_version(Some(SslVersion::TLS1_3))?;
    builder.set_preserve_tls13_cipher_list(true);
    let mut ciphers = tls
        .cipher_suites
        .iter()
        .filter_map(|id| cipher_name(*id))
        .collect::<Vec<_>>();
    // BoringSSL's legacy cipher-list parser requires at least one TLS 1.2
    // cipher even for a QUIC/TLS 1.3-only handshake. QUIC filters it from the
    // emitted ClientHello.
    ciphers.push("TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256");
    builder.set_cipher_list(&ciphers.join(":"))?;
    builder.set_curves_list(
        &tls.supported_groups
            .iter()
            .filter_map(|id| group_name(*id))
            .collect::<Vec<_>>()
            .join(":"),
    )?;
    builder.set_sigalgs_list(
        &tls.signature_algorithms
            .iter()
            .filter_map(|id| signature_name(*id))
            .collect::<Vec<_>>()
            .join(":"),
    )?;
    builder.set_alpn_protos(b"\x02h3")?;
    builder.set_grease_enabled(tls.uses_grease);
    builder.set_aes_hw_override(true);
    builder.set_permute_extensions(false);
    let order = tls
        .extensions
        .iter()
        .filter_map(|id| extension_type(*id))
        .collect::<Vec<_>>();
    builder.set_extension_permutation(&order)?;
    if let Some(extension) = tls.extension_wire.iter().find(|item| item.id == 0xca34) {
        let payload = decode_wire(
            &extension.payload_base64,
            "TLS 0xca34 payload",
            extension.length,
        )?;
        if payload.len() < 2
            || usize::from(u16::from_be_bytes([payload[0], payload[1]])) != payload.len() - 2
        {
            bail!("TLS 0xca34 payload列表长度无效")
        }
        builder.set_requested_trust_anchors(&payload[2..])?;
    }
    if tls.certificate_compression_algorithms.contains(&2) {
        builder.add_certificate_compression_algorithm(BrotliCert)?;
    }
    if verify {
        builder.set_verify(SslVerifyMode::PEER);
        // Use the same bundled trust roots as the HTTP/1 and HTTP/2 client.
        // BoringSSL's default filesystem paths are usually absent on Windows.
        static ROOTS: OnceLock<std::result::Result<X509Store, String>> = OnceLock::new();
        let roots = ROOTS.get_or_init(|| {
            (|| -> Result<X509Store> {
                let mut store = X509StoreBuilder::new()?;
                for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
                    store.add_cert(X509::from_der(der.as_ref())?)?;
                }
                Ok(store.build())
            })()
            .map_err(|error| error.to_string())
        });
        builder.set_cert_store(
            roots
                .as_ref()
                .map_err(|error| anyhow::anyhow!(error.clone()))?
                .clone(),
        );
    } else {
        builder.set_verify(SslVerifyMode::NONE);
    }
    Ok(builder)
}

fn configure_peer_identity(ssl: &mut btls::ssl::SslRef, url: &Url) -> Result<()> {
    // SNI is a fingerprint choice, not an authorization policy. Always verify
    // the URL identity, even when the captured ClientHello omitted SNI.
    let param = ssl.verify_param_mut();
    param.set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS);
    match url.host().context("URL缺少host")? {
        Host::Domain(host) => param.set_host(host)?,
        Host::Ipv4(ip) => {
            // IP connections are created without server_name, so there is no
            // DNS constraint to clear. BoringSSL rejects empty host names.
            param.set_ip(ip.into())?;
        }
        Host::Ipv6(ip) => {
            param.set_ip(ip.into())?;
        }
    }
    Ok(())
}

fn configure_quiche(profile: &ProfileHttp3, verify: bool) -> Result<quiche::Config> {
    let mut config = quiche::Config::with_boring_ssl_ctx_builder(
        quiche::PROTOCOL_VERSION,
        tls_builder(&profile.tls, verify)?,
    )?;
    config.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)?;
    config.verify_peer(verify);
    config.grease(profile.tls.uses_grease);
    let tp = &profile.quic.transport_parameters;
    config.set_max_idle_timeout(tp.max_idle_timeout);
    config.set_max_recv_udp_payload_size(tp.max_udp_payload_size);
    config.set_max_send_udp_payload_size(tp.max_udp_payload_size);
    config.set_initial_max_data(tp.initial_max_data);
    config.set_initial_max_stream_data_bidi_local(tp.initial_max_stream_data_bidi_local);
    config.set_initial_max_stream_data_bidi_remote(tp.initial_max_stream_data_bidi_remote);
    config.set_initial_max_stream_data_uni(tp.initial_max_stream_data_uni);
    config.set_initial_max_streams_bidi(tp.initial_max_streams_bidi);
    config.set_initial_max_streams_uni(tp.initial_max_streams_uni);
    config.set_ack_delay_exponent(tp.ack_delay_exponent);
    config.set_max_ack_delay(tp.max_ack_delay);
    config.set_disable_active_migration(tp.disable_active_migration);
    config.set_active_connection_id_limit(tp.active_connection_id_limit);
    if tp.max_datagram_frame_size > 0 {
        config.enable_dgram(true, 32, 32);
    }
    config.set_raw_transport_parameters(transport_parameter_template(tp)?);
    Ok(config)
}

fn configure_h3(profile: &ProfileHttp3) -> Result<quiche::h3::Config> {
    let mut config = quiche::h3::Config::new()?;
    let mut additional = Vec::new();
    for &(id, value) in &profile.http.settings {
        match id {
            1 => config.set_qpack_max_table_capacity(value),
            6 => config.set_max_field_section_size(value),
            7 => config.set_qpack_blocked_streams(value),
            8 => config.enable_extended_connect(value != 0),
            51 => {}
            _ if id >= 33 && (id - 33) % 31 == 0 => {
                let mut rng = rand::rng();
                let grease_id = 33 + 31 * u64::from((rng.next_u32() & 0x07ff_ffff) | 0x0400_0000);
                additional.push((grease_id, u64::from(rng.next_u32() | 0x4000_0000)));
            }
            _ => additional.push((id, value)),
        }
    }
    config.set_additional_settings(additional)?;
    Ok(config)
}

fn encode_varint(value: u64, output: &mut Vec<u8>) -> Result<()> {
    match value {
        0..=63 => output.push(value as u8),
        64..=16_383 => output.extend_from_slice(&((value as u16) | 0x4000).to_be_bytes()),
        16_384..=1_073_741_823 => {
            output.extend_from_slice(&((value as u32) | 0x8000_0000).to_be_bytes())
        }
        1_073_741_824..=4_611_686_018_427_387_903 => {
            output.extend_from_slice(&(value | 0xc000_0000_0000_0000).to_be_bytes())
        }
        _ => bail!("QUIC varint超出范围"),
    }
    Ok(())
}

fn transport_parameter_template(tp: &H3TransportParameters) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(tp.wire.length);
    let mut rng = rand::rng();
    for item in &tp.wire.parameters {
        let reserved = item.id >= 27 && (item.id - 27) % 31 == 0;
        let id = if reserved {
            27 + 31 * ((rng.next_u64() & ((1_u64 << 56) - 1)) | (1_u64 << 55))
        } else {
            item.id
        };
        let mut value = (0..item.value_hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&item.value_hex[index..index + 2], 16))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if reserved {
            rng.fill_bytes(&mut value);
        }
        if item.id == 17 && value.len() >= 12 {
            let grease =
                0x0a0a_0a0a_u32.wrapping_add(0x1010_1010_u32.wrapping_mul(rng.next_u32() & 0x0f));
            let offset = value.len() - 4;
            value[offset..].copy_from_slice(&grease.to_be_bytes());
        }
        encode_varint(id, &mut output)?;
        encode_varint(value.len() as u64, &mut output)?;
        output.extend_from_slice(&value);
    }
    Ok(output)
}

fn is_stable_capture_header(name: &str) -> bool {
    matches!(
        name,
        "user-agent"
            | "sec-ch-ua"
            | "sec-ch-ua-mobile"
            | "sec-ch-ua-platform"
            | "sec-ch-ua-arch"
            | "sec-ch-ua-bitness"
            | "sec-ch-ua-full-version"
            | "sec-ch-ua-full-version-list"
            | "sec-ch-ua-model"
            | "sec-ch-ua-platform-version"
            | "sec-ch-ua-wow64"
            | "accept-encoding"
            | "accept-language"
            | "dnt"
            | "sec-gpc"
            | "te"
    )
}

fn validate_header(name: &str, value: &str) -> Result<()> {
    let token = name.strip_prefix(':').unwrap_or(name);
    if token.is_empty()
        || !token.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&byte)
        })
        || value.bytes().any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
    {
        bail!("HTTP/3 header contains an invalid name or value")
    }
    if matches!(
        name,
        "connection" | "proxy-connection" | "keep-alive" | "transfer-encoding" | "upgrade"
    ) || (name == "te" && !value.eq_ignore_ascii_case("trailers"))
    {
        bail!("HTTP/3 forbids connection-specific header {name}")
    }
    Ok(())
}

fn request_headers(
    profile: &ProfileHttp3,
    request: &H3Request,
    url: &Url,
) -> Result<Vec<quiche::h3::Header>> {
    let mut authority = match url.port() {
        Some(port) if port != 443 => format!("{}:{port}", url.host_str().context("URL缺少host")?),
        _ => url.host_str().context("URL缺少host")?.to_string(),
    };
    let mut used = vec![false; request.headers.len()];
    let mut has_host = false;
    for (index, (name, value)) in request.headers.iter().enumerate() {
        let name = name.to_ascii_lowercase();
        validate_header(&name, value)?;
        if name.starts_with(':') {
            bail!("HTTP/3 request pseudo-headers are derived from the request URL and method")
        }
        if name == "host" {
            if has_host || value.is_empty() {
                bail!("HTTP/3 request must not contain an empty or duplicate Host")
            }
            authority = value.clone();
            used[index] = true;
            has_host = true;
        }
    }
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let mut out = Vec::new();
    for name in &profile.http.header_order {
        let dynamic = match name.as_str() {
            ":method" => Some(request.method.as_str()),
            ":authority" => Some(authority.as_str()),
            ":scheme" => Some(url.scheme()),
            ":path" => Some(path.as_str()),
            _ => None,
        };
        if let Some(value) = dynamic {
            out.push(quiche::h3::Header::new(name.as_bytes(), value.as_bytes()));
            continue;
        }
        if let Some((index, (_, value))) = request
            .headers
            .iter()
            .enumerate()
            .find(|(index, (header, _))| !used[*index] && header.eq_ignore_ascii_case(name))
        {
            used[index] = true;
            out.push(quiche::h3::Header::new(name.as_bytes(), value.as_bytes()));
        } else if is_stable_capture_header(name)
            && let Some((_, value)) = profile
                .http
                .headers
                .iter()
                .find(|(header, _)| header == name)
        {
            // A fingerprint is not a request template. Never replay captured
            // credentials, cookies, navigation context or entity framing.
            out.push(quiche::h3::Header::new(name.as_bytes(), value.as_bytes()));
        }
    }
    for (index, (name, value)) in request.headers.iter().enumerate() {
        if !used[index] {
            out.push(quiche::h3::Header::new(
                name.to_ascii_lowercase().as_bytes(),
                value.as_bytes(),
            ));
        }
    }
    Ok(out)
}

fn receive_response_headers(
    list: Vec<quiche::h3::Header>,
    status: &mut Option<u16>,
    headers: &mut Vec<(String, String)>,
    trailers_received: &mut bool,
) -> Result<()> {
    if *trailers_received {
        bail!("HTTP/3 response contains multiple trailer sections")
    }
    let mut block_status = None;
    let mut block_headers = Vec::new();
    let mut regular_seen = false;
    for header in list {
        let name = std::str::from_utf8(header.name()).context("HTTP/3 invalid header name")?;
        let value = std::str::from_utf8(header.value()).context("HTTP/3 invalid header value")?;
        validate_header(name, value)?;
        if name.starts_with(':') {
            if name != ":status" || regular_seen || block_status.is_some() || status.is_some() {
                bail!("HTTP/3 response contains an invalid pseudo-header")
            }
            if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("HTTP/3 invalid :status")
            }
            let code: u16 = value.parse()?;
            if !(100..=599).contains(&code) || code == 101 {
                bail!("HTTP/3 invalid :status")
            }
            block_status = Some(code);
        } else {
            regular_seen = true;
            block_headers.push((name.to_string(), value.to_string()));
        }
    }
    if status.is_some() {
        // Trailers are not ordinary response headers. In particular they must
        // not change decompression, framing, redirect or cookie processing.
        *trailers_received = true;
    } else {
        let code = block_status.context("HTTP/3响应缺少:status")?;
        if code >= 200 {
            *status = Some(code);
            *headers = block_headers;
        }
        // Informational fields apply only to that interim response.
    }
    Ok(())
}

#[cfg(test)]
fn finish_response_body(
    method: &str,
    status: u16,
    headers: &mut Vec<(String, String)>,
    body: Vec<u8>,
    limit: usize,
) -> Result<Vec<u8>> {
    finish_response_body_controlled(
        method,
        status,
        headers,
        body,
        limit,
        &H3Control::new(Instant::now(), Duration::MAX),
    )
}

fn finish_response_body_controlled(
    method: &str,
    status: u16,
    headers: &mut Vec<(String, String)>,
    body: Vec<u8>,
    limit: usize,
    control: &H3Control,
) -> Result<Vec<u8>> {
    control.check()?;
    if method == "HEAD" || matches!(status, 204 | 304) {
        if !body.is_empty() {
            bail!("HTTP/3 response must not contain a body for this method/status")
        }
        // Content-Encoding/Length in HEAD or 304 describes a representation
        // that was not transferred; attempting gzip decompression would fail.
        return Ok(body);
    }
    let mut expected_length = None;
    for value in headers
        .iter()
        .filter(|(name, _)| name == "content-length")
        .flat_map(|(_, value)| value.split(','))
    {
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("HTTP/3 invalid Content-Length")
        }
        let length = value
            .parse::<u64>()
            .context("HTTP/3 invalid Content-Length")?;
        if expected_length.is_some_and(|previous| previous != length) {
            bail!("HTTP/3 conflicting Content-Length values")
        }
        expected_length = Some(length);
    }
    if expected_length.is_some_and(|expected| expected != body.len() as u64) {
        bail!("HTTP/3 response body disagrees with Content-Length")
    }
    decode_response_body(headers, body, limit, control)
}

fn request_origin(url: &Url) -> Result<String> {
    let host = url.host_str().context("URL缺少host")?;
    Ok(format!(
        "{}://{}:{}",
        url.scheme(),
        host.to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or(443)
    ))
}

fn read_limited(mut reader: impl Read, limit: usize, control: &H3Control) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        control.check()?;
        let available = chunk
            .len()
            .min(limit.saturating_sub(output.len()).saturating_add(1));
        let len = reader.read(&mut chunk[..available])?;
        control.check()?;
        if len == 0 {
            break;
        }
        if len > limit.saturating_sub(output.len()) {
            bail!("解压后的响应Body超过max_response_bytes限制")
        }
        output.extend_from_slice(&chunk[..len]);
    }
    Ok(output)
}

fn decode_response_body(
    headers: &mut Vec<(String, String)>,
    mut body: Vec<u8>,
    limit: usize,
    control: &H3Control,
) -> Result<Vec<u8>> {
    control.check()?;
    let encodings = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-encoding"))
        .flat_map(|(_, value)| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "identity")
        .collect::<Vec<_>>();
    for encoding in encodings.iter().rev() {
        body = match encoding.as_str() {
            "gzip" | "x-gzip" => read_limited(GzDecoder::new(body.as_slice()), limit, control)?,
            "br" => read_limited(
                brotli::Decompressor::new(body.as_slice(), 4096),
                limit,
                control,
            )?,
            "zstd" => read_limited(zstd::stream::Decoder::new(body.as_slice())?, limit, control)?,
            "deflate" => match read_limited(ZlibDecoder::new(body.as_slice()), limit, control) {
                Ok(decoded) => decoded,
                Err(_) => read_limited(DeflateDecoder::new(body.as_slice()), limit, control)?,
            },
            other => bail!("不支持的HTTP/3 Content-Encoding: {other}"),
        };
    }
    if encodings.is_empty() {
        if body.len() > limit {
            bail!("响应Body超过max_response_bytes限制")
        }
    } else {
        headers.retain(|(name, _)| {
            !name.eq_ignore_ascii_case("content-encoding")
                && !name.eq_ignore_ascii_case("content-length")
        });
    }
    Ok(body)
}

impl H3Client {
    pub(crate) fn connect(
        profile: ProfileHttp3,
        request: &H3Request,
        control: &H3Control,
    ) -> Result<Self> {
        control.check()?;
        validate(&profile)?;
        let url = Url::parse(&request.url)?;
        if url.scheme() != "https" {
            bail!("HTTP/3只支持https URL")
        }
        let peer = match request.peer {
            Some(peer) => peer,
            // Resolve on the async side before entering a blocking worker.
            None => bail!("HTTP/3 peer must be resolved before connection setup"),
        };
        control.check()?;
        let bind: SocketAddr = if peer.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        }
        .parse()?;
        let socket = UdpSocket::bind(bind)?;
        socket.set_nonblocking(true)?;
        let local = socket.local_addr()?;
        let mut config = configure_quiche(&profile, request.verify)?;
        control.check()?;
        let mut scid_bytes = [0_u8; quiche::MAX_CONN_ID_LEN];
        rand::rng().fill_bytes(&mut scid_bytes);
        let mut dcid_bytes = [0_u8; 8];
        rand::rng().fill_bytes(&mut dcid_bytes);
        let scid = if profile
            .quic
            .transport_parameters
            .wire
            .parameters
            .iter()
            .any(|item| item.id == 15 && item.length == 0)
        {
            quiche::ConnectionId::from_ref(&[])
        } else {
            quiche::ConnectionId::from_ref(&scid_bytes)
        };
        let server_name = match url.host().context("URL缺少host")? {
            Host::Domain(host) if profile.tls.extensions.contains(&0) => Some(host),
            _ => None,
        };
        let dcid = quiche::ConnectionId::from_ref(&dcid_bytes);
        let mut conn =
            quiche::connect_with_dcid(server_name, &scid, &dcid, local, peer, &mut config)?;
        {
            let ssl: &mut btls::ssl::SslRef = conn.as_mut();
            if request.verify {
                configure_peer_identity(ssl, &url)?;
            }
            let shares = profile
                .tls
                .key_share_groups
                .iter()
                .filter_map(|id| match id {
                    23 => Some(KeyShare::P256),
                    24 => Some(KeyShare::P384),
                    29 => Some(KeyShare::X25519),
                    4588 => Some(KeyShare::X25519_MLKEM768),
                    _ => None,
                })
                .collect::<Vec<_>>();
            ssl.set_client_key_shares(&shares)?;
            ssl.set_enable_ech_grease(profile.tls.has_ech);
            if profile.tls.extensions.contains(&17613) {
                ssl.add_application_settings(b"h3")?;
                ssl.set_alps_use_new_codepoint(true);
            }
            ssl.set_aes_hw_override(true);
        }
        let h3_config = configure_h3(&profile)?;
        let out = vec![
            0_u8;
            profile
                .quic
                .transport_parameters
                .max_udp_payload_size
                .max(1350)
        ];
        control.check()?;
        Ok(Self {
            origin: request_origin(&url)?,
            profile,
            socket,
            local,
            conn,
            h3_config,
            h3: None,
            out,
            input: vec![0_u8; 65_535],
        })
    }

    pub(crate) fn is_reusable(&self) -> bool {
        !self.conn.is_closed() && !self.conn.timeout().is_some_and(|timeout| timeout.is_zero())
    }

    pub(crate) fn roundtrip(
        &mut self,
        request: H3Request,
        control: &H3Control,
    ) -> std::result::Result<H3Response, H3RequestError> {
        let response_received = Cell::new(false);
        let result = (|| -> Result<H3Response> {
            control.check()?;
            let url = Url::parse(&request.url)?;
            if request_origin(&url)? != self.origin {
                bail!("HTTP/3连接不能跨origin复用")
            }
            if self.conn.is_closed() {
                bail!("缓存的QUIC连接已经关闭")
            }
            let headers = request_headers(&self.profile, &request, &url)?;
            let started = Instant::now();
            let mut sent = false;
            let mut body_offset = 0usize;
            let mut stream_id = None;
            let mut status = None;
            let mut response_headers = Vec::new();
            let mut trailers_received = false;
            let mut response_body = Vec::new();
            let mut finished = false;
            let mut received_datagrams = 0usize;
            while !finished {
                control.check()?;
                if started.elapsed() >= request.timeout {
                    bail!(
                        "HTTP/3请求超时(received_datagrams={received_datagrams}, status={status:?}, body_bytes={})",
                        response_body.len()
                    )
                }
                loop {
                    control.check()?;
                    match self.socket.recv_from(&mut self.input) {
                        Ok((len, from)) => {
                            received_datagrams += 1;
                            match self.conn.recv(
                                &mut self.input[..len],
                                quiche::RecvInfo {
                                    to: self.local,
                                    from,
                                },
                            ) {
                                Ok(_) | Err(quiche::Error::Done) => {}
                                Err(error) => bail!("QUIC接收失败: {error:?}"),
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) => return Err(error.into()),
                    }
                }
                if self.conn.timeout().is_some_and(|timeout| timeout.is_zero()) {
                    self.conn.on_timeout();
                }
                if self.conn.is_established() && self.h3.is_none() {
                    self.h3 = Some(quiche::h3::Connection::with_transport(
                        &mut self.conn,
                        &self.h3_config,
                    )?);
                }
                if let Some(h3_conn) = self.h3.as_mut() {
                    if !sent {
                        let has_body = request.body.as_ref().is_some_and(|body| !body.is_empty());
                        stream_id =
                            Some(h3_conn.send_request(&mut self.conn, &headers, !has_body)?);
                        sent = true;
                    }
                    if let (Some(id), Some(body)) = (stream_id, request.body.as_deref())
                        && body_offset < body.len()
                    {
                        match h3_conn.send_body(&mut self.conn, id, &body[body_offset..], true) {
                            Ok(written) => body_offset += written,
                            Err(quiche::h3::Error::Done) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    loop {
                        control.check()?;
                        match h3_conn.poll(&mut self.conn) {
                            Ok((id, quiche::h3::Event::Headers { list, .. }))
                                if Some(id) == stream_id =>
                            {
                                response_received.set(true);
                                receive_response_headers(
                                    list,
                                    &mut status,
                                    &mut response_headers,
                                    &mut trailers_received,
                                )?;
                            }
                            Ok((id, quiche::h3::Event::Data)) if Some(id) == stream_id => {
                                if status.is_none() || trailers_received {
                                    bail!("HTTP/3 response DATA outside the final response body")
                                }
                                loop {
                                    control.check()?;
                                    match h3_conn.recv_body(&mut self.conn, id, &mut self.input) {
                                        Ok(len) => {
                                            if response_body.len().saturating_add(len)
                                                > request.max_response_bytes
                                            {
                                                bail!("响应Body超过max_response_bytes限制")
                                            }
                                            response_body.extend_from_slice(&self.input[..len]);
                                        }
                                        Err(quiche::h3::Error::Done) => break,
                                        Err(error) => return Err(error.into()),
                                    }
                                }
                                self.conn.send_ack_eliciting().ok();
                            }
                            Ok((id, quiche::h3::Event::Finished)) if Some(id) == stream_id => {
                                finished = true;
                                break;
                            }
                            Ok((id, quiche::h3::Event::Reset(code))) if Some(id) == stream_id => {
                                bail!("HTTP/3请求流被重置: {code}")
                            }
                            Ok(_) => {}
                            Err(quiche::h3::Error::Done) => break,
                            Err(quiche::h3::Error::QpackDecompressionFailed) => {
                                bail!(
                                    "HTTP/3 QPACK解码失败：头块、动态表引用或阻塞流数量违反协议约束"
                                )
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
                loop {
                    control.check()?;
                    match self.conn.send(&mut self.out) {
                        Ok((len, info)) => {
                            self.socket.send_to(&self.out[..len], info.to)?;
                        }
                        Err(quiche::Error::Done) => break,
                        Err(error) => return Err(error.into()),
                    }
                }
                if self.conn.is_closed() && !finished {
                    bail!("QUIC连接在响应完成前关闭")
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let status = status.context("HTTP/3响应缺少最终:status")?;
            let body = finish_response_body_controlled(
                &request.method,
                status,
                &mut response_headers,
                response_body,
                request.max_response_bytes,
                control,
            )?;
            Ok(H3Response {
                status,
                headers: response_headers,
                body,
            })
        })();
        result.map_err(|error| H3RequestError {
            error,
            response_received: response_received.get(),
        })
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    fn profile() -> ProfileHttp3 {
        let records: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/chrome152_schema2.json")).unwrap();
        serde_json::from_value(records[0]["http3"].clone()).unwrap()
    }

    #[test]
    fn running_quic_roundtrip_observes_cancel_after_first_datagram() {
        let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
        sink.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let peer = sink.local_addr().unwrap();
        let control = H3Control::new(Instant::now(), Duration::from_secs(30));
        let worker_control = control.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let request = H3Request {
                method: "POST".into(),
                url: format!("https://{peer}/"),
                headers: Vec::new(),
                body: Some(b"no-replay".to_vec()),
                timeout: Duration::from_secs(30),
                verify: false,
                max_response_bytes: 1024,
                peer: Some(peer),
            };
            let result =
                H3Client::connect(profile(), &request, &worker_control).and_then(|mut client| {
                    client
                        .roundtrip(request, &worker_control)
                        .map_err(|e| e.error)
                });
            tx.send(result.err().map(|error| error.to_string()))
                .unwrap();
        });
        let received = sink.recv_from(&mut [0_u8; 1500]);
        // Cancel even on fixture failure so the test never leaves a 30s worker.
        control.cancel();
        assert!(
            received.is_ok(),
            "worker did not send QUIC Initial: {received:?}"
        );
        let error = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("running worker must stop, not wait for its original deadline")
            .expect("cancelled request cannot succeed");
        worker.join().unwrap();
        assert!(error.contains("cancelled"), "{error}");
    }

    #[test]
    fn decoded_chunk_is_not_returned_after_cancellation_or_deadline() {
        struct CancelReader(H3Control);
        impl Read for CancelReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.0.cancel();
                buf[0] = 1;
                Ok(1)
            }
        }
        let control = H3Control::new(Instant::now(), Duration::from_secs(2));
        let error = read_limited(CancelReader(control.clone()), 32, &control).unwrap_err();
        assert!(error.to_string().contains("cancelled"));

        struct SlowReader;
        impl Read for SlowReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                std::thread::sleep(Duration::from_millis(5));
                buf[0] = 1;
                Ok(1)
            }
        }
        let control = H3Control::new(Instant::now(), Duration::from_millis(1));
        assert!(
            read_limited(SlowReader, 32, &control)
                .unwrap_err()
                .to_string()
                .contains("timeout")
        );
    }

    #[test]
    fn quic_setup_uses_original_budget_and_rejects_pre_cancelled_work() {
        let request = H3Request {
            method: "GET".into(),
            url: "https://127.0.0.1:1/".into(),
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(30),
            verify: false,
            max_response_bytes: 1024,
            peer: Some("127.0.0.1:1".parse().unwrap()),
        };
        let expired = H3Control::new(
            Instant::now() - Duration::from_secs(1),
            Duration::from_millis(1),
        );
        assert!(
            H3Client::connect(profile(), &request, &expired)
                .err()
                .unwrap()
                .to_string()
                .contains("timeout")
        );
        let cancelled = H3Control::new(Instant::now(), Duration::from_secs(30));
        cancelled.cancel();
        assert!(
            H3Client::connect(profile(), &request, &cancelled)
                .err()
                .unwrap()
                .to_string()
                .contains("cancelled")
        );
    }

    #[test]
    fn peer_identity_is_verified_without_sni_for_dns_and_ip() {
        use btls::{
            asn1::Asn1Time,
            bn::BigNum,
            hash::MessageDigest,
            pkey::PKey,
            rsa::Rsa,
            ssl::Ssl,
            stack::Stack,
            x509::{
                X509Name, X509StoreContext,
                extension::{BasicConstraints, SubjectAlternativeName},
            },
        };

        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509Name::builder().unwrap();
        name.append_entry_by_text("CN", "example.test").unwrap();
        let name = name.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap())
            .unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert.append_extension(&BasicConstraints::new().critical().ca().build().unwrap())
            .unwrap();
        let san = SubjectAlternativeName::new()
            .dns("example.test")
            .ip("127.0.0.1")
            .ip("::1")
            .build(&cert.x509v3_context(None, None))
            .unwrap();
        cert.append_extension(&san).unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let cert = cert.build();
        for (url, expected) in [
            ("https://example.test/", true),
            ("https://other.test/", false),
            ("https://127.0.0.1/", true),
            ("https://127.0.0.2/", false),
            ("https://[::1]/", true),
            ("https://[::2]/", false),
        ] {
            let context = SslContextBuilder::new(SslMethod::tls()).unwrap().build();
            let mut ssl = Ssl::new(&context).unwrap();
            configure_peer_identity(&mut ssl, &Url::parse(url).unwrap()).unwrap();
            let mut store = X509StoreBuilder::new().unwrap();
            store.add_cert(&cert).unwrap();
            store.set_param(ssl.verify_param_mut()).unwrap();
            let chain = Stack::new().unwrap();
            let verified = X509StoreContext::new()
                .unwrap()
                .init(&store.build(), &cert, &chain, |ctx| ctx.verify_cert())
                .unwrap();
            assert_eq!(verified, expected, "{url}");
        }
    }

    #[test]
    fn h3_verified_context_loads_bundled_roots() {
        tls_builder(&profile().tls, true).unwrap();
    }

    #[test]
    fn ipv6_literal_resolves_without_dns_or_brackets() {
        let addresses = Url::parse("https://[::1]:8443/")
            .unwrap()
            .socket_addrs(|| Some(443))
            .unwrap();
        assert_eq!(addresses, vec!["[::1]:8443".parse::<SocketAddr>().unwrap()]);
    }

    fn request() -> H3Request {
        H3Request {
            method: "GET".into(),
            url: "https://example.test/path".into(),
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(1),
            verify: false,
            max_response_bytes: 1024,
            peer: None,
        }
    }

    fn header_list(values: &[(&str, &str)]) -> Vec<quiche::h3::Header> {
        values
            .iter()
            .map(|(name, value)| quiche::h3::Header::new(name.as_bytes(), value.as_bytes()))
            .collect()
    }

    #[test]
    fn captured_credentials_and_request_context_are_not_replayed() {
        let mut template = profile();
        for name in [
            "cookie",
            "authorization",
            "x-api-key",
            "content-length",
            "host",
        ] {
            template.http.header_order.push(name.into());
            template
                .http
                .headers
                .push((name.into(), "captured-secret".into()));
        }
        let mut request = request();
        request
            .headers
            .push(("Cookie".into(), "explicit=yes".into()));
        request.headers.push(("Host".into(), "virtual.test".into()));
        let headers =
            request_headers(&template, &request, &Url::parse(&request.url).unwrap()).unwrap();
        assert!(headers.iter().any(|header| header.name() == b"user-agent"));
        assert!(
            headers
                .iter()
                .any(|header| header.name() == b"cookie" && header.value() == b"explicit=yes")
        );
        assert!(
            headers
                .iter()
                .any(|header| header.name() == b":authority" && header.value() == b"virtual.test")
        );
        assert!(!headers.iter().any(|header| matches!(
            header.name(),
            b"origin" | b"referer" | b"authorization" | b"x-api-key" | b"content-length" | b"host"
        )));
    }

    #[test]
    fn request_rejects_connection_specific_headers() {
        let mut request = request();
        request
            .headers
            .push(("Connection".into(), "keep-alive".into()));
        assert!(request_headers(&profile(), &request, &Url::parse(&request.url).unwrap()).is_err());
    }

    #[test]
    fn template_rejects_duplicate_or_late_pseudo_headers() {
        for name in [":status", ":path"] {
            let mut template = profile();
            template.http.headers.push((name.into(), "200".into()));
            template.http.header_order.push(name.into());
            assert!(validate(&template).is_err());
        }
    }

    #[test]
    fn informational_and_trailer_fields_do_not_modify_final_response() {
        let mut status = None;
        let mut headers = Vec::new();
        let mut trailers = false;
        receive_response_headers(
            header_list(&[
                (":status", "103"),
                ("content-encoding", "gzip"),
                ("set-cookie", "interim=1"),
            ]),
            &mut status,
            &mut headers,
            &mut trailers,
        )
        .unwrap();
        assert_eq!(status, None);
        assert!(headers.is_empty());
        receive_response_headers(
            header_list(&[(":status", "200"), ("content-length", "2")]),
            &mut status,
            &mut headers,
            &mut trailers,
        )
        .unwrap();
        receive_response_headers(
            header_list(&[("content-encoding", "gzip"), ("set-cookie", "trailer=1")]),
            &mut status,
            &mut headers,
            &mut trailers,
        )
        .unwrap();
        assert!(trailers);
        assert_eq!(status, Some(200));
        assert_eq!(headers, vec![("content-length".into(), "2".into())]);
        assert_eq!(
            finish_response_body("GET", 200, &mut headers, b"ok".to_vec(), 10).unwrap(),
            b"ok"
        );
    }

    #[test]
    fn invalid_response_status_and_pseudo_headers_are_rejected() {
        for values in [
            vec![(":status", "101")],
            vec![(":status", "99")],
            vec![(":status", "600")],
            vec![(":status", "200"), (":status", "201")],
            vec![("x-before", "1"), (":status", "200")],
            vec![(":status", "200"), ("connection", "close")],
        ] {
            assert!(
                receive_response_headers(
                    header_list(&values),
                    &mut None,
                    &mut Vec::new(),
                    &mut false
                )
                .is_err()
            );
        }
    }

    #[test]
    fn head_and_304_do_not_decompress_missing_representation() {
        for (method, status) in [("HEAD", 200), ("GET", 304), ("GET", 204)] {
            let mut headers = vec![
                ("content-encoding".into(), "gzip".into()),
                ("content-length".into(), "999".into()),
            ];
            assert!(
                finish_response_body(method, status, &mut headers, Vec::new(), 1)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(headers.len(), 2);
            assert!(finish_response_body(method, status, &mut headers, vec![1], 1).is_err());
        }
    }

    #[test]
    fn response_content_length_is_checked_before_decompression() {
        for value in ["3", "2, 3", "-1", "invalid"] {
            let mut headers = vec![("content-length".into(), value.into())];
            assert!(finish_response_body("GET", 200, &mut headers, b"ok".to_vec(), 10).is_err());
        }
        let mut headers = vec![("content-length".into(), "2, 2".into())];
        assert_eq!(
            finish_response_body("GET", 200, &mut headers, b"ok".to_vec(), 10).unwrap(),
            b"ok"
        );
    }

    #[test]
    fn settings_reject_varint_overflow_and_non_reproducible_boolean() {
        for (id, value) in [(1, u64::MAX), (6, 1_u64 << 62), (8, 0), (8, 2), (51, 2)] {
            let mut template = profile();
            if let Some(setting) = template
                .http
                .settings
                .iter_mut()
                .find(|(key, _)| *key == id)
            {
                setting.1 = value;
            } else {
                template.http.settings.insert(3, (id, value));
            }
            assert!(validate(&template).is_err(), "setting {id}={value}");
        }
    }

    #[test]
    fn transport_parameters_reject_runtime_bounds_before_wire_replay() {
        let original = profile().quic.transport_parameters;
        let raw = BASE64.decode(&original.wire.base64).unwrap();
        for field in 0..6 {
            let mut tp = original.clone();
            match field {
                0 => tp.max_udp_payload_size = usize::MAX,
                1 => tp.initial_max_streams_bidi = (1_u64 << 60) + 1,
                2 => tp.initial_max_streams_uni = (1_u64 << 60) + 1,
                3 => tp.ack_delay_exponent = 21,
                4 => tp.max_ack_delay = 1_u64 << 14,
                5 => tp.initial_max_data = u64::MAX,
                _ => unreachable!(),
            }
            let error = validate_transport_parameters(&tp, &raw).unwrap_err();
            assert!(error.to_string().contains("numeric bounds"), "{error}");
        }
    }

    #[test]
    fn oversized_wire_item_length_is_an_error_not_overflow() {
        let mut tp = profile().quic.transport_parameters;
        let raw = BASE64.decode(&tp.wire.base64).unwrap();
        tp.wire.parameters[0].length = usize::MAX;
        assert!(validate_transport_parameters(&tp, &raw).is_err());
    }
}
