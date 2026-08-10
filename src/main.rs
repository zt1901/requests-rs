use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use brotli::{CompressorWriter as BrotliEncoder, Decompressor as BrotliDecoder};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wreq::{
    Client, Emulation,
    header::{HeaderMap, HeaderName, HeaderValue, OrigHeaderMap},
    http2::{
        Http2Options, Priorities, Priority, PseudoId, PseudoOrder, SettingId, SettingsOrder,
        StreamDependency, StreamId,
    },
    tls::{
        AlpnProtocol, ExtensionType, KeyShare, TlsOptions, TlsVersion,
        compress::{CertificateCompressionAlgorithm, CertificateCompressor, Codec},
        session::LruTlsSessionCache,
        trust::CertStore,
    },
};

// 这些参数可直接修改；相对路径以本项目目录为基准。
const 记录文件: &str = "../fingerprint_records.json";
const 记录编号: &str = "latest";
const 目标地址: &str = "https://127.0.0.1:8443/api/fingerprint?source=wreq";
const 本地自签证书: bool = true;
const 请求超时秒数: u64 = 20;

#[derive(Debug, Deserialize)]
struct Record {
    id: String,
    tls: TlsCapture,
    http: HttpCapture,
}

#[derive(Debug, Deserialize)]
struct TlsCapture {
    #[serde(rename = "server_name")]
    _server_name: Option<String>,
    cipher_suites: Vec<u16>,
    extensions: Vec<u16>,
    supported_groups: Vec<u16>,
    #[serde(default)]
    key_share_groups: Vec<u16>,
    #[serde(default)]
    signature_algorithms: Vec<u16>,
    #[serde(default)]
    delegated_credentials: Vec<u16>,
    #[serde(default)]
    certificate_compression_algorithms: Vec<u16>,
    record_size_limit: Option<u16>,
    #[serde(default)]
    alpn: Vec<String>,
    #[serde(default)]
    has_session_ticket: bool,
    #[serde(default)]
    has_status_request: bool,
    #[serde(default)]
    has_signed_certificate_timestamp: bool,
    #[serde(default)]
    has_ech: bool,
    #[serde(default)]
    uses_grease: bool,
    ja3: String,
    ja4: String,
}

#[derive(Debug, Deserialize)]
struct HttpCapture {
    protocol: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    header_order: Vec<String>,
    #[serde(default)]
    pseudo_header_order: Vec<String>,
    #[serde(default)]
    settings: Vec<Http2Setting>,
    connection_window_update: Option<u32>,
    #[serde(default)]
    priorities: Vec<Http2Priority>,
    akamai: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Http2Setting {
    id: u16,
    value: u32,
}

#[derive(Debug, Deserialize)]
struct Http2Priority {
    stream_id: u32,
    exclusive: u8,
    dependency: u32,
    weight: u16,
    source: String,
}

#[derive(Debug, Serialize)]
struct CollisionResult {
    source_id: String,
    replay_id: String,
    ja3_equal: bool,
    ja4_equal: bool,
    signature_algorithms_equal: bool,
    supported_groups_equal: bool,
    key_share_groups_equal: bool,
    certificate_compression_equal: bool,
    akamai_equal: bool,
    header_order_equal: bool,
    limitations: Vec<String>,
}

#[derive(Debug)]
struct BrotliCompressor;

#[derive(Debug)]
struct ZlibCompressor;

#[derive(Debug)]
struct ZstdCompressor;

impl CertificateCompressor for BrotliCompressor {
    fn compress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut writer = BrotliEncoder::new(output, input.len(), 11, 22);
            writer.write_all(input)?;
            writer.flush()?;
            Ok(())
        })
    }

    fn decompress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut reader = BrotliDecoder::new(input, 4096);
            io::copy(&mut reader, output)?;
            Ok(())
        })
    }

    fn algorithm(&self) -> CertificateCompressionAlgorithm {
        CertificateCompressionAlgorithm::BROTLI
    }
}

impl CertificateCompressor for ZlibCompressor {
    fn compress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut encoder = ZlibEncoder::new(output, Compression::default());
            encoder.write_all(input)?;
            encoder.finish()?;
            Ok(())
        })
    }

    fn decompress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut reader = ZlibDecoder::new(input);
            io::copy(&mut reader, output)?;
            Ok(())
        })
    }

    fn algorithm(&self) -> CertificateCompressionAlgorithm {
        CertificateCompressionAlgorithm::ZLIB
    }
}

