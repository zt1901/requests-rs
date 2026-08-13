use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::{self, Write},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use arc_swap::{ArcSwap, ArcSwapOption};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use brotli::{CompressorWriter as BrotliEncoder, Decompressor as BrotliDecoder};
use bytes::Bytes;
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use futures_util::{Stream, StreamExt};
use pyo3::{
    exceptions::PyRuntimeError,
    ffi::c_str,
    prelude::*,
    types::{PyAny, PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyModule, PyString, PyTuple},
};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, Notify},
};
use url::Url;
use wreq::{
    Client, Emulation, Method, Proxy, Uri, Version,
    cookie::{CookieStore, Cookies as RequestCookies, Jar},
    header::{HeaderMap, HeaderName, HeaderValue, OrigHeaderMap},
    http2::{
        Http2Options, Priorities, Priority, PseudoId, PseudoOrder, SettingId, SettingsOrder,
        StreamDependency, StreamId,
    },
    multipart, redirect,
    tls::{
        AlpnProtocol, ExtensionType, KeyShare, TlsOptions, TlsVersion,
        compress::{CertificateCompressionAlgorithm, CertificateCompressor, Codec},
        session::LruTlsSessionCache,
        trust::CertStore,
    },
};

const 内置指纹: &str = include_str!("../fingerprints.json");
const 版权说明: &str = include_str!("../NOTICE.txt");
// 公开 API 包装作为资源编译进原生扩展，分发 wheel 不再包含明文 requests.py。
const API包装源码: &std::ffi::CStr = c_str!(include_str!("api_wrapper.py"));
type NativeHistoryEntry = (u16, String, String, Vec<(String, String)>);
type NativeCookie = (String, String, Option<String>, Option<String>, bool, bool);
type NativeTransferCounters = Arc<TransferCounters>;
type RawNativeResponse = (
    u16,
    NativeHeaders,
    Bytes,
    String,
    String,
    String,
    Vec<NativeHistoryEntry>,
    Option<NativeTransferCounters>,
);

struct TransferCounters {
    upload: AtomicU64,
    download: AtomicU64,
}

struct TransferMeter {
    proxy: Proxy,
    counters: NativeTransferCounters,
    _completed: tokio::task::JoinHandle<()>,
}

#[pyclass]
struct NativeTransferStats {
    counters: NativeTransferCounters,
}

#[pymethods]
impl NativeTransferStats {
    #[getter]
    fn upload_size(&self) -> u64 {
        self.counters.upload.load(Ordering::Acquire)
    }

    #[getter]
    fn download_size(&self) -> u64 {
        self.counters.download.load(Ordering::Acquire)
    }

    #[getter]
    fn response_size(&self) -> u64 {
        self.upload_size() + self.download_size()
    }

    #[getter]
    fn scope(&self) -> &'static str {
        "tcp_payload_through_metered_tunnel"
    }
}

#[pyclass]
struct NativeHeaders {
    raw: Vec<(String, String)>,
    values: HashMap<String, Vec<String>>,
    names: Vec<String>,
}

#[pyclass]
struct NativeResponse {
    #[pyo3(get)]
    status_code: u16,
    #[pyo3(get)]
    headers: Py<NativeHeaders>,
    #[pyo3(get)]
    content: Py<PyBytes>,
    #[pyo3(get)]
    url: String,
    #[pyo3(get)]
    fingerprint_id: String,
    #[pyo3(get)]
    impersonate: String,
    #[pyo3(get)]
    transfer_stats: Option<Py<NativeTransferStats>>,
    history: Vec<NativeHistoryEntry>,
}

#[pymethods]
impl NativeResponse {
    fn history(&self) -> Vec<NativeHistoryEntry> {
        self.history.clone()
    }
}

#[pymethods]
impl NativeHeaders {
    #[getter]
    fn raw(&self) -> Vec<(String, String)> {
        self.raw.clone()
    }

    fn get_list(&self, name: String) -> Vec<String> {
        self.values
            .get(&name.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    fn get(&self, name: String, default: Option<String>) -> Option<String> {
        self.values
            .get(&name.to_ascii_lowercase())
            .map(|values| values.join(", "))
            .or(default)
    }

    fn names(&self) -> Vec<String> {
        self.names.clone()
    }

    fn len(&self) -> usize {
        self.values.len()
    }
}

fn native_headers(headers: Vec<(String, String)>) -> NativeHeaders {
    let mut values = HashMap::<String, Vec<String>>::new();
    let mut names = Vec::new();
    for (name, value) in &headers {
        let key = name.to_ascii_lowercase();
        if !values.contains_key(&key) {
            names.push(name.clone());
        }
        values.entry(key).or_default().push(value.clone());
    }
    NativeHeaders {
        raw: headers,
        values,
        names,
    }
}

fn native_headers_from_map(headers: &HeaderMap) -> NativeHeaders {
    native_headers(
        headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
    )
}

#[pyfunction]
fn build_response_headers(headers: Vec<(String, String)>) -> NativeHeaders {
    native_headers(headers)
}
type NativeBodyStream = Pin<Box<dyn Stream<Item = wreq::Result<Bytes>> + Send>>;

static 共享运行时: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
static 指纹记录缓存: OnceLock<Result<Vec<Arc<Record>>, String>> = OnceLock::new();
static 根证书库缓存: OnceLock<Result<CertStore, String>> = OnceLock::new();

fn shared_runtime() -> PyResult<Arc<tokio::runtime::Runtime>> {
    if let Some(runtime) = 共享运行时.get() {
        return Ok(runtime.clone());
    }
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .max_blocking_threads(32)
            .thread_name("requests-rust-tokio")
            .build()
            .map_err(|error| PyRuntimeError::new_err(format!("无法创建Tokio运行时: {error}")))?,
    );
    let _ = 共享运行时.set(runtime.clone());
    Ok(共享运行时.get().cloned().unwrap_or(runtime))
}

fn parse_records(source: &str) -> Result<Vec<Arc<Record>>> {
    serde_json::from_str::<Vec<Record>>(source)
        .map(|records| records.into_iter().map(Arc::new).collect())
        .context("JSON结构不是指纹记录列表")
}

fn embedded_records() -> PyResult<&'static Vec<Arc<Record>>> {
    指纹记录缓存
        .get_or_init(|| parse_records(内置指纹).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| PyRuntimeError::new_err(format!("内置指纹无效: {error}")))
}

#[derive(Debug, Clone, Deserialize)]
struct Record {
    id: String,
    profile: String,
    tls: TlsCapture,
    http: HttpCapture,
    #[serde(skip)]
    emulation: OnceLock<Result<Emulation, String>>,
}

