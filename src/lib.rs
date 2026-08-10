use std::{
    collections::{BTreeSet, HashMap},
    io::{self, Write},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use brotli::{CompressorWriter as BrotliEncoder, Decompressor as BrotliDecoder};
use bytes::Bytes;
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use futures_util::{Stream, StreamExt, TryStreamExt};
use pyo3::{
    exceptions::PyRuntimeError,
    prelude::*,
    types::{PyAny, PyBytes, PyModule},
};
use serde::Deserialize;
use tokio::sync::{Mutex as AsyncMutex, Notify};
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
type NativeHistoryEntry = (u16, String, String, Vec<(String, String)>);
type NativeCookie = (String, String, Option<String>, Option<String>, bool, bool);
type NativeResponse = (
    u16,
    Vec<(String, String)>,
    Py<PyBytes>,
    String,
    String,
    String,
    Vec<NativeHistoryEntry>,
);
type RawNativeResponse = (
    u16,
    Vec<(String, String)>,
    Bytes,
    String,
    String,
    String,
    Vec<NativeHistoryEntry>,
);
type NativeBodyStream = Pin<Box<dyn Stream<Item = wreq::Result<Bytes>> + Send>>;
type NativeRequestConfig = (
    String,
    String,
    Vec<(String, String)>,
    Option<Vec<u8>>,
    f64,
    Option<f64>,
    bool,
    Option<String>,
    bool,
    usize,
);

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
    server_name: Option<String>,
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
        .pre_shared_key(capture.extensions.contains(&41))
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
    let ignored = [
        "host",
        "content-length",
        "connection",
        "cookie",
        "authorization",
        "proxy-authorization",
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
    client: Mutex<Option<Client>>,
}

struct SessionState {
    profile: String,
    variants: Vec<Variant>,
    rotation: bool,
    proxy: Mutex<Option<Proxy>>,
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
        .tls_sni(variant.record.tls.server_name.is_some())
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
    if let Some(client) = variant
        .client
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Client缓存锁已损坏"))?
        .clone()
    {
        return Ok(client);
    }
    let client = build_client(state, variant).map_err(to_py_error)?;
    if state.closed.load(Ordering::Acquire) {
        return Ok(client);
    }
    let mut cached = variant
        .client
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Client缓存锁已损坏"))?;
    Ok(cached.get_or_insert_with(|| client.clone()).clone())
}

async fn cached_client_async(state: Arc<SessionState>, index: usize) -> PyResult<Client> {
    if let Some(client) = state.variants[index]
        .client
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Client缓存锁已损坏"))?
        .clone()
    {
        return Ok(client);
    }
    tokio::task::spawn_blocking(move || cached_client(&state, &state.variants[index]))
        .await
        .map_err(|error| PyRuntimeError::new_err(format!("Client构建任务失败: {error}")))?
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

fn batch_error(index: usize, error: PyErr) -> PyErr {
    PyRuntimeError::new_err(format!("批量请求[{index}]失败: {error}"))
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
    state
        .proxy
        .lock()
        .map_err(|_| PyRuntimeError::new_err("代理配置锁已损坏"))
        .map(|proxy| proxy.clone())
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
) -> PyResult<RawNativeResponse> {
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
    let status = response.status().as_u16();
    let final_url = response.uri().to_string();
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
    let content = response.bytes().await.map_err(to_py_error)?;
    Ok((
        status,
        response_headers,
        content,
        fingerprint_id,
        profile,
        final_url,
        history,
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
    let content = response.bytes().await.map_err(to_py_error)?;
    Ok((
        status,
        response_headers,
        content,
        fingerprint_id,
        profile,
        final_url,
        history,
    ))
}

fn into_native_response(py: Python<'_>, response: RawNativeResponse) -> NativeResponse {
    let (status, headers, content, fingerprint_id, profile, url, history) = response;
    (
        status,
        headers,
        PyBytes::new(py, &content).unbind(),
        fingerprint_id,
        profile,
        url,
        history,
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
    #[pyo3(signature = (impersonate, fingerprint_rotation=false, proxy=None, verify=true, connect_timeout=None, fingerprints_path=None))]
    fn new(
        py: Python<'_>,
        impersonate: String,
        fingerprint_rotation: bool,
        proxy: Option<String>,
        verify: bool,
        connect_timeout: Option<f64>,
        fingerprints_path: Option<String>,
    ) -> PyResult<Self> {
        let normalized = normalize_profile(&impersonate);
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
            .filter(|record| normalize_profile(&record.profile) == normalized)
            .cloned()
            .collect();
        if matching.is_empty() {
            let profiles = record_profiles(&records).join(", ");
            return Err(PyRuntimeError::new_err(format!(
                "{source_name}中没有 {impersonate}，可用版本: {profiles}"
            )));
        }
        shared_runtime()?;
        let state = Arc::new(SessionState {
            profile: normalized,
            variants: matching
                .into_iter()
                .map(|record| Variant {
                    record,
                    session_cache: Arc::new(LruTlsSessionCache::new(8)),
                    client: Mutex::new(None),
                })
                .collect(),
            rotation: fingerprint_rotation,
            proxy: Mutex::new(proxy),
            verify,
            connect_timeout: parse_optional_timeout("connect_timeout", connect_timeout)?,
            cookie_jar: Arc::new(Jar::default()),
            counter: AtomicUsize::new(0),
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
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, allow_redirects=true, max_redirects=10))]
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
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<NativeResponse> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = if self.state.rotation {
            self.state.counter.fetch_add(1, Ordering::AcqRel) % self.state.variants.len()
        } else {
            0
        };
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = parse_headers(headers)?;
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
            ))
        })?;
        Ok(into_native_response(py, result))
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, allow_redirects=true, max_redirects=10))]
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
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = if self.state.rotation {
            self.state.counter.fetch_add(1, Ordering::AcqRel) % self.state.variants.len()
        } else {
            0
        };
        let state = self.state.clone();
        let method = parse_method(&method)?;
        let headers = parse_headers(headers)?;
        let proxy = selected_proxy(&state, proxy_override, proxy)?;
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let client = cached_client_async(state, index).await?;
            execute_request(
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

    // 批量入口预先固定每条请求的指纹序号，再在Rust运行时内限制并发。
    #[pyo3(signature = (requests, concurrency))]
    fn request_batch_async<'py>(
        &self,
        py: Python<'py>,
        requests: Vec<NativeRequestConfig>,
        concurrency: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        if concurrency == 0 {
            return Err(PyRuntimeError::new_err("concurrency必须大于0"));
        }
        let mut prepared = Vec::with_capacity(requests.len());
        for (position, request) in requests.into_iter().enumerate() {
            let (
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy_override,
                proxy,
                allow_redirects,
                max_redirects,
            ) = request;
            let index = if self.state.rotation {
                self.state.counter.fetch_add(1, Ordering::AcqRel) % self.state.variants.len()
            } else {
                0
            };
            prepared.push((
                position,
                index,
                parse_method(&method).map_err(|error| batch_error(position, error))?,
                url,
                parse_headers(headers).map_err(|error| batch_error(position, error))?,
                body,
                parse_timeout("timeout", timeout).map_err(|error| batch_error(position, error))?,
                parse_optional_timeout("read_timeout", read_timeout)
                    .map_err(|error| batch_error(position, error))?,
                selected_proxy(&self.state, proxy_override, proxy)
                    .map_err(|error| batch_error(position, error))?,
                allow_redirects,
                max_redirects,
                self.state.variants[index].record.id.clone(),
                self.state.profile.clone(),
            ));
        }
        let state = self.state.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let request_count = prepared.len();
            let mut clients = HashMap::new();
            let mut first_positions = HashMap::new();
            for request in &prepared {
                first_positions.entry(request.1).or_insert(request.0);
            }
            for (variant_index, position) in first_positions {
                let client = cached_client_async(state.clone(), variant_index)
                    .await
                    .map_err(|error| batch_error(position, error))?;
                clients.insert(variant_index, client);
            }
            let requests = futures_util::stream::iter(prepared.into_iter().map(|request| {
                let client = clients
                    .get(&request.1)
                    .expect("批量请求使用的Client已经准备完成")
                    .clone();
                async move {
                    let (
                        position,
                        _variant_index,
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
                    ) = request;
                    execute_request(
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
                    .map_err(|error| {
                        PyRuntimeError::new_err(format!("批量请求[{position}]失败: {error}"))
                    })
                    .map(|response| (position, response))
                }
            }))
            .buffer_unordered(concurrency);
            futures_util::pin_mut!(requests);
            let mut ordered = std::iter::repeat_with(|| None)
                .take(request_count)
                .collect::<Vec<_>>();
            while let Some((position, response)) = requests.try_next().await? {
                ordered[position] = Some(response);
            }
            Ok(ordered
                .into_iter()
                .map(|response| response.expect("批量请求结果已经完整收集"))
                .collect::<Vec<_>>())
        })
    }

    // 流式入口与普通入口使用相同选项，确保两种响应模式语义一致。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, allow_redirects=true, max_redirects=10))]
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
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<NativeStreamResponse> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = if self.state.rotation {
            self.state.counter.fetch_add(1, Ordering::AcqRel) % self.state.variants.len()
        } else {
            0
        };
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = parse_headers(headers)?;
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
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, allow_redirects=true, max_redirects=10))]
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
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = if self.state.rotation {
            self.state.counter.fetch_add(1, Ordering::AcqRel) % self.state.variants.len()
        } else {
            0
        };
        let method = parse_method(&method)?;
        let headers = parse_headers(headers)?;
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
    #[pyo3(signature = (method, url, headers, fields, files, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, allow_redirects=true, max_redirects=10))]
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
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<NativeResponse> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = if self.state.rotation {
            self.state.counter.fetch_add(1, Ordering::AcqRel) % self.state.variants.len()
        } else {
            0
        };
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = parse_headers(headers)?;
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
        Ok(into_native_response(py, result))
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, fields, files, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, allow_redirects=true, max_redirects=10))]
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
        allow_redirects: bool,
        max_redirects: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let index = if self.state.rotation {
            self.state.counter.fetch_add(1, Ordering::AcqRel) % self.state.variants.len()
        } else {
            0
        };
        let method = parse_method(&method)?;
        let headers = parse_headers(headers)?;
        let proxy = selected_proxy(&self.state, proxy_override, proxy)?;
        let state = self.state.clone();
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let client = cached_client_async(state, index).await?;
            execute_multipart_request(
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
            .await
        })
    }

    fn set_proxy(&self, proxy: Option<String>) -> PyResult<()> {
        ensure_open(&self.state)?;
        let proxy = proxy
            .map(|value| Proxy::all(&value).map_err(to_py_error))
            .transpose()?;
        let mut current = self
            .state
            .proxy
            .lock()
            .map_err(|_| PyRuntimeError::new_err("代理配置锁已损坏"))?;
        *current = proxy;
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
            let mut client = variant
                .client
                .lock()
                .map_err(|_| PyRuntimeError::new_err("Client缓存锁已损坏"))?;
            *client = None;
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
    module.add_function(wrap_pyfunction!(available_profiles, module)?)?;
    Ok(())
}