impl CertificateCompressor for ZstdCompressor {
    fn compress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut encoder = zstd::stream::Encoder::new(output, 0)?;
            encoder.write_all(input)?;
            encoder.finish()?;
            Ok(())
        })
    }

    fn decompress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut reader = zstd::stream::Decoder::new(input)?;
            io::copy(&mut reader, output)?;
            Ok(())
        })
    }

    fn algorithm(&self) -> CertificateCompressionAlgorithm {
        CertificateCompressionAlgorithm::ZSTD
    }
}

static ZLIB_COMPRESSOR: ZlibCompressor = ZlibCompressor;
static BROTLI_COMPRESSOR: BrotliCompressor = BrotliCompressor;
static ZSTD_COMPRESSOR: ZstdCompressor = ZstdCompressor;

fn load_record(path: &Path, record_id: &str) -> Result<Record> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("无法读取记录文件: {}", path.display()))?;
    let records: Vec<Value> = serde_json::from_str(&content).context("指纹记录JSON无效")?;
    let value = if record_id == "latest" {
        records
            .into_iter()
            .rev()
            .find(|item| {
                !item["http"]["target"]
                    .as_str()
                    .is_some_and(|target| target.contains("source=wreq"))
            })
            .context("指纹记录文件没有可回放的源记录")?
    } else {
        records
            .into_iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(record_id))
            .with_context(|| format!("没有找到指纹记录: {record_id}"))?
    };
    serde_json::from_value(value).context("指纹记录缺少wreq装配所需字段")
}

fn cipher_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x1301 => "TLS_AES_128_GCM_SHA256",
        0x1302 => "TLS_AES_256_GCM_SHA384",
        0x1303 => "TLS_CHACHA20_POLY1305_SHA256",
        0xc02b => "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
        0xc02f => "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
        0xc02c => "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
        0xc030 => "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
        0xcca9 => "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
        0xcca8 => "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
        0xc013 => "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA",
        0xc014 => "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA",
        0x009c => "TLS_RSA_WITH_AES_128_GCM_SHA256",
        0x009d => "TLS_RSA_WITH_AES_256_GCM_SHA384",
        0x002f => "TLS_RSA_WITH_AES_128_CBC_SHA",
        0x0035 => "TLS_RSA_WITH_AES_256_CBC_SHA",
        _ => return None,
    })
}

fn group_name(id: u16) -> Option<&'static str> {
    Some(match id {
        23 => "P-256",
        24 => "P-384",
        25 => "P-521",
        29 => "X25519",
        30 => "X448",
        256 => "ffdhe2048",
        257 => "ffdhe3072",
        4588 => "X25519MLKEM768",
        _ => return None,
    })
}

fn key_share(id: u16) -> Option<KeyShare> {
    Some(match id {
        23 => KeyShare::P256,
        24 => KeyShare::P384,
        29 => KeyShare::X25519,
        4588 => KeyShare::X25519_MLKEM768,
        _ => return None,
    })
}

fn signature_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0401 => "rsa_pkcs1_sha256",
        0x0501 => "rsa_pkcs1_sha384",
        0x0601 => "rsa_pkcs1_sha512",
        0x0403 => "ecdsa_secp256r1_sha256",
        0x0503 => "ecdsa_secp384r1_sha384",
        0x0603 => "ecdsa_secp521r1_sha512",
        0x0804 => "rsa_pss_rsae_sha256",
        0x0805 => "rsa_pss_rsae_sha384",
        0x0806 => "rsa_pss_rsae_sha512",
        0x0807 => "ed25519",
        0x0808 => "ed448",
        0x0809 => "rsa_pss_pss_sha256",
        0x080a => "rsa_pss_pss_sha384",
        0x080b => "rsa_pss_pss_sha512",
        _ => return None,
    })
}