#[derive(Debug, Clone, Deserialize)]
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
}

#[derive(Debug, Clone, Deserialize)]
struct HttpCapture {
    protocol: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    pseudo_header_order: Vec<String>,
    #[serde(default)]
    settings: Vec<Http2Setting>,
    connection_window_update: Option<u32>,
    #[serde(default)]
    priorities: Vec<Http2Priority>,
}

#[derive(Debug, Clone, Deserialize)]
struct Http2Setting {
    id: u16,
    value: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct Http2Priority {
    stream_id: u32,
    exclusive: u8,
    dependency: u32,
    weight: u16,
    source: String,
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
        0xc00a => "TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA",
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
        0x0201 => "rsa_pkcs1_sha1",
        0x0501 => "rsa_pkcs1_sha384",
        0x0601 => "rsa_pkcs1_sha512",
        0x0403 => "ecdsa_secp256r1_sha256",
        0x0203 => "ecdsa_sha1",
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
        0x0904 => "mldsa44",
        0x0905 => "mldsa65",
        0x0906 => "mldsa87",
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

fn build_tls(capture: &TlsCapture) -> Result<TlsOptions> {
    let cipher_names: Vec<_> = capture
        .cipher_suites
        .iter()
        .filter_map(|id| cipher_name(*id))
        .collect();
    let group_names: Vec<_> = capture
        .supported_groups
        .iter()
        .filter_map(|id| group_name(*id))
        .collect();
    let signature_names: Vec<_> = capture
        .signature_algorithms
        .iter()
        .filter_map(|id| signature_name(*id))
        .collect();
    if cipher_names.is_empty() || group_names.is_empty() || signature_names.is_empty() {
        bail!("指纹缺少可识别的TLS参数")
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
        // 扩展41只会在已经拿到服务器ticket的后续握手中出现。以采集到的一次
        // ClientHello 是否包含41决定缓存，会让首次握手未恢复的profile永远无法恢复。
        // 浏览器支持Session Ticket时先建立缓存，BTLS仅在缓存命中后自然发送PSK。
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
            _ => None,
        })
        .collect();
    if !compressors.is_empty() {
        builder = builder.certificate_compressors(compressors);
    }
    let extension_order: Vec<_> = capture
        .extensions
        .iter()
        .filter_map(|id| extension_type(*id))
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

fn build_http2(capture: &HttpCapture) -> Http2Options {
    let values: HashMap<_, _> = capture
        .settings
        .iter()
        .map(|item| (item.id, item.value))
        .collect();
    let mut builder = Http2Options::builder();
    if let Some(stream_id) = capture
        .priorities
        .iter()
        .find(|item| item.source == "HEADERS")
        .map(|item| item.stream_id)
    {
        builder = builder.initial_stream_id(stream_id);
    }
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
    builder = builder.settings_order(
        SettingsOrder::builder()
            .extend(
                capture
                    .settings
                    .iter()
                    .filter_map(|item| setting_id(item.id)),
            )
            .build(),
    );
    builder = builder.headers_pseudo_order(
        PseudoOrder::builder()
            .extend(
                capture
                    .pseudo_header_order
                    .iter()
                    .filter_map(|name| match name.as_str() {
                        ":method" => Some(PseudoId::Method),
                        ":path" => Some(PseudoId::Path),
                        ":authority" => Some(PseudoId::Authority),
                        ":scheme" => Some(PseudoId::Scheme),
                        _ => None,
                    }),
            )
            .build(),
    );
    let priorities: Vec<_> = capture
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
    if !priorities.is_empty() {
        builder = builder.priorities(Priorities::builder().extend(priorities).build());
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
    // 这些字段取决于当前请求类型、发起页面和调度优先级；采集自一次顶层导航，
    // 不能作为所有 API、资源或跨站请求的固定浏览器默认值发送。
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
        headers.append(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
        original.insert(name.clone());
    }
    Ok((headers, original))
}

fn build_emulation(record: &Record) -> Result<Emulation> {
    let (headers, original) = build_headers(&record.http)?;
    let mut builder = Emulation::builder()
        .tls_options(build_tls(&record.tls)?)
        .headers(headers)
        .orig_headers(original);
    if record.http.protocol == "HTTP/2" {
        builder = builder.http2_options(build_http2(&record.http));
    }
    Ok(builder.build(Default::default()))
}

struct Variant {
    record: Arc<Record>,
    session_cache: Arc<LruTlsSessionCache>,
    // 热路径通过原子快照读取已构建Client；初始化锁只在首个请求竞争构建时使用。
    client: ArcSwapOption<Client>,
    client_init: Mutex<()>,
}

struct SessionState {
    profile: String,
    variants: Vec<Variant>,
    rotation: bool,
    // 固定模式下当前代理会话绑定的指纹变体；切换代理会话时重新随机选择。
    selected_variant: AtomicUsize,
    // 默认代理由set_proxy原子替换，请求仅加载快照，不阻塞其他并发请求。
    proxy: ArcSwapOption<Proxy>,
    // 仅用于识别代理会话是否从 A 切换到 B，不参与实际代理连接。
    proxy_identity: ArcSwapOption<String>,
    // Session默认请求头构造期预解析，普通请求只合并请求级增量头。
    default_headers: ArcSwap<HeaderMap>,
    verify: bool,
    connect_timeout: Option<Duration>,
    cookie_jar: Arc<Jar>,
    counter: AtomicUsize,
    closed: AtomicBool,
}

fn normalize_profile(profile: &str) -> String {
    profile
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn build_client(state: &SessionState, variant: &Variant) -> Result<Client> {
    let emulation = variant
        .record
        .emulation
        .get_or_init(|| build_emulation(&variant.record).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))?;
    let mut builder = Client::builder()
        .emulation(emulation.clone())
        // 浏览器按当前请求目标的域名发送SNI，不能由采集记录某次请求是否带SNI决定。
        .tls_sni(true)
        .tls_session_cache(variant.session_cache.clone())
        .cookie_provider(state.cookie_jar.clone());
    if let Some(timeout) = state.connect_timeout {
        builder = builder.connect_timeout(timeout);
    }
    if state.verify {
        let store = 根证书库缓存
            .get_or_init(|| {
                CertStore::from_der_certs(webpki_root_certs::TLS_SERVER_ROOT_CERTS)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))?;
        builder = builder.tls_cert_store(store.clone());
    } else {
        builder = builder.tls_cert_verification(false);
    }
    builder.build().context("无法构建wreq Client")
}

fn cached_client(state: &SessionState, variant: &Variant) -> PyResult<Client> {
    if let Some(client) = variant.client.load_full() {
        return Ok((*client).clone());
    }
    let _init_guard = variant
        .client_init
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Client初始化锁已损坏"))?;
    if let Some(client) = variant.client.load_full() {
        return Ok((*client).clone());
    }
    let client = build_client(state, variant).map_err(to_py_error)?;
    if state.closed.load(Ordering::Acquire) {
        return Ok(client);
    }
    variant.client.store(Some(Arc::new(client.clone())));
    Ok(client)
}

async fn cached_client_async(state: Arc<SessionState>, index: usize) -> PyResult<Client> {
    if let Some(client) = state.variants[index].client.load_full() {
        return Ok((*client).clone());
    }
    tokio::task::spawn_blocking(move || cached_client(&state, &state.variants[index]))
        .await
        .map_err(|error| PyRuntimeError::new_err(format!("Client构建任务失败: {error}")))?
}

fn random_variant(count: usize) -> usize {
    rand::rng().random_range(0..count)
}

fn request_variant(state: &SessionState) -> usize {
    if state.rotation {
        state.counter.fetch_add(1, Ordering::AcqRel) % state.variants.len()
    } else {
        state.selected_variant.load(Ordering::Acquire)
    }
}

fn parse_timeout(name: &str, value: f64) -> PyResult<Duration> {
    if !value.is_finite() || value <= 0.0 {
        return Err(PyRuntimeError::new_err(format!("{name}必须是有限正数")));
    }
    Duration::try_from_secs_f64(value)
        .map_err(|_| PyRuntimeError::new_err(format!("{name}超出可表示范围")))
}

fn parse_optional_timeout(name: &str, value: Option<f64>) -> PyResult<Option<Duration>> {
    value.map(|value| parse_timeout(name, value)).transpose()
}

fn ensure_open(state: &SessionState) -> PyResult<()> {
    if state.closed.load(Ordering::Acquire) {
        return Err(PyRuntimeError::new_err("Session已经关闭"));
    }
    Ok(())
}

fn selected_proxy(
    state: &SessionState,
    proxy_override: bool,
    proxy: Option<String>,
) -> PyResult<Option<Proxy>> {
    if proxy_override {
        return proxy
            .map(|value| Proxy::all(&value).map_err(to_py_error))
            .transpose();
    }
    Ok(state.proxy.load_full().map(|proxy| (*proxy).clone()))
}

fn selected_proxy_url(
    state: &SessionState,
    proxy_override: bool,
    proxy: Option<String>,
) -> Option<String> {
    if proxy_override {
        proxy
    } else {
        state.proxy_identity.load_full().map(|value| (*value).clone())
    }
}

fn parse_connect_target(value: &str) -> Result<(String, u16)> {
    let (host, port) = value.rsplit_once(':').context("CONNECT目标缺少端口")?;
    Ok((
        host.trim_matches(['[', ']']).to_string(),
        port.parse().context("CONNECT目标端口无效")?,
    ))
}

async fn copy_with_counter<R, W>(
    mut reader: R,
    mut writer: W,
    counters: NativeTransferCounters,
    upload: bool,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buffer = [0u8; 16_384];
    loop {
        let count = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        if writer.write_all(&buffer[..count]).await.is_err() {
            return;
        }
        let counter = if upload {
            &counters.upload
        } else {
            &counters.download
        };
        counter.fetch_add(count as u64, Ordering::Relaxed);
    }
}

async fn run_transfer_meter(
    listener: TcpListener,
    upstream_proxy: Option<String>,
    counters: NativeTransferCounters,
) {
    let Ok((client, _)) = listener.accept().await else {
        return;
    };
    let mut reader = BufReader::new(client);
    let mut request_head = Vec::new();
    if reader.read_until(b'\n', &mut request_head).await.is_err() {
        return;
    }
    let request_line = String::from_utf8_lossy(&request_head);
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("CONNECT") {
        return;
    }
    let Some(target) = parts.next() else {
        return;
    };
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line).await.is_err() || line == b"\r\n" {
            break;
        }
    }
    let Ok((host, port)) = parse_connect_target(target) else {
        return;
    };
    let mut upstream = if let Some(proxy_url) = upstream_proxy {
        let Ok(parsed) = Url::parse(&proxy_url) else {
            return;
        };
        if parsed.scheme() != "http" {
            return;
        }
        let Some(proxy_host) = parsed.host_str() else {
            return;
        };
        let Ok(mut stream) = TcpStream::connect((proxy_host, parsed.port().unwrap_or(80))).await else {
            return;
        };
        let mut connect = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
        if !parsed.username().is_empty() || parsed.password().is_some() {
            let password = parsed.password().unwrap_or_default();
            let auth = BASE64.encode(format!("{}:{password}", parsed.username()));
            connect.push_str(&format!("Proxy-Authorization: Basic {auth}\r\n"));
        }
        connect.push_str("\r\n");
        if stream.write_all(connect.as_bytes()).await.is_err() {
            return;
        }
        let mut upstream_reader = BufReader::new(stream);
        let mut status = Vec::new();
        if upstream_reader.read_until(b'\n', &mut status).await.is_err()
            || !String::from_utf8_lossy(&status).contains(" 200 ")
        {
            return;
        }
        let mut connect_response_bytes = status.len();
        loop {
            let mut line = Vec::new();
            if upstream_reader.read_until(b'\n', &mut line).await.is_err() || line == b"\r\n" {
                break;
            }
            connect_response_bytes += line.len();
        }
        counters.download.fetch_add(connect_response_bytes as u64, Ordering::Relaxed);
        upstream_reader.into_inner()
    } else {
        let Ok(stream) = TcpStream::connect((host.as_str(), port)).await else {
            return;
        };
        stream
    };
    let mut client = reader.into_inner();
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .is_err()
    {
        return;
    }
    let (client_read, client_write) = client.split();
    let (upstream_read, upstream_write) = upstream.split();
    tokio::join!(
        copy_with_counter(client_read, upstream_write, counters.clone(), true),
        copy_with_counter(upstream_read, client_write, counters, false),
    );
}

async fn start_transfer_meter(upstream_proxy: Option<String>) -> PyResult<TransferMeter> {
    if let Some(proxy) = upstream_proxy.as_deref() {
        let parsed = Url::parse(proxy)
            .map_err(|error| PyRuntimeError::new_err(format!("transfer_stats代理URL无效: {error}")))?;
        if parsed.scheme() != "http" {
            return Err(PyRuntimeError::new_err(
                "transfer_stats仅支持直连或http://上游代理；HTTPS/SOCKS代理无法保持相同的TCP计量语义",
            ));
        }
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(to_py_error)?;
    let port = listener.local_addr().map_err(to_py_error)?.port();
    let counters = Arc::new(TransferCounters {
        upload: AtomicU64::new(0),
        download: AtomicU64::new(0),
    });
    let completed = tokio::spawn(run_transfer_meter(listener, upstream_proxy, counters.clone()));
    let proxy = Proxy::all(format!("http://127.0.0.1:{port}")).map_err(to_py_error)?;
    Ok(TransferMeter {
        proxy,
        counters,
        _completed: completed,
    })
}

fn select_proxy_mapping(
    url: &str,
    proxies: Vec<(String, Option<String>)>,
) -> PyResult<Option<String>> {
    let scheme = Uri::from_maybe_shared(url.to_string())
        .map_err(to_py_error)?
        .scheme_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    for key in [
        format!("{scheme}://"),
        scheme,
        "all://".to_string(),
        "all".to_string(),
    ] {
        if let Some((_, proxy)) = proxies.iter().find(|(name, _)| name == &key) {
            return Ok(proxy.clone());
        }
    }
    Ok(None)
}

fn append_query(url: &str, params: Vec<(String, Vec<String>)>) -> PyResult<String> {
    if params.is_empty() {
        return Ok(url.to_string());
    }
    let mut parsed = Url::parse(url).map_err(to_py_error)?;
    let mut serializer = parsed.query_pairs_mut();
    for (name, values) in params {
        for value in values {
            serializer.append_pair(&name, &value);
        }
    }
    drop(serializer);
    Ok(parsed.into())
}

fn encode_form(params: Vec<(String, Vec<String>)>) -> Vec<u8> {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, values) in params {
        for value in values {
            serializer.append_pair(&name, &value);
        }
    }
    serializer.finish().into_bytes()
}

fn json_value(value: &Bound<'_, PyAny>) -> PyResult<Option<JsonValue>> {
    if value.is_none() {
        return Ok(Some(JsonValue::Null));
    }
    if value.is_instance_of::<PyBool>() {
        return Ok(Some(JsonValue::Bool(value.extract()?)));
    }
    if value.is_instance_of::<PyString>() {
        return Ok(Some(JsonValue::String(value.extract()?)));
    }
    if value.is_instance_of::<PyInt>() {
        if let Ok(number) = value.extract::<i64>() {
            return Ok(Some(JsonValue::Number(number.into())));
        }
        if let Ok(number) = value.extract::<u64>() {
            return Ok(Some(JsonValue::Number(number.into())));
        }
        return Ok(None);
    }
    if value.is_instance_of::<PyFloat>() {
        let number = value.extract::<f64>()?;
        return Ok(serde_json::Number::from_f64(number).map(JsonValue::Number));
    }
    if let Ok(values) = value.cast::<PyList>() {
        let mut result = Vec::with_capacity(values.len());
        for item in values.iter() {
            let Some(item) = json_value(&item)? else {
                return Ok(None);
            };
            result.push(item);
        }
        return Ok(Some(JsonValue::Array(result)));
    }
    if let Ok(values) = value.cast::<PyTuple>() {
        let mut result = Vec::with_capacity(values.len());
        for item in values.iter() {
            let Some(item) = json_value(&item)? else {
                return Ok(None);
            };
            result.push(item);
        }
        return Ok(Some(JsonValue::Array(result)));
    }
    if let Ok(values) = value.cast::<PyDict>() {
        let mut result = JsonMap::with_capacity(values.len());
        for (key, item) in values.iter() {
            let Ok(key) = key.extract::<String>() else {
                return Ok(None);
            };
            let Some(item) = json_value(&item)? else {
                return Ok(None);
            };
            result.insert(key, item);
        }
        return Ok(Some(JsonValue::Object(result)));
    }
    Ok(None)
}

fn parse_method(method: &str) -> PyResult<Method> {
    Ok(match method {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        "HEAD" => Method::HEAD,
        "OPTIONS" => Method::OPTIONS,
        _ => Method::from_bytes(method.as_bytes()).map_err(to_py_error)?,
    })
}

fn parse_headers(headers: Vec<(String, String)>) -> PyResult<HeaderMap> {
    let mut result = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        result.append(
            HeaderName::from_bytes(name.as_bytes()).map_err(to_py_error)?,
            HeaderValue::from_str(&value).map_err(to_py_error)?,
        );
    }
    Ok(result)
}

