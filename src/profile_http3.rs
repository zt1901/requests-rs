use std::{
    cell::Cell,
    collections::HashSet,
    io::{Read, Write},
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use btls::ssl::{
    CertificateCompressionAlgorithm, CertificateCompressor, ExtensionType, KeyShare,
    SslContextBuilder, SslMethod, SslVerifyMode, SslVersion,
};
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use quiche::h3::NameValue;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

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
    let mut offset = 0;
    let mut seen = HashSet::new();
    for item in &tp.wire.parameters {
        if !seen.insert(item.id) || item.value_hex.len() != item.length * 2 {
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
        decode_wire(&item.payload_base64, "http3 TLS扩展payload", item.length)?;
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
            || item.value_hex.len() != item.length * 2
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
    for required in [":method", ":authority", ":scheme", ":path"] {
        if !profile
            .http
            .header_order
            .iter()
            .any(|name| name == required)
        {
            bail!("http3.http.header_order缺少{required}")
        }
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
        builder.set_default_verify_paths()?;
    } else {
        builder.set_verify(SslVerifyMode::NONE);
    }
    Ok(builder)
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

fn request_headers(
    profile: &ProfileHttp3,
    request: &H3Request,
    url: &Url,
) -> Result<Vec<quiche::h3::Header>> {
    let authority = match url.port() {
        Some(port) if port != 443 => format!("{}:{port}", url.host_str().context("URL缺少host")?),
        _ => url.host_str().context("URL缺少host")?.to_string(),
    };
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let mut used = vec![false; request.headers.len()];
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
        } else if let Some((_, value)) = profile
            .http
            .headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
        {
            out.push(quiche::h3::Header::new(name.as_bytes(), value.as_bytes()));
        }
    }
    for (index, (name, value)) in request.headers.iter().enumerate() {
        if !used[index] && !name.starts_with(':') {
            out.push(quiche::h3::Header::new(name.as_bytes(), value.as_bytes()));
        }
    }
    Ok(out)
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

fn read_limited(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    if limit == usize::MAX {
        reader.take(u64::MAX).read_to_end(&mut output)?;
    } else {
        reader.take(limit as u64 + 1).read_to_end(&mut output)?;
        if output.len() > limit {
            bail!("解压后的响应Body超过max_response_bytes限制")
        }
    }
    Ok(output)
}

fn decode_response_body(
    headers: &mut Vec<(String, String)>,
    mut body: Vec<u8>,
    limit: usize,
) -> Result<Vec<u8>> {
    let encodings = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-encoding"))
        .flat_map(|(_, value)| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "identity")
        .collect::<Vec<_>>();
    for encoding in encodings.iter().rev() {
        body = match encoding.as_str() {
            "gzip" | "x-gzip" => read_limited(GzDecoder::new(body.as_slice()), limit)?,
            "br" => read_limited(brotli::Decompressor::new(body.as_slice(), 4096), limit)?,
            "zstd" => read_limited(zstd::stream::Decoder::new(body.as_slice())?, limit)?,
            "deflate" => match read_limited(ZlibDecoder::new(body.as_slice()), limit) {
                Ok(decoded) => decoded,
                Err(_) => read_limited(DeflateDecoder::new(body.as_slice()), limit)?,
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
    pub(crate) fn connect(profile: ProfileHttp3, request: &H3Request) -> Result<Self> {
        validate(&profile)?;
        let url = Url::parse(&request.url)?;
        if url.scheme() != "https" {
            bail!("HTTP/3只支持https URL")
        }
        let host = url.host_str().context("URL缺少host")?;
        let peer = match request.peer {
            Some(peer) => peer,
            None => (host, url.port_or_known_default().unwrap_or(443))
                .to_socket_addrs()?
                .next()
                .context("DNS没有返回地址")?,
        };
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
        let server_name = profile.tls.extensions.contains(&0).then_some(host);
        let dcid = quiche::ConnectionId::from_ref(&dcid_bytes);
        let mut conn =
            quiche::connect_with_dcid(server_name, &scid, &dcid, local, peer, &mut config)?;
        {
            let ssl: &mut btls::ssl::SslRef = conn.as_mut();
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
    ) -> std::result::Result<H3Response, H3RequestError> {
        let response_received = Cell::new(false);
        let result = (|| -> Result<H3Response> {
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
            let mut response_body = Vec::new();
            let mut finished = false;
            let mut received_datagrams = 0usize;
            while !finished {
                if started.elapsed() >= request.timeout {
                    bail!(
                        "HTTP/3请求超时(received_datagrams={received_datagrams}, status={status:?}, body_bytes={})",
                        response_body.len()
                    )
                }
                loop {
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
                        match h3_conn.poll(&mut self.conn) {
                            Ok((id, quiche::h3::Event::Headers { list, .. }))
                                if Some(id) == stream_id =>
                            {
                                response_received.set(true);
                                for header in list {
                                    let name = String::from_utf8_lossy(header.name()).into_owned();
                                    let value =
                                        String::from_utf8_lossy(header.value()).into_owned();
                                    if name == ":status" {
                                        status = Some(value.parse()?);
                                    } else {
                                        response_headers.push((name, value));
                                    }
                                }
                            }
                            Ok((id, quiche::h3::Event::Data)) if Some(id) == stream_id => {
                                loop {
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
                            Err(error) => return Err(error.into()),
                        }
                    }
                }
                loop {
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
            let body = decode_response_body(
                &mut response_headers,
                response_body,
                request.max_response_bytes,
            )?;
            Ok(H3Response {
                status: status.context("HTTP/3响应缺少:status")?,
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