fn extension_type(id: u16) -> Option<ExtensionType> {
    Some(match id {
        0 => ExtensionType::SERVER_NAME,
        5 => ExtensionType::STATUS_REQUEST,
        10 => ExtensionType::SUPPORTED_GROUPS,
        11 => ExtensionType::EC_POINT_FORMATS,
        13 => ExtensionType::SIGNATURE_ALGORITHMS,
        16 => ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION,
        18 => ExtensionType::CERTIFICATE_TIMESTAMP,
        23 => ExtensionType::EXTENDED_MASTER_SECRET,
        27 => ExtensionType::CERT_COMPRESSION,
        28 => ExtensionType::RECORD_SIZE_LIMIT,
        34 => ExtensionType::DELEGATED_CREDENTIAL,
        35 => ExtensionType::SESSION_TICKET,
        41 => ExtensionType::PRE_SHARED_KEY,
        43 => ExtensionType::SUPPORTED_VERSIONS,
        45 => ExtensionType::PSK_KEY_EXCHANGE_MODES,
        51 => ExtensionType::KEY_SHARE,
        17513 => ExtensionType::APPLICATION_SETTINGS_OLD,
        17613 => ExtensionType::APPLICATION_SETTINGS,
        0xff01 => ExtensionType::RENEGOTIATE,
        0xfe0d => ExtensionType::ENCRYPTED_CLIENT_HELLO,
        _ => return None,
    })
}

fn build_tls(capture: &TlsCapture, limitations: &mut Vec<String>) -> Result<TlsOptions> {
    let cipher_names: Vec<_> = capture
        .cipher_suites
        .iter()
        .filter_map(|id| {
            let name = cipher_name(*id);
            if name.is_none() {
                limitations.push(format!("未识别TLS cipher 0x{id:04x}"));
            }
            name
        })
        .collect();
    let group_names: Vec<_> = capture
        .supported_groups
        .iter()
        .filter_map(|id| {
            let name = group_name(*id);
            if name.is_none() {
                limitations.push(format!("未识别supported group {id}"));
            }
            name
        })
        .collect();
    let signature_names: Vec<_> = capture
        .signature_algorithms
        .iter()
        .filter_map(|id| {
            let name = signature_name(*id);
            if name.is_none() {
                limitations.push(format!("未识别signature algorithm 0x{id:04x}"));
            }
            name
        })
        .collect();
    if cipher_names.is_empty() || group_names.is_empty() || signature_names.is_empty() {
        bail!("捕获记录无法生成有效TLS配置")
    }

    let mut builder = TlsOptions::builder()
        .cipher_list(cipher_names.join(":"))
        .curves_list(group_names.join(":"))
        .sigalgs_list(signature_names.join(":"))
        .alpn_protocols(capture.alpn.iter().filter_map(|item| match item.as_str() {
            "h2" => Some(AlpnProtocol::HTTP2),
            "http/1.1" => Some(AlpnProtocol::HTTP1),
            _ => None,
        }))
        .min_tls_version(TlsVersion::TLS_1_2)
        .max_tls_version(TlsVersion::TLS_1_3)
        .session_ticket(capture.has_session_ticket)
        // ClientHello中的PSK扩展只在命中先前服务端ticket时出现，不能用一次采集
        // 是否出现41决定会话缓存；支持Session Ticket的profile应允许后续自然恢复。
        .pre_shared_key(capture.has_session_ticket)
        .enable_ocsp_stapling(capture.has_status_request)
        .enable_signed_cert_timestamps(capture.has_signed_certificate_timestamp)
        .enable_ech_grease(capture.has_ech)
        .grease_enabled(capture.uses_grease)
        .permute_extensions(false)
        .preserve_tls13_cipher_list(true)
        .aes_hw_override(true)
        .record_size_limit(capture.record_size_limit);

    let shares: Vec<_> = capture
        .key_share_groups
        .iter()
        .filter_map(|id| key_share(*id))
        .collect();
    if !shares.is_empty() {
        builder = builder.key_shares(shares);
    }
    let delegated: Vec<_> = capture
        .delegated_credentials
        .iter()
        .filter_map(|id| signature_name(*id))
        .collect();
    if !delegated.is_empty() {
        builder = builder.delegated_credentials(delegated.join(":"));
    }
    let compressors: Vec<&'static dyn CertificateCompressor> = capture
        .certificate_compression_algorithms
        .iter()
        .filter_map(|id| match id {
            1 => Some(&ZLIB_COMPRESSOR as &dyn CertificateCompressor),
            2 => Some(&BROTLI_COMPRESSOR as &dyn CertificateCompressor),
            3 => Some(&ZSTD_COMPRESSOR as &dyn CertificateCompressor),
            _ => {
                limitations.push(format!("未识别证书压缩算法 {id}"));
                None
            }
        })
        .collect();
    if !compressors.is_empty() {
        builder = builder.certificate_compressors(compressors);
    }
    let extension_order: Vec<_> = capture
        .extensions
        .iter()
        .filter_map(|id| {
            let value = extension_type(*id);
            if value.is_none() {
                limitations.push(format!("wreq没有公开TLS扩展 {id} 的排序常量"));
            }
            value
        })
        .collect();
    if !extension_order.is_empty() {
        builder = builder.extension_permutation(extension_order);
    }
    if capture.extensions.contains(&17613) || capture.extensions.contains(&17513) {
        builder = builder
            .alps_protocols([wreq::tls::AlpsProtocol::HTTP2])
            .alps_use_new_codepoint(capture.extensions.contains(&17613));
    }

    Ok(builder.build())
}