fn merge_headers(
    default_headers: &HeaderMap,
    request_headers: Vec<(String, String)>,
) -> PyResult<HeaderMap> {
    if request_headers.is_empty() {
        return Ok(default_headers.clone());
    }
    let overridden: HashSet<String> = request_headers
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    let mut headers = HeaderMap::with_capacity(default_headers.len() + request_headers.len());
    for (name, value) in default_headers {
        if !overridden.contains(name.as_str()) {
            headers.append(name.clone(), value.clone());
        }
    }
    for (name, value) in request_headers {
        headers.append(
            HeaderName::from_bytes(name.as_bytes()).map_err(to_py_error)?,
            HeaderValue::from_str(&value).map_err(to_py_error)?,
        );
    }
    Ok(headers)
}

fn cookie_pairs(jar: &Jar, url: &str) -> PyResult<Vec<(String, String)>> {
    let uri = url.parse::<Uri>().map_err(to_py_error)?;
    let values = match jar.cookies(&uri, Version::HTTP_11) {
        RequestCookies::Compressed(value) => vec![value],
        RequestCookies::Uncompressed(values) => values,
        RequestCookies::Empty => Vec::new(),
        _ => Vec::new(),
    };
    let mut result = Vec::new();
    for value in values {
        for item in value.to_str().map_err(to_py_error)?.split(';') {
            if let Some((name, value)) = item.trim().split_once('=') {
                result.push((name.to_string(), value.to_string()));
            }
        }
    }
    Ok(result)
}