fn setting_id(id: u16) -> Option<SettingId> {
    Some(match id {
        1 => SettingId::HeaderTableSize,
        2 => SettingId::EnablePush,
        3 => SettingId::MaxConcurrentStreams,
        4 => SettingId::InitialWindowSize,
        5 => SettingId::MaxFrameSize,
        6 => SettingId::MaxHeaderListSize,
        8 => SettingId::EnableConnectProtocol,
        9 => SettingId::NoRfc7540Priorities,
        _ => return None,
    })
}

fn build_http2(capture: &HttpCapture, limitations: &mut Vec<String>) -> Http2Options {
    let values: HashMap<_, _> = capture
        .settings
        .iter()
        .map(|item| (item.id, item.value))
        .collect();
    let mut builder = Http2Options::builder();
    if let Some(value) = values.get(&1) {
        builder = builder.header_table_size(*value);
    }
    if let Some(value) = values.get(&2) {
        builder = builder.enable_push(*value != 0);
    }
    if let Some(value) = values.get(&3) {
        builder = builder.max_concurrent_streams(*value);
    }
    if let Some(value) = values.get(&4) {
        builder = builder.initial_window_size(*value);
    }
    if let Some(value) = values.get(&5) {
        builder = builder.max_frame_size(*value);
    }
    if let Some(value) = values.get(&6) {
        builder = builder.max_header_list_size(*value);
    }
    if let Some(increment) = capture.connection_window_update {
        builder = builder.initial_connection_window_size(increment.saturating_add(65_535));
    }
    let settings_order = capture.settings.iter().filter_map(|item| {
        let value = setting_id(item.id);
        if value.is_none() {
            limitations.push(format!("wreq没有公开HTTP/2 setting {} 的排序常量", item.id));
        }
        value
    });
    builder = builder.settings_order(SettingsOrder::builder().extend(settings_order).build());

    let pseudo_order = capture
        .pseudo_header_order
        .iter()
        .filter_map(|name| match name.as_str() {
            ":method" => Some(PseudoId::Method),
            ":path" => Some(PseudoId::Path),
            ":authority" => Some(PseudoId::Authority),
            ":scheme" => Some(PseudoId::Scheme),
            _ => None,
        });
    builder = builder.headers_pseudo_order(PseudoOrder::builder().extend(pseudo_order).build());

    let standalone: Vec<_> = capture
        .priorities
        .iter()
        .filter(|item| item.source == "PRIORITY")
        .map(|item| {
            Priority::new(
                StreamId::from(item.stream_id),
                StreamDependency::new(
                    StreamId::from(item.dependency),
                    item.weight.saturating_sub(1) as u8,
                    item.exclusive != 0,
                ),
            )
        })
        .collect();
    if !standalone.is_empty() {
        builder = builder.priorities(Priorities::builder().extend(standalone).build());
    }
    if let Some(item) = capture
        .priorities
        .iter()
        .find(|item| item.source == "HEADERS")
    {
        builder = builder.headers_stream_dependency(StreamDependency::new(
            StreamId::from(item.dependency),
            item.weight.saturating_sub(1) as u8,
            item.exclusive != 0,
        ));
    }
    builder.build()
}

fn build_headers(capture: &HttpCapture) -> Result<(HeaderMap, OrigHeaderMap)> {
    // 导航上下文与业务/API请求不一致，不能把一次导航采集值固化到每条请求。
    let ignored = [
        "host",
        "content-length",
        "connection",
        "cookie",
        "authorization",
        "proxy-authorization",
        "accept",
        "origin",
        "referer",
        "upgrade-insecure-requests",
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-user",
        "sec-fetch-dest",
        "priority",
    ];
    let mut headers = HeaderMap::new();
    let mut original = OrigHeaderMap::new();
    for (name, value) in &capture.headers {
        if ignored.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("请求头名称无效: {name}"))?;
        let header_value =
            HeaderValue::from_str(value).with_context(|| format!("请求头值无效: {name}"))?;
        headers.append(header_name, header_value);
        original.insert(name.clone());
    }
    Ok((headers, original))
}

fn build_client(
    record: &Record,
    limitations: &mut Vec<String>,
    session_cache: Arc<LruTlsSessionCache>,
) -> Result<Client> {
    let tls = build_tls(&record.tls, limitations)?;
    let (headers, original) = build_headers(&record.http)?;
    let mut emulation = Emulation::builder()
        .tls_options(tls)
        .headers(headers)
        .orig_headers(original);
    if record.http.protocol == "HTTP/2" {
        emulation = emulation.http2_options(build_http2(&record.http, limitations));
    } else {
        limitations.push("源记录不是HTTP/2，未装配Akamai参数".to_string());
    }
    let mut builder = Client::builder()
        .emulation(emulation.build(Default::default()))
        // 每次握手由请求目标的URI host决定SNI，不能依赖采集记录中的一次性值。
        .tls_sni(true)
        .tls_session_cache(session_cache)
        .connect_timeout(Duration::from_secs(请求超时秒数));

    if 本地自签证书 {
        builder = builder.tls_cert_verification(false);
    } else {
        // 显式传入编译期Mozilla根证书，禁止退回系统证书路径。
        let store = CertStore::from_der_certs(webpki_root_certs::TLS_SERVER_ROOT_CERTS)
            .context("无法构建内置Mozilla根证书库")?;
        builder = builder.tls_cert_store(store);
    }
    builder.build().context("无法构建wreq客户端")
}

fn compare(source: &Record, replay: &Value, limitations: Vec<String>) -> CollisionResult {
    let tls = &replay["tls"];
    let http = &replay["http"];
    CollisionResult {
        source_id: source.id.clone(),
        replay_id: replay["id"].as_str().unwrap_or_default().to_string(),
        ja3_equal: tls["ja3"].as_str() == Some(source.tls.ja3.as_str()),
        ja4_equal: tls["ja4"].as_str() == Some(source.tls.ja4.as_str()),
        signature_algorithms_equal: tls["signature_algorithms"]
            == serde_json::json!(source.tls.signature_algorithms),
        supported_groups_equal: tls["supported_groups"]
            == serde_json::json!(source.tls.supported_groups),
        key_share_groups_equal: tls["key_share_groups"]
            == serde_json::json!(source.tls.key_share_groups),
        certificate_compression_equal: tls["certificate_compression_algorithms"]
            == serde_json::json!(source.tls.certificate_compression_algorithms),
        akamai_equal: http["akamai"].as_str() == source.http.akamai.as_deref(),
        header_order_equal: http["header_order"] == serde_json::json!(source.http.header_order),
        limitations,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let record = load_record(&base.join(记录文件), 记录编号)?;
    let mut limitations = Vec::new();
    let session_cache = Arc::new(LruTlsSessionCache::new(8));

    if record.tls.extensions.contains(&41) {
        let warmup_client = build_client(&record, &mut limitations, session_cache.clone())?;
        let warmup_response = warmup_client
            .get(format!("{目标地址}&warmup=1"))
            .send()
            .await
            .context("wreq的TLS Session预热请求失败")?;
        drop(warmup_response);
        drop(warmup_client);
    }

    let client = build_client(&record, &mut limitations, session_cache)?;
    let response = client
        .get(目标地址)
        .send()
        .await
        .context("wreq回放请求失败")?
        .error_for_status()
        .context("指纹服务器返回错误状态")?;
    let replay: Value =
        serde_json::from_str(&response.text().await?).context("指纹服务器返回的JSON无效")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&compare(&record, &replay, limitations))?
    );
    Ok(())
}