fn prepare_headers(
    state: &SessionState,
    url: &str,
    request_headers: Vec<(String, String)>,
    request_cookies: Option<Vec<(String, String)>>,
) -> PyResult<HeaderMap> {
    let mut headers = merge_headers(&state.default_headers.load(), request_headers)?;
    let Some(request_cookies) = request_cookies else {
        return Ok(headers);
    };
    headers.remove("cookie");
    let mut values = cookie_pairs(&state.cookie_jar, url)?;
    let mut positions: HashMap<String, usize> = values
        .iter()
        .enumerate()
        .map(|(index, (name, _))| (name.clone(), index))
        .collect();
    for (name, value) in request_cookies {
        if let Some(index) = positions.get(&name) {
            values[*index] = (name, value);
        } else {
            positions.insert(name.clone(), values.len());
            values.push((name, value));
        }
    }
    if !values.is_empty() {
        let cookie = values
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        headers.append(
            HeaderName::from_static("cookie"),
            HeaderValue::from_str(&cookie).map_err(to_py_error)?,
        );
    }
    Ok(headers)
}

#[allow(clippy::too_many_arguments)]
async fn execute_request(
    client: Client,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Duration,
    read_timeout: Option<Duration>,
    proxy: Option<Proxy>,
    allow_redirects: bool,
    max_redirects: usize,
    fingerprint_id: String,
    profile: String,
    transfer_stats: bool,
    upstream_proxy: Option<String>,
) -> PyResult<RawNativeResponse> {
    if transfer_stats && !url.starts_with("https://") {
        return Err(PyRuntimeError::new_err(
            "transfer_stats仅支持HTTPS请求，统计对象表示TLS隧道的实际TCP字节",
        ));
    }
    let meter = if transfer_stats {
        Some(start_transfer_meter(upstream_proxy).await?)
    } else {
        None
    };
    let mut request = client
        .request(method, &url)
        .timeout(timeout)
        .redirect(if allow_redirects {
            redirect::Policy::limited(max_redirects)
        } else {
            redirect::Policy::none()
        })
        .headers(headers);
    if let Some(timeout) = read_timeout {
        request = request.read_timeout(timeout);
    }
    if let Some(meter) = &meter {
        request = request.proxy(meter.proxy.clone());
    } else if let Some(proxy) = proxy {
        request = request.proxy(proxy);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = request.send().await.map_err(to_py_error)?;
    let status = response.status().as_u16();
    let final_url = response.uri().to_string();
    let history = response
        .extensions()
        .get::<redirect::History>()
        .map(history_entries)
        .unwrap_or_default();
    let response_headers = native_headers_from_map(response.headers());
    let content = response.bytes().await.map_err(to_py_error)?;
    let transfer_counters = if let Some(meter) = meter {
        Some(meter.counters)
    } else {
        None
    };
    Ok((
        status,
        response_headers,
        content,
        fingerprint_id,
        profile,
        final_url,
        history,
        transfer_counters,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn execute_stream_request(
    client: Client,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Duration,
    read_timeout: Option<Duration>,
    proxy: Option<Proxy>,
    allow_redirects: bool,
    max_redirects: usize,
    fingerprint_id: String,
    profile: String,
) -> PyResult<NativeStreamResponse> {
    let mut request = client
        .request(method, &url)
        .timeout(timeout)
        .redirect(if allow_redirects {
            redirect::Policy::limited(max_redirects)
        } else {
            redirect::Policy::none()
        })
        .headers(headers);
    if let Some(timeout) = read_timeout {
        request = request.read_timeout(timeout);
    }
    if let Some(proxy) = proxy {
        request = request.proxy(proxy);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = request.send().await.map_err(to_py_error)?;
    let status_code = response.status().as_u16();
    let history = response
        .extensions()
        .get::<redirect::History>()
        .map(history_entries)
        .unwrap_or_default();
    let response_headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let final_url = response.uri().to_string();
    let stream: NativeBodyStream = Box::pin(response.bytes_stream());
    Ok(NativeStreamResponse {
        status_code,
        headers: response_headers,
        url: final_url,
        fingerprint_id,
        impersonate: profile,
        history,
        state: Arc::new(AsyncMutex::new(StreamState {
            stream: Some(stream),
            buffered: Vec::new(),
            offset: 0,
            terminal_error: None,
        })),
        closed: Arc::new(AtomicBool::new(false)),
        close_notify: Arc::new(Notify::new()),
        read_active: Arc::new(AtomicBool::new(false)),
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_multipart_request(
    client: Client,
    method: Method,
    url: String,
    headers: HeaderMap,
    fields: Vec<(String, String)>,
    files: Vec<(String, String, Option<String>, Option<String>)>,
    timeout: Duration,
    read_timeout: Option<Duration>,
    proxy: Option<Proxy>,
    allow_redirects: bool,
    max_redirects: usize,
    fingerprint_id: String,
    profile: String,
) -> PyResult<RawNativeResponse> {
    let mut form = multipart::Form::new();
    for (name, value) in fields {
        form = form.text(name, value);
    }
    for (name, path, filename, content_type) in files {
        let mut part = multipart::Part::file(&path).await.map_err(to_py_error)?;
        if let Some(filename) = filename {
            part = part.file_name(filename);
        }
        if let Some(content_type) = content_type {
            part = part.mime_str(&content_type).map_err(to_py_error)?;
        }
        form = form.part(name, part);
    }
    let mut request = client
        .request(method, &url)
        .timeout(timeout)
        .redirect(if allow_redirects {
            redirect::Policy::limited(max_redirects)
        } else {
            redirect::Policy::none()
        })
        .headers(headers)
        .multipart(form);
    if let Some(timeout) = read_timeout {
        request = request.read_timeout(timeout);
    }
    if let Some(proxy) = proxy {
        request = request.proxy(proxy);
    }
    let response = request.send().await.map_err(to_py_error)?;
    let status = response.status().as_u16();
    let final_url = response.uri().to_string();
    let history = response
        .extensions()
        .get::<redirect::History>()
        .map(history_entries)
        .unwrap_or_default();
    let response_headers = native_headers_from_map(response.headers());
    let content = response.bytes().await.map_err(to_py_error)?;
    Ok((
        status,
        response_headers,
        content,
        fingerprint_id,
        profile,
        final_url,
        history,
        None,
    ))
}

fn into_native_response(
    py: Python<'_>,
    response: RawNativeResponse,
) -> PyResult<Py<NativeResponse>> {
    let (status, headers, content, fingerprint_id, profile, url, history, transfer_counters) = response;
    let native_headers = Py::new(py, headers)?;
    Py::new(
        py,
        NativeResponse {
            status_code: status,
            headers: native_headers,
            content: PyBytes::new(py, &content).unbind(),
            url,
            fingerprint_id,
            impersonate: profile,
            history,
            transfer_stats: transfer_counters
                .map(|counters| Py::new(py, NativeTransferStats { counters }))
                .transpose()?,
        },
    )
}

struct StreamState {
    stream: Option<NativeBodyStream>,
    buffered: Vec<u8>,
    offset: usize,
    terminal_error: Option<String>,
}

struct ReadGuard(Arc<AtomicBool>);

impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

async fn close_stream(
    state: Arc<AsyncMutex<StreamState>>,
    closed: Arc<AtomicBool>,
    close_notify: Arc<Notify>,
) {
    closed.store(true, Ordering::Release);
    close_notify.notify_one();
    let mut state = state.lock().await;
    state.stream = None;
    state.buffered.clear();
    state.offset = 0;
}

async fn read_stream(
    state: Arc<AsyncMutex<StreamState>>,
    closed: Arc<AtomicBool>,
    close_notify: Arc<Notify>,
    read_active: Arc<AtomicBool>,
    size: i64,
) -> PyResult<Vec<u8>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    if closed.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    if read_active.swap(true, Ordering::AcqRel) {
        return Err(PyRuntimeError::new_err(
            "同一流同时只允许一个活动读取或消费者",
        ));
    }
    let _guard = ReadGuard(read_active);
    let requested = if size < 0 { usize::MAX } else { size as usize };
    let mut state = state.lock().await;
    if let Some(error) = &state.terminal_error {
        return Err(PyRuntimeError::new_err(error.clone()));
    }
    while state.buffered.len().saturating_sub(state.offset) < requested {
        if closed.load(Ordering::Acquire) {
            state.stream = None;
            state.buffered.clear();
            state.offset = 0;
            return Ok(Vec::new());
        }
        let Some(stream) = state.stream.as_mut() else {
            break;
        };
        let notified = close_notify.notified();
        match tokio::select! {
            biased;
            _ = notified => None,
            item = stream.next() => item,
        } {
            Some(Ok(chunk)) => state.buffered.extend_from_slice(&chunk),
            Some(Err(error)) => {
                let message = error.to_string();
                state.stream = None;
                state.buffered.clear();
                state.offset = 0;
                state.terminal_error = Some(message.clone());
                return Err(PyRuntimeError::new_err(message));
            }
            None => {
                state.stream = None;
                if closed.load(Ordering::Acquire) {
                    state.buffered.clear();
                    state.offset = 0;
                    return Ok(Vec::new());
                }
                break;
            }
        }
    }
    let available = state.buffered.len().saturating_sub(state.offset);
    let count = available.min(requested);
    let result = state.buffered[state.offset..state.offset + count].to_vec();
    state.offset += count;
    if state.offset == state.buffered.len() {
        state.buffered.clear();
        state.offset = 0;
    } else if state.offset >= 64 * 1024 {
        let offset = state.offset;
        state.buffered.drain(..offset);
        state.offset = 0;
    }
    Ok(result)
}

#[pyclass]
struct NativeStreamResponse {
    #[pyo3(get)]
    status_code: u16,
    #[pyo3(get)]
    headers: Vec<(String, String)>,
    #[pyo3(get)]
    url: String,
    #[pyo3(get)]
    fingerprint_id: String,
    #[pyo3(get)]
    impersonate: String,
    #[pyo3(get)]
    history: Vec<NativeHistoryEntry>,
    state: Arc<AsyncMutex<StreamState>>,
    closed: Arc<AtomicBool>,
    close_notify: Arc<Notify>,
    read_active: Arc<AtomicBool>,
}

#[pymethods]
impl NativeStreamResponse {
    #[pyo3(signature = (size=-1))]
    fn read(&self, py: Python<'_>, size: i64) -> PyResult<Vec<u8>> {
        let runtime = shared_runtime()?;
        let state = self.state.clone();
        let closed = self.closed.clone();
        let close_notify = self.close_notify.clone();
        let read_active = self.read_active.clone();
        py.detach(|| runtime.block_on(read_stream(state, closed, close_notify, read_active, size)))
    }

    #[pyo3(signature = (size=-1))]
    fn read_async<'py>(&self, py: Python<'py>, size: i64) -> PyResult<Bound<'py, PyAny>> {
        let state = self.state.clone();
        let closed = self.closed.clone();
        let close_notify = self.close_notify.clone();
        let read_active = self.read_active.clone();
        pyo3_async_runtimes::tokio::future_into_py(
            py,
            read_stream(state, closed, close_notify, read_active, size),
        )
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.close_notify.notify_one();
        if let Some(runtime) = 共享运行时.get() {
            runtime.spawn(close_stream(
                self.state.clone(),
                self.closed.clone(),
                self.close_notify.clone(),
            ));
        }
    }

    fn close_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = self.state.clone();
        let closed = self.closed.clone();
        let close_notify = self.close_notify.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            close_stream(state, closed, close_notify).await;
            Ok(())
        })
    }
}

#[pyclass]
struct NativeSession {
    state: Arc<SessionState>,
}

#[pymethods]
impl NativeSession {
    #[new]
    #[pyo3(signature = (impersonate, fingerprint_rotation=false, proxy=None, verify=true, connect_timeout=None, fingerprints_path=None, default_headers=Vec::new()))]
    fn new(
        py: Python<'_>,
        impersonate: String,
        fingerprint_rotation: bool,
        proxy: Option<String>,
        verify: bool,
        connect_timeout: Option<f64>,
        fingerprints_path: Option<String>,
        default_headers: Vec<(String, String)>,
    ) -> PyResult<Self> {
        let normalized = normalize_profile(&impersonate);
        let proxy_identity = proxy.clone();
        let proxy = proxy
            .map(|value| Proxy::all(&value).map_err(to_py_error))
            .transpose()?;
        // 指纹来源：传入路径时由本实例独立读取解析（提前单实例加载），
        // 指纹随SessionState的variants持有，不进入进程级全局缓存，多个实例可各读各的文件；
        // 不传路径时回退到编译进wheel的内置指纹（进程级全局缓存只服务内置数据）。
        let (records, source_name) = match fingerprints_path {
            Some(path) => {
                // 构造期文件I/O和JSON反序列化不占用GIL，避免并发创建实例彼此串行。
                let records = py
                    .detach(move || {
                        let content = std::fs::read_to_string(&path)
                            .with_context(|| format!("无法读取指纹文件 {path}"))?;
                        parse_records(&content).with_context(|| format!("指纹文件解析失败 {path}"))
                    })
                    .map_err(to_py_error)?;
                (records, "指纹文件")
            }
            None => (embedded_records()?.clone(), "内置指纹"),
        };
        let matching: Vec<_> = records
            .iter()
            // TLS extension 41（PSK）依赖同一服务端此前签发的 ticket，不能作为新代理会话的
            // 首个 ClientHello 静态复现；仅从可独立对撞的首握手记录中选择粘性随机变体。
            .filter(|record| {
                normalize_profile(&record.profile) == normalized
                    && !record.tls.extensions.contains(&41)
            })
            .cloned()
            .collect();
        if matching.is_empty() {
            let profiles = record_profiles(&records).join(", ");
            return Err(PyRuntimeError::new_err(format!(
                "{source_name}中没有 {impersonate}，可用版本: {profiles}"
            )));
        }
        shared_runtime()?;
        let selected_variant = random_variant(matching.len());
        let state = Arc::new(SessionState {
            profile: normalized,
            variants: matching
                .into_iter()
                .map(|record| Variant {
                    record,
                    session_cache: Arc::new(LruTlsSessionCache::new(8)),
                    client: ArcSwapOption::empty(),
                    client_init: Mutex::new(()),
                })
                .collect(),
            rotation: fingerprint_rotation,
            proxy: ArcSwapOption::from(proxy.map(Arc::new)),
            proxy_identity: ArcSwapOption::<String>::from(proxy_identity.map(Arc::new)),
            default_headers: ArcSwap::from_pointee(parse_headers(default_headers)?),
            verify,
            connect_timeout: parse_optional_timeout("connect_timeout", connect_timeout)?,
            cookie_jar: Arc::new(Jar::default()),
            counter: AtomicUsize::new(0),
            selected_variant: AtomicUsize::new(selected_variant),
            closed: AtomicBool::new(false),
        });
        Ok(Self { state })
    }

    #[getter]
    fn fingerprint_count(&self) -> usize {
        self.state.variants.len()
    }

    // PyO3边界保留显式请求选项，避免把参数塞进不透明字典。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, allow_redirects=true, max_redirects=10, transfer_stats=false))]
    fn request(
        &self,
        py: Python<'_>,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        timeout: f64,
        read_timeout: Option<f64>,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
        allow_redirects: bool,
        max_redirects: usize,
        transfer_stats: bool,
    ) -> PyResult<Py<NativeResponse>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let proxy_url = selected_proxy_url(&state, proxy_override, proxy.clone());
        let proxy = selected_proxy(&state, proxy_override, proxy)?;
        let result = py.detach(move || {
            let variant = &state.variants[index];
            let client = cached_client(&state, variant)?;
            let fingerprint_id = variant.record.id.clone();
            let profile = state.profile.clone();
            runtime.block_on(execute_request(
                client,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy,
                allow_redirects,
                max_redirects,
                fingerprint_id,
                profile,
                transfer_stats,
                proxy_url,
            ))
        })?;
        into_native_response(py, result)
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, allow_redirects=true, max_redirects=10, transfer_stats=false))]
    fn request_async<'py>(
        &self,
        py: Python<'py>,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        timeout: f64,
        read_timeout: Option<f64>,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
        allow_redirects: bool,
        max_redirects: usize,
        transfer_stats: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let proxy_url = selected_proxy_url(&state, proxy_override, proxy.clone());
        let proxy = selected_proxy(&state, proxy_override, proxy)?;
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let client = cached_client_async(state, index).await?;
            let response = execute_request(
                client,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy,
                allow_redirects,
                max_redirects,
                fingerprint_id,
                profile,
                transfer_stats,
                proxy_url,
            )
            .await?;
            Python::attach(|py| into_native_response(py, response))
        })
    }

    // 流式入口与普通入口使用相同选项，确保两种响应模式语义一致。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, allow_redirects=true, max_redirects=10))]
    fn request_stream(
        &self,
        py: Python<'_>,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        timeout: f64,
        read_timeout: Option<f64>,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<NativeStreamResponse> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let proxy = selected_proxy(&state, proxy_override, proxy)?;
        py.detach(move || {
            let variant = &state.variants[index];
            let client = cached_client(&state, variant)?;
            let fingerprint_id = variant.record.id.clone();
            let profile = state.profile.clone();
            runtime.block_on(execute_stream_request(
                client,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy,
                allow_redirects,
                max_redirects,
                fingerprint_id,
                profile,
            ))
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, allow_redirects=true, max_redirects=10))]
    fn request_stream_async<'py>(
        &self,
        py: Python<'py>,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        timeout: f64,
        read_timeout: Option<f64>,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = request_variant(&self.state);
        let method = parse_method(&method)?;
        let headers = prepare_headers(&self.state, &url, headers, cookies)?;
        let proxy = selected_proxy(&self.state, proxy_override, proxy)?;
        let state = self.state.clone();
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let client = cached_client_async(state, index).await?;
            execute_stream_request(
                client,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy,
                allow_redirects,
                max_redirects,
                fingerprint_id,
                profile,
            )
            .await
        })
    }

    // multipart文件由Tokio直接流式读取，Python只传路径和元数据。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, fields, files, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, allow_redirects=true, max_redirects=10))]
    fn request_multipart(
        &self,
        py: Python<'_>,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        fields: Vec<(String, String)>,
        files: Vec<(String, String, Option<String>, Option<String>)>,
        timeout: f64,
        read_timeout: Option<f64>,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<Py<NativeResponse>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let proxy = selected_proxy(&state, proxy_override, proxy)?;
        let result = py.detach(move || {
            let variant = &state.variants[index];
            let client = cached_client(&state, variant)?;
            let fingerprint_id = variant.record.id.clone();
            let profile = state.profile.clone();
            runtime.block_on(execute_multipart_request(
                client,
                method,
                url,
                headers,
                fields,
                files,
                timeout,
                read_timeout,
                proxy,
                allow_redirects,
                max_redirects,
                fingerprint_id,
                profile,
            ))
        })?;
        into_native_response(py, result)
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, fields, files, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, allow_redirects=true, max_redirects=10))]
    fn request_multipart_async<'py>(
        &self,
        py: Python<'py>,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        fields: Vec<(String, String)>,
        files: Vec<(String, String, Option<String>, Option<String>)>,
        timeout: f64,
        read_timeout: Option<f64>,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = request_variant(&self.state);
        let method = parse_method(&method)?;
        let headers = prepare_headers(&self.state, &url, headers, cookies)?;
        let proxy = selected_proxy(&self.state, proxy_override, proxy)?;
        let state = self.state.clone();
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let client = cached_client_async(state, index).await?;
            let response = execute_multipart_request(
                client,
                method,
                url,
                headers,
                fields,
                files,
                timeout,
                read_timeout,
                proxy,
                allow_redirects,
                max_redirects,
                fingerprint_id,
                profile,
            )
            .await?;
            Python::attach(|py| into_native_response(py, response))
        })
    }

    fn set_proxy(&self, proxy: Option<String>) -> PyResult<()> {
        ensure_open(&self.state)?;
        let changed = self
            .state
            .proxy_identity
            .load_full()
            .as_deref()
            .map(String::as_str)
            != proxy.as_deref();
        let proxy_identity = proxy.clone();
        let proxy = proxy
            .map(|value| Proxy::all(&value).map_err(to_py_error))
            .transpose()?;
        if !changed {
            return Ok(());
        }
        self.state
            .proxy_identity
            .store(proxy_identity.map(Arc::new));
        self.state.proxy.store(proxy.map(Arc::new));
        if !self.state.rotation {
            self.state
                .selected_variant
                .store(random_variant(self.state.variants.len()), Ordering::Release);
        }
        // 默认代理切换后不能复用旧 client 的连接池，否则存量代理隧道可能继续承载后续请求。
        // 清空只影响下一次按默认代理发包的懒初始化；正在执行的请求仍持有自己的 Client 快照。
        for variant in &self.state.variants {
            variant.client.store(None);
        }
        Ok(())
    }

    fn select_proxy(
        &self,
        url: String,
        proxies: Vec<(String, Option<String>)>,
    ) -> PyResult<Option<String>> {
        ensure_open(&self.state)?;
        select_proxy_mapping(&url, proxies)
    }

    fn append_query(&self, url: String, params: Vec<(String, Vec<String>)>) -> PyResult<String> {
        ensure_open(&self.state)?;
        append_query(&url, params)
    }

    fn encode_form(&self, params: Vec<(String, Vec<String>)>) -> PyResult<Vec<u8>> {
        ensure_open(&self.state)?;
        Ok(encode_form(params))
    }

    fn encode_json(&self, value: Bound<'_, PyAny>) -> PyResult<Option<Vec<u8>>> {
        ensure_open(&self.state)?;
        json_value(&value)?
            .map(|value| serde_json::to_vec(&value).map_err(to_py_error))
            .transpose()
    }

    fn set_default_headers(&self, headers: Vec<(String, String)>) -> PyResult<()> {
        ensure_open(&self.state)?;
        self.state
            .default_headers
            .store(Arc::new(parse_headers(headers)?));
        Ok(())
    }

    fn set_cookie(&self, cookie: String, url: String) -> PyResult<()> {
        ensure_open(&self.state)?;
        self.state.cookie_jar.add(cookie.as_str(), url.as_str());
        Ok(())
    }

    fn get_cookies(&self) -> PyResult<Vec<NativeCookie>> {
        ensure_open(&self.state)?;
        Ok(self
            .state
            .cookie_jar
            .get_all()
            .map(|cookie| {
                (
                    cookie.name().to_string(),
                    cookie.value().to_string(),
                    cookie.domain().map(str::to_string),
                    cookie.path().map(str::to_string),
                    cookie.secure(),
                    cookie.http_only(),
                )
            })
            .collect())
    }

    fn get_cookie_pairs(&self, url: String) -> PyResult<Vec<(String, String)>> {
        ensure_open(&self.state)?;
        let uri = url.parse::<Uri>().map_err(to_py_error)?;
        let values = match self.state.cookie_jar.cookies(&uri, Version::HTTP_11) {
            RequestCookies::Compressed(value) => vec![value],
            RequestCookies::Uncompressed(values) => values,
            RequestCookies::Empty => Vec::new(),
            _ => Vec::new(),
        };
        let mut result = Vec::new();
        for value in values {
            for item in value.to_str().map_err(to_py_error)?.split(';') {
                if let Some((name, value)) = item.trim().split_once('=') {
                    result.push((name.to_string(), value.to_string()));
                }
            }
        }
        Ok(result)
    }

    fn clear_cookies(&self) -> PyResult<()> {
        ensure_open(&self.state)?;
        self.state.cookie_jar.clear();
        Ok(())
    }

    fn remove_cookie(&self, name: String, url: String) -> PyResult<()> {
        ensure_open(&self.state)?;
        self.state.cookie_jar.remove(name, url.as_str());
        Ok(())
    }

    fn close(&self) -> PyResult<()> {
        if self.state.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        for variant in &self.state.variants {
            variant.client.store(None);
        }
        Ok(())
    }
}

fn history_entries(history: &redirect::History) -> Vec<NativeHistoryEntry> {
    history
        .into_iter()
        .map(|entry| {
            (
                entry.status.as_u16(),
                entry.previous.to_string(),
                entry.uri.to_string(),
                entry
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_string(),
                            value.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

fn to_py_error(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

fn record_profiles(records: &[Arc<Record>]) -> Vec<String> {
    records
        .iter()
        .map(|record| record.profile.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn embedded_profiles() -> Vec<String> {
    embedded_records()
        .map(|records| record_profiles(records))
        .unwrap_or_default()
}

#[pyfunction]
fn available_profiles() -> Vec<String> {
    embedded_profiles()
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    shared_runtime()?;
    let runtime = 共享运行时.get().expect("共享Tokio运行时已经初始化");
    if pyo3_async_runtimes::tokio::init_with_runtime(runtime.as_ref()).is_err()
        && !std::ptr::eq(pyo3_async_runtimes::tokio::get_runtime(), runtime.as_ref())
    {
        return Err(PyRuntimeError::new_err(
            "Python异步桥已经绑定到另一个Tokio运行时",
        ));
    }
    module.add_class::<NativeSession>()?;
    module.add_class::<NativeStreamResponse>()?;
    module.add_class::<NativeHeaders>()?;
    module.add_class::<NativeResponse>()?;
    module.add_function(wrap_pyfunction!(available_profiles, module)?)?;
    module.add_function(wrap_pyfunction!(build_response_headers, module)?)?;
    module.add("readme", 版权说明)?;
    let api_module = PyModule::from_code(
        module.py(),
        API包装源码,
        c"requests_rust._embedded_api",
        c"requests_rust._embedded_api",
    )?;
    for name in [
        "Response",
        "Headers",
        "Cookie",
        "Cookies",
        "CookieTypes",
        "Session",
        "AsyncSession",
        "request",
        "get",
        "post",
        "put",
        "patch",
        "delete",
        "head",
        "options",
    ] {
        module.add(name, api_module.getattr(name)?)?;
    }
    Ok(())
}
