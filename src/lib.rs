mod profile_http3;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    future::Future,
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, Write},
    net::{IpAddr, SocketAddr},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, LazyLock, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use arc_swap::{ArcSwap, ArcSwapOption};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use brotli::{CompressorWriter as BrotliEncoder, Decompressor as BrotliDecoder};
use bytes::{Bytes, BytesMut};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use futures_util::{Stream, StreamExt};
use hickory_resolver::{
    TokioResolver,
    config::{ConnectionConfig, LookupIpStrategy, NameServerConfig, ResolverConfig},
    net::runtime::TokioRuntimeProvider,
};
use lru::LruCache;
use once_cell::sync::OnceCell;
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
    sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
};
use url::Url;
use wreq::{
    Client, Emulation, Method, Proxy, Uri, Version,
    cookie::{CookieStore, Cookies as RequestCookies, Jar},
    dns::{Addrs as DnsAddrs, Name as DnsName, Resolve as DnsResolve, Resolving},
    header::{
        AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderMap, HeaderName,
        HeaderValue, OrigHeaderMap, PROXY_AUTHORIZATION,
    },
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
    ws::{
        WebSocket,
        message::{CloseCode, CloseFrame, Message, Utf8Bytes},
    },
};

#[derive(Debug, Clone)]
struct CustomDnsResolver {
    resolver: TokioResolver,
}

impl DnsResolve for CustomDnsResolver {
    fn resolve(&self, name: DnsName) -> Resolving {
        let resolver = self.resolver.clone();
        Box::pin(async move {
            let lookup = resolver.lookup_ip(name.as_str()).await?;
            let addrs: DnsAddrs = Box::new(
                lookup
                    .iter()
                    .map(|ip| SocketAddr::new(ip, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(addrs)
        })
    }
}

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
    completed: Option<tokio::task::JoinHandle<()>>,
}

impl TransferMeter {
    async fn finish(&mut self) {
        if let Some(completed) = self.completed.take() {
            // 响应Body已经完整读完，计数字节已穿过meter；主动结束隧道，避免连接池保活阻塞等待。
            tokio::task::yield_now().await;
            completed.abort();
            let _ = completed.await;
        }
    }
}

impl Drop for TransferMeter {
    fn drop(&mut self) {
        if let Some(completed) = self.completed.take() {
            completed.abort();
        }
    }
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
    http_version: String,
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
type NativeBodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

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

// Python Future完成由单线程串行回投事件循环，避免与冷Client构建争用Tokio阻塞池。
type PythonCompletionJob = Box<dyn FnOnce() + Send + 'static>;
static PYTHON完成任务发送端: LazyLock<
    Result<std::sync::mpsc::Sender<PythonCompletionJob>, String>,
> = LazyLock::new(|| {
    let (sender, receiver) = std::sync::mpsc::channel::<PythonCompletionJob>();
    std::thread::Builder::new()
        .name("requests-rust-python-completion".to_string())
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            }
        })
        .map_err(|error| format!("无法创建Python异步完成线程: {error}"))?;
    Ok(sender)
});

fn python_completion_sender() -> PyResult<&'static std::sync::mpsc::Sender<PythonCompletionJob>> {
    PYTHON完成任务发送端
        .as_ref()
        .map_err(|error| PyRuntimeError::new_err(error.clone()))
}

struct RequestsRustAsyncRuntime;

impl pyo3_async_runtimes::generic::Runtime for RequestsRustAsyncRuntime {
    type JoinError = tokio::task::JoinError;
    type JoinHandle = tokio::task::JoinHandle<()>;

    fn spawn<F>(future: F) -> Self::JoinHandle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        共享运行时
            .get()
            .expect("共享Tokio运行时已经初始化")
            .spawn(future)
    }

    fn spawn_blocking<F>(function: F) -> Self::JoinHandle
    where
        F: FnOnce() + Send + 'static,
    {
        let completion_thread_stopped = PYTHON完成任务发送端
            .as_ref()
            .expect("Python异步完成线程已经初始化")
            .send(Box::new(function))
            .is_err();
        Self::spawn(async move {
            assert!(!completion_thread_stopped, "Python异步完成线程已经停止");
        })
    }
}

impl pyo3_async_runtimes::generic::ContextExt for RequestsRustAsyncRuntime {
    fn scope<F, R>(
        _locals: pyo3_async_runtimes::TaskLocals,
        future: F,
    ) -> Pin<Box<dyn Future<Output = R> + Send>>
    where
        F: Future<Output = R> + Send + 'static,
    {
        Box::pin(future)
    }

    fn get_task_locals() -> Option<pyo3_async_runtimes::TaskLocals> {
        None
    }
}

fn rust_future_into_py<'py, F, T>(py: Python<'py>, future: F) -> PyResult<Bound<'py, PyAny>>
where
    F: Future<Output = PyResult<T>> + Send + 'static,
    T: for<'target> IntoPyObject<'target> + Send + 'static,
{
    python_completion_sender()?;
    let locals = pyo3_async_runtimes::TaskLocals::with_running_loop(py)?;
    pyo3_async_runtimes::generic::future_into_py_with_locals::<RequestsRustAsyncRuntime, _, T>(
        py, locals, future,
    )
}

fn parse_records(source: &str) -> Result<Vec<Arc<Record>>> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RecordDocument {
        Records(Vec<Record>),
        Envelope {
            schema_version: Option<u32>,
            records: Vec<Record>,
        },
    }

    let document = serde_json::from_str::<RecordDocument>(source)
        .context("JSON结构不是指纹记录列表或包含records的捕获器结果")?;
    let records = match document {
        RecordDocument::Records(records) => records,
        RecordDocument::Envelope {
            schema_version,
            records,
        } => {
            validate_schema_version(schema_version, "捕获结果")?;
            if let Some(schema_version) = schema_version
                && records
                    .iter()
                    .any(|record| record.schema_version != Some(schema_version))
            {
                bail!("捕获结果schema_version={schema_version}与内部指纹记录不一致")
            }
            records
        }
    };
    if records.is_empty() {
        bail!("指纹记录列表不能为空")
    }
    for record in &records {
        validate_record(record)?;
    }
    Ok(records.into_iter().map(Arc::new).collect())
}

fn embedded_records() -> PyResult<&'static Vec<Arc<Record>>> {
    指纹记录缓存
        .get_or_init(|| parse_records(内置指纹).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| PyRuntimeError::new_err(format!("内置指纹无效: {error}")))
}

#[derive(Debug, Clone, Deserialize)]
struct Record {
    #[serde(default)]
    schema_version: Option<u32>,
    id: String,
    profile: String,
    tls: TlsCapture,
    http: HttpCapture,
    #[serde(default)]
    http3: Option<profile_http3::ProfileHttp3>,
    #[serde(skip)]
    emulation: OnceLock<Result<Emulation, String>>,
}

#[derive(Debug, Clone, Deserialize)]
struct TlsCapture {
    #[serde(rename = "server_name")]
    _server_name: Option<String>,
    cipher_suites: Vec<u16>,
    extensions: Vec<u16>,
    #[serde(default)]
    extension_wire: Vec<TlsExtensionWire>,
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
struct TlsExtensionWire {
    id: u16,
    length: usize,
    payload_base64: String,
}

#[derive(Debug, Clone, Deserialize)]
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

fn validate_schema_version(version: Option<u32>, scope: &str) -> Result<()> {
    if let Some(version) = version
        && !matches!(version, 1 | 2)
    {
        bail!("{scope}使用不支持的schema_version={version}，当前仅支持1和2")
    }
    Ok(())
}

fn is_grease_value(value: u16) -> bool {
    value & 0x0f0f == 0x0a0a
}

fn ensure_unique_ids(values: &[u16], scope: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for value in values {
        if !is_grease_value(*value) && !seen.insert(*value) {
            bail!("{scope}包含重复ID 0x{value:04x}")
        }
    }
    Ok(())
}

fn validate_record(record: &Record) -> Result<()> {
    validate_schema_version(record.schema_version, &format!("指纹{}", record.id))?;
    match (record.schema_version, record.http3.as_ref()) {
        (Some(2), Some(http3)) => profile_http3::validate(http3)?,
        (Some(2), None) => bail!("schema_version=2的指纹必须包含完整http3模板"),
        (_, Some(_)) => bail!("http3模板只能出现在schema_version=2指纹中"),
        _ => {}
    }
    if record.id.trim().is_empty() || record.profile.trim().is_empty() {
        bail!("指纹id和profile不能为空")
    }

    let tls = &record.tls;
    ensure_unique_ids(&tls.cipher_suites, "cipher_suites")?;
    ensure_unique_ids(&tls.extensions, "extensions")?;
    ensure_unique_ids(&tls.supported_groups, "supported_groups")?;
    ensure_unique_ids(&tls.key_share_groups, "key_share_groups")?;
    for (scope, values, supported) in [
        (
            "cipher_suites",
            tls.cipher_suites.as_slice(),
            cipher_name as fn(u16) -> Option<&'static str>,
        ),
        (
            "supported_groups",
            tls.supported_groups.as_slice(),
            group_name,
        ),
        (
            "signature_algorithms",
            tls.signature_algorithms.as_slice(),
            signature_name,
        ),
        (
            "delegated_credentials",
            tls.delegated_credentials.as_slice(),
            signature_name,
        ),
    ] {
        let unknown = values
            .iter()
            .copied()
            .filter(|value| !is_grease_value(*value) && supported(*value).is_none())
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            bail!("{scope}包含尚不能等价回放的ID: {unknown:?}")
        }
    }
    let unknown_key_shares = tls
        .key_share_groups
        .iter()
        .copied()
        .filter(|value| !is_grease_value(*value) && key_share(*value).is_none())
        .collect::<Vec<_>>();
    if !unknown_key_shares.is_empty() {
        bail!("key_share_groups包含尚不能等价回放的ID: {unknown_key_shares:?}")
    }
    let unknown_extensions = tls
        .extensions
        .iter()
        .copied()
        .filter(|value| !is_grease_value(*value) && extension_type(*value).is_none())
        .collect::<Vec<_>>();
    if !unknown_extensions.is_empty() {
        bail!("extensions包含尚不能等价回放的ID: {unknown_extensions:?}")
    }
    let unknown_compressors = tls
        .certificate_compression_algorithms
        .iter()
        .copied()
        .filter(|value| !matches!(value, 1..=3))
        .collect::<Vec<_>>();
    if !unknown_compressors.is_empty() {
        bail!("certificate_compression_algorithms包含未知ID: {unknown_compressors:?}")
    }
    let unknown_alpn = tls
        .alpn
        .iter()
        .filter(|value| !matches!(value.as_str(), "h2" | "http/1.1"))
        .collect::<Vec<_>>();
    if !unknown_alpn.is_empty() {
        bail!("alpn包含尚不能回放的协议: {unknown_alpn:?}")
    }

    if !tls.extension_wire.is_empty() {
        let mut wire_ids = Vec::new();
        for extension in &tls.extension_wire {
            let payload = BASE64
                .decode(&extension.payload_base64)
                .with_context(|| format!("TLS扩展0x{:04x}的payload_base64无效", extension.id))?;
            if payload.len() != extension.length {
                bail!(
                    "TLS扩展0x{:04x}声明长度{}，实际payload长度{}",
                    extension.id,
                    extension.length,
                    payload.len()
                )
            }
            if !is_grease_value(extension.id) {
                wire_ids.push(extension.id);
            }
        }
        if wire_ids != tls.extensions {
            bail!("extension_wire与extensions的非GREASE顺序不一致")
        }
    }
    requested_trust_anchors(tls)?;

    let http = &record.http;
    if !matches!(http.protocol.as_str(), "HTTP/1.1" | "HTTP/2") {
        bail!(
            "捕获协议{}尚不能作为浏览器传输指纹回放；HTTP/3当前仅提供通用QUIC传输",
            http.protocol
        )
    }
    if !http.header_order.is_empty() {
        let actual = http
            .headers
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let declared = http
            .header_order
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if actual != declared {
            bail!("http.header_order与http.headers中的实际顺序不一致")
        }
    }
    let mut setting_ids = HashSet::new();
    for setting in &http.settings {
        if !setting_ids.insert(setting.id) {
            bail!("HTTP/2 SETTINGS包含重复ID {}", setting.id)
        }
        if !(1..=6).contains(&setting.id) {
            bail!("HTTP/2 SETTINGS ID {}尚不能等价回放", setting.id)
        }
    }
    for name in &http.pseudo_header_order {
        if !matches!(
            name.as_str(),
            ":method" | ":path" | ":authority" | ":scheme"
        ) {
            bail!("未知HTTP/2伪Header: {name}")
        }
    }
    for priority in &http.priorities {
        if !matches!(priority.source.as_str(), "HEADERS" | "PRIORITY")
            || priority.exclusive > 1
            || !(1..=256).contains(&priority.weight)
        {
            bail!("HTTP/2 priority字段无法等价回放: {priority:?}")
        }
    }
    build_emulation(record).map(|_| ())
}

fn requested_trust_anchors(capture: &TlsCapture) -> Result<Option<Vec<u8>>> {
    let mut matches = capture
        .extension_wire
        .iter()
        .filter(|extension| extension.id == 0xca34);
    let wire = matches.next();
    if matches.next().is_some() {
        bail!("TLS扩展0xca34重复")
    }
    if !capture.extensions.contains(&0xca34) {
        if wire.is_some() {
            bail!("extension_wire包含0xca34但extensions未声明")
        }
        return Ok(None);
    }
    let wire = wire.context("TLS扩展0xca34缺少extension_wire payload，不能等价回放")?;
    let payload = BASE64
        .decode(&wire.payload_base64)
        .context("TLS扩展0xca34的payload_base64无效")?;
    if payload.len() < 2 {
        bail!("TLS扩展0xca34 payload缺少外层列表长度")
    }
    let declared = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let ids = &payload[2..];
    if declared != ids.len() {
        bail!("TLS扩展0xca34列表声明长度{declared}，实际长度{}", ids.len())
    }
    let mut offset = 0;
    while offset < ids.len() {
        let length = ids[offset] as usize;
        offset += 1;
        if length == 0 || offset + length > ids.len() {
            bail!("TLS扩展0xca34包含空或截断的Trust Anchor ID")
        }
        offset += length;
    }
    Ok(Some(ids.to_vec()))
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
    if is_grease_value(id) {
        return Some("grease");
    }
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
        0xca34 => ExtensionType::TRUST_ANCHORS,
        _ => return None,
    })
}

fn build_tls(capture: &TlsCapture, permute_extensions: bool) -> Result<TlsOptions> {
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
        .permute_extensions(permute_extensions)
        .preserve_tls13_cipher_list(true)
        .aes_hw_override(true)
        .record_size_limit(capture.record_size_limit);

    if let Some(ids) = requested_trust_anchors(capture)? {
        builder = builder.requested_trust_anchors(ids);
    }

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
    if !permute_extensions {
        let extension_order: Vec<_> = capture
            .extensions
            .iter()
            .filter_map(|id| extension_type(*id))
            .collect();
        if !extension_order.is_empty() {
            builder = builder.extension_permutation(extension_order);
        }
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
    // 只固化与浏览器/平台稳定绑定的传输Header。业务、认证和导航上下文必须由调用方传入。
    let stable = [
        "user-agent",
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
        "sec-ch-ua-arch",
        "sec-ch-ua-bitness",
        "sec-ch-ua-full-version",
        "sec-ch-ua-full-version-list",
        "sec-ch-ua-model",
        "sec-ch-ua-platform-version",
        "sec-ch-ua-wow64",
        "accept-encoding",
        "accept-language",
        "dnt",
        "sec-gpc",
        "te",
    ];
    let mut headers = HeaderMap::new();
    let mut original = OrigHeaderMap::new();
    let order = if capture.header_order.is_empty() {
        capture
            .headers
            .iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    } else {
        capture.header_order.iter().collect::<Vec<_>>()
    };
    for name in order {
        original.insert(name.clone());
    }
    for (name, value) in &capture.headers {
        if !stable.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        headers.append(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok((headers, original))
}

fn is_chromium_profile(profile: &str) -> bool {
    let normalized = normalize_profile(profile);
    normalized.starts_with("chrome") || normalized.starts_with("edge")
}

fn build_emulation(record: &Record) -> Result<Emulation> {
    let (headers, original) = build_headers(&record.http)?;
    let mut builder = Emulation::builder()
        .tls_options(build_tls(
            &record.tls,
            is_chromium_profile(&record.profile),
        )?)
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
    // 请求级DNS配置按指纹变体隔离并有界复用，避免热路径重复构建Client和Resolver。
    dns_clients: Mutex<LruCache<String, Arc<OnceCell<Client>>>>,
}

struct DefaultProxy {
    identity: String,
    proxy: Proxy,
}

struct CachedOriginState {
    pending: usize,
    committed: bool,
}

#[derive(Clone, Copy)]
struct Http3Capability {
    available: bool,
    expires_at: Instant,
}

struct SessionState {
    profile: String,
    variants: Vec<Variant>,
    rotation: bool,
    fingerprint_pool: bool,
    fingerprint_pool_size: usize,
    // 扩池只发生在首次命中阶段；池满后通过ArcSwap快照无锁随机选择。
    fingerprint_pool_members: ArcSwap<Vec<usize>>,
    fingerprint_pool_init: Mutex<()>,
    max_cached_origins: usize,
    cached_origins: Mutex<HashMap<String, CachedOriginState>>,
    client_pool_max_size: usize,
    // 固定模式下当前代理会话绑定的指纹变体；切换代理会话时重新随机选择。
    selected_variant: AtomicUsize,
    // 默认代理URL身份和Proxy对象必须来自同一原子快照。
    default_proxy: ArcSwapOption<DefaultProxy>,
    proxy_generation: AtomicU64,
    // Session默认请求头构造期预解析，普通请求只合并请求级增量头。
    default_headers: ArcSwap<HeaderMap>,
    verify: bool,
    connect_timeout: Option<Duration>,
    happy_eyeballs_timeout: Option<Duration>,
    dns_resolver: Option<CustomDnsResolver>,
    dns_overrides: Arc<Vec<(String, Vec<SocketAddr>)>>,
    // schema 2模板连接按指纹变体和origin隔离，并在Session生命周期内复用。
    profile_http3_clients: Mutex<LruCache<String, Arc<Mutex<Option<profile_http3::H3Client>>>>>,
    // QUIC能力按origin和请求级DNS路由缓存；短期负缓存避免每次都等待UDP超时。
    http3_capabilities: Mutex<LruCache<String, Http3Capability>>,
    cookie_jar: Arc<Jar>,
    cookie_store: bool,
    closed: AtomicBool,
    connection_slots: Arc<Semaphore>,
    max_response_bytes: usize,
    max_websocket_message_bytes: usize,
}

async fn acquire_connection_slot(state: &Arc<SessionState>) -> PyResult<OwnedSemaphorePermit> {
    state
        .connection_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| PyRuntimeError::new_err("Session已经关闭"))
}

fn normalize_profile(profile: &str) -> String {
    profile
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn build_client_with_dns(
    state: &SessionState,
    variant: &Variant,
    dns_resolver: Option<&CustomDnsResolver>,
    dns_override: bool,
) -> Result<Client> {
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
        .tcp_happy_eyeballs_timeout(state.happy_eyeballs_timeout)
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(state.client_pool_max_size)
        .pool_max_size(state.client_pool_max_size);
    if state.cookie_store {
        builder = builder.cookie_provider(state.cookie_jar.clone());
    }
    let resolver = if dns_override {
        dns_resolver
    } else {
        state.dns_resolver.as_ref()
    };
    if let Some(resolver) = resolver {
        builder = builder.dns_resolver(resolver.clone());
    }
    for (domain, addrs) in state.dns_overrides.iter() {
        builder = builder.resolve_to_addrs(domain.clone(), addrs.clone());
    }
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

fn build_client(state: &SessionState, variant: &Variant) -> Result<Client> {
    build_client_with_dns(state, variant, None, false)
}

fn cache_route_allowed(
    state: &SessionState,
    url: &str,
    proxy_identity: Option<&str>,
) -> PyResult<(bool, Option<String>)> {
    if state.max_cached_origins == 0 {
        return Ok((false, None));
    }
    let uri = url.parse::<Uri>().map_err(to_py_error)?;
    let scheme = uri.scheme_str().unwrap_or("http");
    let host = uri
        .host()
        .ok_or_else(|| PyRuntimeError::new_err("URL缺少主机"))?;
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_ascii_lowercase()
    };
    let port = uri
        .port_u16()
        .unwrap_or(if matches!(scheme, "https" | "wss") {
            443
        } else {
            80
        });
    let proxy_key = if let Some(proxy_identity) = proxy_identity {
        let mut hasher = DefaultHasher::new();
        proxy_identity.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    } else {
        "DIRECT".to_string()
    };
    let route = format!("{scheme}://{host}:{port}|{proxy_key}");
    let mut cached = state
        .cached_origins
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Origin缓存锁已损坏"))?;
    if let Some(state) = cached.get_mut(&route) {
        if state.committed {
            return Ok((true, None));
        }
        state.pending += 1;
        return Ok((true, Some(route)));
    }
    if cached.len() >= state.max_cached_origins {
        return Ok((false, None));
    }
    cached.insert(
        route.clone(),
        CachedOriginState {
            pending: 1,
            committed: false,
        },
    );
    Ok((true, Some(route)))
}

struct CachedOriginReservation {
    state: Arc<SessionState>,
    route: Option<String>,
}

impl CachedOriginReservation {
    fn new(state: Arc<SessionState>, route: Option<String>) -> Self {
        Self { state, route }
    }

    fn commit(&mut self) {
        if let Some(route) = self.route.take()
            && let Ok(mut cached) = self.state.cached_origins.lock()
            && let Some(route_state) = cached.get_mut(&route)
        {
            route_state.committed = true;
            route_state.pending = route_state.pending.saturating_sub(1);
        }
    }
}

impl Drop for CachedOriginReservation {
    fn drop(&mut self) {
        if let Some(route) = self.route.take()
            && let Ok(mut cached) = self.state.cached_origins.lock()
            && let Some(route_state) = cached.get_mut(&route)
        {
            route_state.pending = route_state.pending.saturating_sub(1);
            if route_state.pending == 0 && !route_state.committed {
                cached.remove(&route);
            }
        }
    }
}

fn request_dns_client(
    state: &SessionState,
    variant: &Variant,
    dns_servers: Vec<String>,
    dns_timeout: Option<f64>,
    proxy_generation: u64,
) -> PyResult<Client> {
    let normalized = dns_servers
        .iter()
        .map(|value| {
            parse_dns_server(value).map(|(ip, port)| SocketAddr::new(ip, port).to_string())
        })
        .collect::<PyResult<Vec<_>>>()?;
    let key = format!("{}|{dns_timeout:?}", normalized.join(","));
    if !state.fingerprint_pool {
        let resolver = build_custom_dns_resolver(normalized, dns_timeout)?;
        return build_client_with_dns(state, variant, resolver.as_ref(), true).map_err(to_py_error);
    }
    let cell = {
        let mut clients = variant
            .dns_clients
            .lock()
            .map_err(|_| PyRuntimeError::new_err("请求级DNS Client缓存锁已损坏"))?;
        if let Some(cell) = clients.get(&key) {
            cell.clone()
        } else {
            let cell = Arc::new(OnceCell::new());
            clients.put(key.clone(), cell.clone());
            cell
        }
    };
    let client = cell
        .get_or_try_init(|| {
            let resolver = build_custom_dns_resolver(normalized, dns_timeout)?;
            build_client_with_dns(state, variant, resolver.as_ref(), true).map_err(to_py_error)
        })
        .cloned()?;
    if state.proxy_generation.load(Ordering::Acquire) != proxy_generation
        && let Ok(mut clients) = variant.dns_clients.lock()
        && clients
            .peek(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &cell))
    {
        clients.pop(&key);
    }
    Ok(client)
}

fn touch_fingerprint_pool(state: &SessionState, index: usize) -> PyResult<()> {
    if !state.fingerprint_pool {
        return Ok(());
    }
    let members = state.fingerprint_pool_members.load_full();
    if members.contains(&index) || members.len() >= state.fingerprint_pool_size {
        return Ok(());
    }
    let _guard = state
        .fingerprint_pool_init
        .lock()
        .map_err(|_| PyRuntimeError::new_err("指纹连接池初始化锁已损坏"))?;
    let current = state.fingerprint_pool_members.load_full();
    if current.contains(&index) || current.len() >= state.fingerprint_pool_size {
        return Ok(());
    }
    let mut next = (*current).clone();
    next.push(index);
    state.fingerprint_pool_members.store(Arc::new(next));
    Ok(())
}

fn cached_client(state: &SessionState, index: usize, proxy_generation: u64) -> PyResult<Client> {
    let variant = &state.variants[index];
    if !state.fingerprint_pool {
        return build_client(state, variant).map_err(to_py_error);
    }
    if let Some(client) = variant.client.load_full() {
        touch_fingerprint_pool(state, index)?;
        return Ok((*client).clone());
    }
    let _init_guard = variant
        .client_init
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Client初始化锁已损坏"))?;
    if let Some(client) = variant.client.load_full() {
        touch_fingerprint_pool(state, index)?;
        return Ok((*client).clone());
    }
    let client = build_client(state, variant).map_err(to_py_error)?;
    if state.closed.load(Ordering::Acquire) {
        return Ok(client);
    }
    if state.proxy_generation.load(Ordering::Acquire) == proxy_generation {
        variant.client.store(Some(Arc::new(client.clone())));
        touch_fingerprint_pool(state, index)?;
    }
    Ok(client)
}

async fn cached_client_async(
    state: Arc<SessionState>,
    index: usize,
    proxy_generation: u64,
) -> PyResult<Client> {
    if let Some(client) = state.variants[index].client.load_full() {
        return Ok((*client).clone());
    }
    tokio::task::spawn_blocking(move || cached_client(&state, index, proxy_generation))
        .await
        .map_err(|error| PyRuntimeError::new_err(format!("Client构建任务失败: {error}")))?
}

fn selected_request_client(
    state: &SessionState,
    index: usize,
    dns_override: bool,
    dns_servers: Vec<String>,
    dns_timeout: Option<f64>,
    cache_route: bool,
    proxy_generation: u64,
) -> PyResult<Client> {
    let variant = &state.variants[index];
    if !cache_route {
        let resolver = if dns_override {
            build_custom_dns_resolver(dns_servers, dns_timeout)?
        } else {
            None
        };
        return build_client_with_dns(state, variant, resolver.as_ref(), dns_override)
            .map_err(to_py_error);
    }
    if dns_override {
        request_dns_client(state, variant, dns_servers, dns_timeout, proxy_generation)
    } else {
        cached_client(state, index, proxy_generation)
    }
}

async fn selected_request_client_async(
    state: Arc<SessionState>,
    index: usize,
    dns_override: bool,
    dns_servers: Vec<String>,
    dns_timeout: Option<f64>,
    cache_route: bool,
    proxy_generation: u64,
) -> PyResult<Client> {
    if !dns_override && cache_route {
        return cached_client_async(state, index, proxy_generation).await;
    }
    tokio::task::spawn_blocking(move || {
        selected_request_client(
            &state,
            index,
            dns_override,
            dns_servers,
            dns_timeout,
            cache_route,
            proxy_generation,
        )
    })
    .await
    .map_err(|error| PyRuntimeError::new_err(format!("请求级DNS Client构建任务失败: {error}")))?
}

fn random_variant(count: usize) -> usize {
    rand::rng().random_range(0..count)
}

fn request_variant(state: &SessionState) -> usize {
    if !state.rotation {
        return state.selected_variant.load(Ordering::Acquire);
    }
    if !state.fingerprint_pool {
        return rand::rng().random_range(0..state.variants.len());
    }
    let target_size = state.fingerprint_pool_size.min(state.variants.len());
    let members = state.fingerprint_pool_members.load_full();
    if members.len() < target_size {
        let Ok(_guard) = state.fingerprint_pool_init.lock() else {
            return rand::rng().random_range(0..state.variants.len());
        };
        let current = state.fingerprint_pool_members.load_full();
        if current.len() >= target_size {
            return current[rand::rng().random_range(0..current.len())];
        }
        let available: Vec<_> = (0..state.variants.len())
            .filter(|index| !current.contains(index))
            .collect();
        if !available.is_empty() {
            let index = available[rand::rng().random_range(0..available.len())];
            let mut next = (*current).clone();
            next.push(index);
            state.fingerprint_pool_members.store(Arc::new(next));
            return index;
        }
    }
    members[rand::rng().random_range(0..members.len())]
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

fn selected_proxy_snapshot(
    state: &SessionState,
    proxy_override: bool,
    proxy: Option<String>,
) -> PyResult<(Option<String>, Option<Proxy>, u64)> {
    let generation = state.proxy_generation.load(Ordering::Acquire);
    if proxy_override {
        let parsed = proxy
            .as_ref()
            .map(|value| Proxy::all(value).map_err(to_py_error))
            .transpose()?;
        return Ok((proxy, parsed, generation));
    }
    let snapshot = state.default_proxy.load_full();
    Ok(snapshot.map_or((None, None, generation), |value| {
        (
            Some(value.identity.clone()),
            Some(value.proxy.clone()),
            generation,
        )
    }))
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

async fn read_meter_line<R>(reader: &mut R, total: &mut usize) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    const MAX_CONNECT_HEAD: usize = 64 * 1024;
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).await?;
    if line.is_empty() {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
    }
    *total = total.saturating_add(line.len());
    if *total > MAX_CONNECT_HEAD {
        return Err(std::io::Error::other("CONNECT请求头超过64KiB"));
    }
    Ok(line)
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
    let mut request_head_bytes = 0;
    let Ok(request_head) = read_meter_line(&mut reader, &mut request_head_bytes).await else {
        return;
    };
    let request_line = String::from_utf8_lossy(&request_head);
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("CONNECT") {
        return;
    }
    let Some(target) = parts.next() else {
        return;
    };
    loop {
        let Ok(line) = read_meter_line(&mut reader, &mut request_head_bytes).await else {
            return;
        };
        if line == b"\r\n" {
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
        let Ok(mut stream) = TcpStream::connect((proxy_host, parsed.port().unwrap_or(80))).await
        else {
            return;
        };
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let mut connect = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
        if !parsed.username().is_empty() || parsed.password().is_some() {
            let password = parsed.password().unwrap_or_default();
            let auth = BASE64.encode(format!("{}:{password}", parsed.username()));
            connect.push_str(&format!("Proxy-Authorization: Basic {auth}\r\n"));
        }
        connect.push_str("\r\n");
        if stream.write_all(connect.as_bytes()).await.is_err() {
            return;
        }
        counters
            .upload
            .fetch_add(connect.len() as u64, Ordering::Relaxed);
        let mut upstream_reader = BufReader::new(stream);
        let mut response_head_bytes = 0;
        let Ok(status) = read_meter_line(&mut upstream_reader, &mut response_head_bytes).await
        else {
            return;
        };
        if !String::from_utf8_lossy(&status).contains(" 200 ") {
            return;
        }
        let mut connect_response_bytes = status.len();
        loop {
            let Ok(line) = read_meter_line(&mut upstream_reader, &mut response_head_bytes).await
            else {
                return;
            };
            if line == b"\r\n" {
                break;
            }
            connect_response_bytes += line.len();
        }
        counters
            .download
            .fetch_add(connect_response_bytes as u64, Ordering::Relaxed);
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

async fn start_transfer_meter(
    upstream_proxy: Option<String>,
    timeout: Duration,
) -> PyResult<TransferMeter> {
    if let Some(proxy) = upstream_proxy.as_deref() {
        let parsed = Url::parse(proxy).map_err(|error| {
            PyRuntimeError::new_err(format!("transfer_stats代理URL无效: {error}"))
        })?;
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
    let task_counters = counters.clone();
    let completed = tokio::spawn(async move {
        let _ = tokio::time::timeout(
            timeout,
            run_transfer_meter(listener, upstream_proxy, task_counters),
        )
        .await;
    });
    let proxy = Proxy::all(format!("http://127.0.0.1:{port}")).map_err(to_py_error)?;
    Ok(TransferMeter {
        proxy,
        counters,
        completed: Some(completed),
    })
}

fn select_proxy_mapping(
    url: &str,
    proxies: Vec<(String, Option<String>)>,
) -> PyResult<Option<String>> {
    #[allow(clippy::unnecessary_to_owned)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestHttpVersion {
    Auto,
    Http1,
    Http2,
    Http3,
}

fn parse_request_http_version(version: &str) -> PyResult<RequestHttpVersion> {
    match version.to_ascii_lowercase().as_str() {
        "auto" | "default" => Ok(RequestHttpVersion::Auto),
        "http1" | "http1.1" | "http/1.1" => Ok(RequestHttpVersion::Http1),
        "http2" | "h2" | "http/2" => Ok(RequestHttpVersion::Http2),
        "http3" | "h3" | "http/3" | "v3" | "v3only" => Ok(RequestHttpVersion::Http3),
        _ => Err(PyRuntimeError::new_err(
            "http_version必须是auto、http1.1、http2或http3",
        )),
    }
}

fn apply_wreq_request_version(
    request: wreq::RequestBuilder,
    version: RequestHttpVersion,
) -> wreq::RequestBuilder {
    match version {
        RequestHttpVersion::Auto => request,
        RequestHttpVersion::Http1 => request.version(Version::HTTP_11),
        // wreq默认ALPN顺序优先h2，同时允许服务端只支持HTTP/1.1时平滑降级。
        RequestHttpVersion::Http2 | RequestHttpVersion::Http3 => request,
    }
}

fn parse_websocket_version(version: &str) -> PyResult<Version> {
    match version.to_ascii_lowercase().as_str() {
        "http1" | "http1.1" | "http/1.1" => Ok(Version::HTTP_11),
        "http2" | "h2" | "http/2" => Ok(Version::HTTP_2),
        _ => Err(PyRuntimeError::new_err(
            "WebSocket version必须是http1或http2",
        )),
    }
}

fn http_version_name(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "UNKNOWN",
    }
}

fn websocket_cookie_url(url: &str) -> PyResult<String> {
    let mut parsed = Url::parse(url).map_err(to_py_error)?;
    let scheme = match parsed.scheme() {
        "ws" => "http",
        "wss" => "https",
        "http" | "https" => return Ok(url.to_string()),
        _ => {
            return Err(PyRuntimeError::new_err(
                "WebSocket URL必须使用ws://或wss://",
            ));
        }
    };
    parsed
        .set_scheme(scheme)
        .map_err(|_| PyRuntimeError::new_err("WebSocket URL协议转换失败"))?;
    Ok(parsed.into())
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

fn http3_headers_for_url(
    state: &SessionState,
    url: &str,
    base_headers: &HeaderMap,
) -> PyResult<HeaderMap> {
    let mut headers = base_headers.clone();
    if headers.contains_key(COOKIE) {
        return Ok(headers);
    }
    let values = cookie_pairs(&state.cookie_jar, url)?;
    if !values.is_empty() {
        let cookie = values
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        headers.insert(COOKIE, HeaderValue::from_str(&cookie).map_err(to_py_error)?);
    }
    Ok(headers)
}

fn same_origin(first: &Url, second: &Url) -> bool {
    first.scheme() == second.scheme()
        && first.host_str() == second.host_str()
        && first.port_or_known_default() == second.port_or_known_default()
}

struct Http3AttemptError {
    error: PyErr,
    response_received: bool,
}

fn http3_header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn profile_http3_peer(state: &SessionState, url: &str) -> Option<SocketAddr> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    state
        .dns_overrides
        .iter()
        .find(|(domain, _)| domain.eq_ignore_ascii_case(host))
        .and_then(|(_, addrs)| addrs.first().copied())
        .map(|mut address| {
            address.set_port(port);
            address
        })
}

fn profile_http3_cache_key(index: usize, url: &str, peer: Option<SocketAddr>) -> PyResult<String> {
    let parsed = Url::parse(url).map_err(to_py_error)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| PyRuntimeError::new_err("HTTP/3 URL缺少host"))?;
    Ok(format!(
        "{index}|{}://{}:{}|{}",
        parsed.scheme(),
        host.to_ascii_lowercase(),
        parsed.port_or_known_default().unwrap_or(443),
        peer.map_or_else(|| "dns".to_string(), |value| value.to_string())
    ))
}

async fn profile_http3_roundtrip(
    state: Arc<SessionState>,
    index: usize,
    request: profile_http3::H3Request,
) -> std::result::Result<profile_http3::H3Response, Http3AttemptError> {
    let template = state.variants[index]
        .record
        .http3
        .clone()
        .ok_or_else(|| Http3AttemptError {
            error: PyRuntimeError::new_err("当前指纹没有schema 2 HTTP/3模板"),
            response_received: false,
        })?;
    let key = profile_http3_cache_key(index, &request.url, request.peer).map_err(|error| {
        Http3AttemptError {
            error,
            response_received: false,
        }
    })?;
    let slot = {
        let mut clients = state
            .profile_http3_clients
            .lock()
            .map_err(|_| Http3AttemptError {
                error: PyRuntimeError::new_err("模板HTTP/3 Client缓存锁已损坏"),
                response_received: false,
            })?;
        if let Some(client) = clients.get(&key) {
            client.clone()
        } else {
            let client = Arc::new(Mutex::new(None));
            clients.put(key.clone(), client.clone());
            client
        }
    };
    let task_slot = slot.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut client = task_slot
            .lock()
            .map_err(|_| profile_http3::H3RequestError {
                error: anyhow::anyhow!("模板HTTP/3连接锁已损坏"),
                response_received: false,
            })?;
        if client.as_ref().is_some_and(|client| !client.is_reusable()) {
            *client = None;
        }
        if client.is_none() {
            *client = Some(
                profile_http3::H3Client::connect(template, &request).map_err(|error| {
                    profile_http3::H3RequestError {
                        error,
                        response_received: false,
                    }
                })?,
            );
        }
        client
            .as_mut()
            .expect("HTTP/3 client initialized")
            .roundtrip(request)
    })
    .await
    .map_err(|error| Http3AttemptError {
        error: PyRuntimeError::new_err(format!("HTTP/3工作线程失败: {error}")),
        response_received: false,
    })?;
    match result {
        Ok(response) => Ok(response),
        Err(error) => {
            if let Ok(mut clients) = state.profile_http3_clients.lock()
                && clients
                    .peek(&key)
                    .is_some_and(|cached| Arc::ptr_eq(cached, &slot))
            {
                clients.pop(&key);
            }
            Err(Http3AttemptError {
                error: PyRuntimeError::new_err(format!("HTTP/3模板回放失败: {error}")),
                response_received: error.response_received,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_profile_http3_request(
    _permit: OwnedSemaphorePermit,
    state: Arc<SessionState>,
    index: usize,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Duration,
    allow_redirects: bool,
    max_redirects: usize,
    fingerprint_id: String,
    profile: String,
    max_response_bytes: usize,
) -> std::result::Result<RawNativeResponse, Http3AttemptError> {
    let started = Instant::now();
    let mut method = method;
    let mut url = url;
    let mut headers = headers;
    let mut body = body;
    let mut history = Vec::new();
    let mut redirect_count = 0usize;
    loop {
        let request_headers =
            http3_headers_for_url(&state, &url, &headers).map_err(|error| Http3AttemptError {
                error,
                response_received: false,
            })?;
        let request = profile_http3::H3Request {
            method: method.as_str().to_string(),
            url: url.clone(),
            headers: http3_header_pairs(&request_headers),
            body: body.clone(),
            timeout: remaining_protocol_timeout(started, timeout).map_err(|error| {
                Http3AttemptError {
                    error,
                    response_received: false,
                }
            })?,
            verify: state.verify,
            max_response_bytes,
            peer: profile_http3_peer(&state, &url),
        };
        let response = profile_http3_roundtrip(state.clone(), index, request).await?;
        if state.cookie_store {
            for (name, value) in &response.headers {
                if name.eq_ignore_ascii_case("set-cookie") {
                    state.cookie_jar.add(value.as_str(), &url);
                }
            }
        }
        let location = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("location"))
            .map(|(_, value)| value.clone());
        let is_redirect = matches!(response.status, 301 | 302 | 303 | 307 | 308);
        if !allow_redirects || !is_redirect || location.is_none() {
            return Ok((
                response.status,
                native_headers(response.headers),
                Bytes::from(response.body),
                fingerprint_id,
                profile,
                url,
                "HTTP/3".to_string(),
                history,
                None,
            ));
        }
        if redirect_count >= max_redirects {
            return Err(Http3AttemptError {
                error: PyRuntimeError::new_err(format!(
                    "重定向次数超过max_redirects限制: {max_redirects}"
                )),
                response_received: true,
            });
        }
        let current = Url::parse(&url).map_err(|error| Http3AttemptError {
            error: to_py_error(error),
            response_received: true,
        })?;
        let target = current
            .join(location.as_deref().expect("redirect location exists"))
            .map_err(|error| Http3AttemptError {
                error: to_py_error(error),
                response_received: true,
            })?;
        if target.scheme() != "https" {
            return Err(Http3AttemptError {
                error: PyRuntimeError::new_err("HTTP/3重定向目标必须使用https://"),
                response_received: true,
            });
        }
        history.push((
            response.status,
            url.clone(),
            target.to_string(),
            response.headers.clone(),
        ));
        if !same_origin(&current, &target) {
            headers.remove(AUTHORIZATION);
            headers.remove(PROXY_AUTHORIZATION);
            headers.remove(COOKIE);
            headers.remove(HOST);
        }
        if response.status == 303
            || ((response.status == 301 || response.status == 302) && method == Method::POST)
        {
            method = Method::GET;
            body = None;
            headers.remove(CONTENT_LENGTH);
            headers.remove(CONTENT_TYPE);
        }
        url = target.into();
        redirect_count += 1;
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_profile_http3_stream_request(
    permit: OwnedSemaphorePermit,
    state: Arc<SessionState>,
    index: usize,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Duration,
    allow_redirects: bool,
    max_redirects: usize,
    fingerprint_id: String,
    profile: String,
) -> std::result::Result<NativeStreamResponse, Http3AttemptError> {
    let max_response_bytes = state.max_response_bytes;
    let response = execute_profile_http3_request(
        permit,
        state,
        index,
        method,
        url,
        headers,
        body,
        timeout,
        allow_redirects,
        max_redirects,
        fingerprint_id,
        profile,
        max_response_bytes,
    )
    .await?;
    let (status_code, headers, body, fingerprint_id, impersonate, url, http_version, history, _) =
        response;
    let stream: NativeBodyStream = Box::pin(futures_util::stream::once(async move { Ok(body) }));
    Ok(NativeStreamResponse {
        status_code,
        headers: headers.raw,
        url,
        fingerprint_id,
        impersonate,
        http_version,
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
        permit: Arc::new(Mutex::new(None)),
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_request(
    _permit: OwnedSemaphorePermit,
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
    request_version: RequestHttpVersion,
    fingerprint_id: String,
    profile: String,
    transfer_stats: bool,
    upstream_proxy: Option<String>,
    max_response_bytes: usize,
) -> PyResult<RawNativeResponse> {
    if transfer_stats && !url.starts_with("https://") {
        return Err(PyRuntimeError::new_err(
            "transfer_stats仅支持HTTPS请求，统计对象表示TLS隧道的实际TCP字节",
        ));
    }
    let mut meter = if transfer_stats {
        Some(start_transfer_meter(upstream_proxy, timeout).await?)
    } else {
        None
    };
    let mut request = apply_wreq_request_version(client.request(method, &url), request_version)
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
    let http_version = http_version_name(response.version()).to_string();
    let history = response
        .extensions()
        .get::<redirect::History>()
        .map(history_entries)
        .unwrap_or_default();
    let response_headers = native_headers_from_map(response.headers());
    let content = collect_response_body(response.bytes_stream(), max_response_bytes).await?;
    if let Some(meter) = meter.as_mut() {
        meter.finish().await;
    }
    let transfer_counters = meter.as_ref().map(|meter| meter.counters.clone());
    Ok((
        status,
        response_headers,
        content,
        fingerprint_id,
        profile,
        final_url,
        http_version,
        history,
        transfer_counters,
    ))
}

async fn collect_response_body<S, E>(mut stream: S, limit: usize) -> PyResult<Bytes>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut body = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(PyRuntimeError::new_err(format!(
                "响应Body超过max_response_bytes限制: {limit}字节"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

#[allow(clippy::too_many_arguments)]
async fn execute_stream_request(
    permit: OwnedSemaphorePermit,
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
    request_version: RequestHttpVersion,
    fingerprint_id: String,
    profile: String,
) -> PyResult<NativeStreamResponse> {
    let mut request = apply_wreq_request_version(client.request(method, &url), request_version)
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
    let http_version = http_version_name(response.version()).to_string();
    let stream: NativeBodyStream = Box::pin(
        response
            .bytes_stream()
            .map(|result| result.map_err(|error| error.to_string())),
    );
    Ok(NativeStreamResponse {
        status_code,
        headers: response_headers,
        url: final_url,
        fingerprint_id,
        impersonate: profile,
        http_version,
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
        permit: Arc::new(Mutex::new(Some(permit))),
    })
}

const HTTP3_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(30);
const HTTP3_POSITIVE_CACHE_TTL: Duration = Duration::from_secs(600);
const HTTP3_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

fn is_safe_http_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

fn remaining_protocol_timeout(started: Instant, timeout: Duration) -> PyResult<Duration> {
    timeout
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| PyRuntimeError::new_err("请求在协议降级前已超过timeout"))
}

fn http3_capability_key(
    url: &str,
    dns_override: bool,
    dns_servers: &[String],
    dns_timeout: Option<f64>,
) -> PyResult<String> {
    let parsed = Url::parse(url).map_err(to_py_error)?;
    let route = if dns_override {
        format!("{}|{dns_timeout:?}", dns_servers.join(","))
    } else {
        "session".to_string()
    };
    Ok(format!("{}|{route}", parsed.origin().ascii_serialization()))
}

fn cached_http3_capability(state: &SessionState, key: &str) -> PyResult<Option<bool>> {
    let mut capabilities = state
        .http3_capabilities
        .lock()
        .map_err(|_| PyRuntimeError::new_err("HTTP/3能力缓存锁已损坏"))?;
    let cached = capabilities.get(key).copied();
    if let Some(capability) = cached {
        if capability.expires_at > Instant::now() {
            return Ok(Some(capability.available));
        }
        capabilities.pop(key);
    }
    Ok(None)
}

fn store_http3_capability(state: &SessionState, key: String, available: bool) -> PyResult<()> {
    let ttl = if available {
        HTTP3_POSITIVE_CACHE_TTL
    } else {
        HTTP3_NEGATIVE_CACHE_TTL
    };
    state
        .http3_capabilities
        .lock()
        .map_err(|_| PyRuntimeError::new_err("HTTP/3能力缓存锁已损坏"))?
        .put(
            key,
            Http3Capability {
                available,
                expires_at: Instant::now() + ttl,
            },
        );
    Ok(())
}

async fn probe_profile_http3(
    state: Arc<SessionState>,
    index: usize,
    url: &str,
    headers: &HeaderMap,
    timeout: Duration,
    capability_key: &str,
) -> PyResult<bool> {
    if let Some(available) = cached_http3_capability(&state, capability_key)? {
        return Ok(available);
    }
    if state.variants[index].record.http3.is_none() {
        return Ok(false);
    }
    let request_headers = http3_headers_for_url(&state, url, headers)?;
    let request = profile_http3::H3Request {
        method: "HEAD".to_string(),
        url: url.to_string(),
        headers: http3_header_pairs(&request_headers),
        body: None,
        timeout: timeout.min(HTTP3_PROBE_TIMEOUT),
        verify: state.verify,
        max_response_bytes: 0,
        peer: profile_http3_peer(&state, url),
    };
    let available = profile_http3_roundtrip(state.clone(), index, request)
        .await
        .is_ok();
    store_http3_capability(&state, capability_key.to_string(), available)?;
    Ok(available)
}

#[allow(clippy::too_many_arguments)]
async fn execute_preferred_request(
    permit: OwnedSemaphorePermit,
    state: Arc<SessionState>,
    index: usize,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Duration,
    read_timeout: Option<Duration>,
    proxy_url: Option<String>,
    proxy: Option<Proxy>,
    allow_redirects: bool,
    max_redirects: usize,
    request_version: RequestHttpVersion,
    fingerprint_id: String,
    profile: String,
    transfer_stats: bool,
    dns_override: bool,
    dns_servers: Vec<String>,
    dns_timeout: Option<f64>,
    cache_route: bool,
    proxy_generation: u64,
) -> PyResult<RawNativeResponse> {
    let protocol_started = Instant::now();
    let mut permit = Some(permit);
    let direct_http3 = request_version == RequestHttpVersion::Http3
        && proxy_url.is_none()
        && !transfer_stats
        && !dns_override
        && url.starts_with("https://")
        && state.variants[index].record.http3.is_some();
    if direct_http3 {
        let capability_key = http3_capability_key(&url, dns_override, &dns_servers, dns_timeout)?;
        let cached_capability = cached_http3_capability(&state, &capability_key)?;
        if cached_capability != Some(false) {
            let safe_method = is_safe_http_method(&method);
            let can_use_http3 = if safe_method {
                true
            } else {
                probe_profile_http3(
                    state.clone(),
                    index,
                    &url,
                    &headers,
                    remaining_protocol_timeout(protocol_started, timeout)?,
                    &capability_key,
                )
                .await?
            };
            if can_use_http3 {
                let remaining = remaining_protocol_timeout(protocol_started, timeout)?;
                let attempt_timeout = if safe_method && cached_capability.is_none() {
                    remaining.min(HTTP3_PROBE_TIMEOUT)
                } else {
                    remaining
                };
                let result = execute_profile_http3_request(
                    permit.take().expect("request permit must be present"),
                    state.clone(),
                    index,
                    method.clone(),
                    url.clone(),
                    headers.clone(),
                    body.clone(),
                    attempt_timeout,
                    allow_redirects,
                    max_redirects,
                    fingerprint_id.clone(),
                    profile.clone(),
                    state.max_response_bytes,
                )
                .await;
                match result {
                    Ok(response) => {
                        store_http3_capability(&state, capability_key, true)?;
                        return Ok(response);
                    }
                    Err(error) if error.response_received => {
                        store_http3_capability(&state, capability_key, true)?;
                        return Err(error.error);
                    }
                    Err(error) if !safe_method => {
                        store_http3_capability(&state, capability_key, false)?;
                        return Err(error.error);
                    }
                    Err(_) => {
                        store_http3_capability(&state, capability_key, false)?;
                        permit = Some(acquire_connection_slot(&state).await?);
                    }
                }
            }
        }
    }
    let client = selected_request_client_async(
        state.clone(),
        index,
        dns_override,
        dns_servers,
        dns_timeout,
        cache_route,
        proxy_generation,
    )
    .await?;
    execute_request(
        permit.expect("request permit must be present"),
        client,
        method,
        url,
        headers,
        body,
        if direct_http3 {
            remaining_protocol_timeout(protocol_started, timeout)?
        } else {
            timeout
        },
        read_timeout,
        proxy,
        allow_redirects,
        max_redirects,
        request_version,
        fingerprint_id,
        profile,
        transfer_stats,
        proxy_url,
        state.max_response_bytes,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_preferred_stream_request(
    permit: OwnedSemaphorePermit,
    state: Arc<SessionState>,
    index: usize,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    timeout: Duration,
    read_timeout: Option<Duration>,
    proxy_url: Option<String>,
    proxy: Option<Proxy>,
    allow_redirects: bool,
    max_redirects: usize,
    request_version: RequestHttpVersion,
    fingerprint_id: String,
    profile: String,
    dns_override: bool,
    dns_servers: Vec<String>,
    dns_timeout: Option<f64>,
    cache_route: bool,
    proxy_generation: u64,
) -> PyResult<NativeStreamResponse> {
    let protocol_started = Instant::now();
    let mut permit = Some(permit);
    let direct_http3 = request_version == RequestHttpVersion::Http3
        && proxy_url.is_none()
        && !dns_override
        && url.starts_with("https://")
        && state.variants[index].record.http3.is_some();
    if direct_http3 {
        let capability_key = http3_capability_key(&url, dns_override, &dns_servers, dns_timeout)?;
        let cached_capability = cached_http3_capability(&state, &capability_key)?;
        if cached_capability != Some(false) {
            let safe_method = is_safe_http_method(&method);
            let can_use_http3 = if safe_method {
                true
            } else {
                probe_profile_http3(
                    state.clone(),
                    index,
                    &url,
                    &headers,
                    remaining_protocol_timeout(protocol_started, timeout)?,
                    &capability_key,
                )
                .await?
            };
            if can_use_http3 {
                let remaining = remaining_protocol_timeout(protocol_started, timeout)?;
                let attempt_timeout = if safe_method && cached_capability.is_none() {
                    remaining.min(HTTP3_PROBE_TIMEOUT)
                } else {
                    remaining
                };
                let result = execute_profile_http3_stream_request(
                    permit.take().expect("request permit must be present"),
                    state.clone(),
                    index,
                    method.clone(),
                    url.clone(),
                    headers.clone(),
                    body.clone(),
                    attempt_timeout,
                    allow_redirects,
                    max_redirects,
                    fingerprint_id.clone(),
                    profile.clone(),
                )
                .await;
                match result {
                    Ok(response) => {
                        store_http3_capability(&state, capability_key, true)?;
                        return Ok(response);
                    }
                    Err(error) if error.response_received => {
                        store_http3_capability(&state, capability_key, true)?;
                        return Err(error.error);
                    }
                    Err(error) if !safe_method => {
                        store_http3_capability(&state, capability_key, false)?;
                        return Err(error.error);
                    }
                    Err(_) => {
                        store_http3_capability(&state, capability_key, false)?;
                        permit = Some(acquire_connection_slot(&state).await?);
                    }
                }
            }
        }
    }
    let client = selected_request_client_async(
        state.clone(),
        index,
        dns_override,
        dns_servers,
        dns_timeout,
        cache_route,
        proxy_generation,
    )
    .await?;
    execute_stream_request(
        permit.expect("request permit must be present"),
        client,
        method,
        url,
        headers,
        body,
        if direct_http3 {
            remaining_protocol_timeout(protocol_started, timeout)?
        } else {
            timeout
        },
        read_timeout,
        proxy,
        allow_redirects,
        max_redirects,
        request_version,
        fingerprint_id,
        profile,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_multipart_request(
    _permit: OwnedSemaphorePermit,
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
    request_version: RequestHttpVersion,
    fingerprint_id: String,
    profile: String,
    max_response_bytes: usize,
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
    let mut request = apply_wreq_request_version(client.request(method, &url), request_version)
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
    let http_version = http_version_name(response.version()).to_string();
    let history = response
        .extensions()
        .get::<redirect::History>()
        .map(history_entries)
        .unwrap_or_default();
    let response_headers = native_headers_from_map(response.headers());
    let content = collect_response_body(response.bytes_stream(), max_response_bytes).await?;
    Ok((
        status,
        response_headers,
        content,
        fingerprint_id,
        profile,
        final_url,
        http_version,
        history,
        None,
    ))
}

fn into_native_response(
    py: Python<'_>,
    response: RawNativeResponse,
) -> PyResult<Py<NativeResponse>> {
    let (
        status,
        headers,
        content,
        fingerprint_id,
        profile,
        url,
        http_version,
        history,
        transfer_counters,
    ) = response;
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
            http_version,
            history,
            transfer_stats: transfer_counters
                .map(|counters| Py::new(py, NativeTransferStats { counters }))
                .transpose()?,
        },
    )
}

type RawWebSocketMessage = (String, Option<String>, Vec<u8>, Option<u16>, Option<String>);

enum WebSocketCommand {
    Send(Message, oneshot::Sender<PyResult<()>>),
}

type WebSocketCloseCommand = (u16, String, oneshot::Sender<PyResult<()>>);

enum WebSocketEvent {
    Message(RawWebSocketMessage),
    Error(String),
}

fn raw_websocket_message(message: Message) -> RawWebSocketMessage {
    match message {
        Message::Text(value) => (
            "text".to_string(),
            Some(value.to_string()),
            Vec::new(),
            None,
            None,
        ),
        Message::Binary(value) => ("binary".to_string(), None, value.to_vec(), None, None),
        Message::Ping(value) => ("ping".to_string(), None, value.to_vec(), None, None),
        Message::Pong(value) => ("pong".to_string(), None, value.to_vec(), None, None),
        Message::Close(frame) => (
            "close".to_string(),
            None,
            Vec::new(),
            frame.as_ref().map(|frame| u16::from(frame.code.clone())),
            frame.map(|frame| frame.reason.to_string()),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_websocket(
    _permit: OwnedSemaphorePermit,
    mut socket: WebSocket,
    mut commands: mpsc::Receiver<WebSocketCommand>,
    mut close_commands: mpsc::Receiver<WebSocketCloseCommand>,
    events: mpsc::Sender<WebSocketEvent>,
    closed: Arc<AtomicBool>,
    terminal_error: Arc<Mutex<Option<String>>>,
    io_timeout: Duration,
) {
    loop {
        tokio::select! {
            biased;
            close = close_commands.recv() => {
                let Some((code, reason, completed)) = close else {
                    break;
                };
                let frame = CloseFrame {
                    code: CloseCode::from(code),
                    reason: Utf8Bytes::from(reason),
                };
                let result = socket
                    .send(Message::Close(Some(frame)));
                let result = tokio::time::timeout(io_timeout, result)
                    .await
                    .map_err(|_| PyRuntimeError::new_err("WebSocket关闭超时"))
                    .and_then(|result| result.map_err(to_py_error));
                let _ = completed.send(result);
                break;
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                match command {
                    WebSocketCommand::Send(message, completed) => {
                        let mut interrupted_close = None;
                        let result = tokio::select! {
                            biased;
                            close = close_commands.recv() => {
                                interrupted_close = close;
                                Err(PyRuntimeError::new_err("WebSocket发送被关闭操作中断"))
                            }
                            result = tokio::time::timeout(io_timeout, socket.send(message)) => {
                                result
                                    .map_err(|_| PyRuntimeError::new_err("WebSocket发送超时"))
                                    .and_then(|result| result.map_err(to_py_error))
                            }
                        };
                        let failed = result.is_err() && interrupted_close.is_none();
                        let _ = completed.send(result);
                        if let Some((code, reason, close_completed)) = interrupted_close {
                            let frame = CloseFrame {
                                code: CloseCode::from(code),
                                reason: Utf8Bytes::from(reason),
                            };
                            let result = tokio::time::timeout(
                                io_timeout,
                                socket.send(Message::Close(Some(frame))),
                            )
                            .await
                            .map_err(|_| PyRuntimeError::new_err("WebSocket关闭超时"))
                            .and_then(|result| result.map_err(to_py_error));
                            let _ = close_completed.send(result);
                            break;
                        }
                        if failed {
                            break;
                        }
                    }
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(message)) => {
                        let is_close = matches!(message, Message::Close(_));
                        if let Err(error) = events.try_send(WebSocketEvent::Message(raw_websocket_message(message))) {
                            if matches!(error, mpsc::error::TrySendError::Full(_))
                                && let Ok(mut terminal) = terminal_error.lock()
                            {
                                *terminal = Some(
                                    "WebSocket接收队列超过256条，连接已关闭".to_string(),
                                );
                            }
                            break;
                        }
                        if is_close {
                            break;
                        }
                    }
                    Some(Err(error)) => {
                        let _ = events.try_send(WebSocketEvent::Error(error.to_string()));
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    closed.store(true, Ordering::Release);
}

async fn send_websocket_command(
    commands: mpsc::Sender<WebSocketCommand>,
    message: Message,
) -> PyResult<()> {
    let (completed_tx, completed_rx) = oneshot::channel();
    commands
        .send(WebSocketCommand::Send(message, completed_tx))
        .await
        .map_err(|_| PyRuntimeError::new_err("WebSocket已经关闭"))?;
    completed_rx
        .await
        .map_err(|_| PyRuntimeError::new_err("WebSocket发送任务已经终止"))?
}

async fn close_websocket_command(
    close_commands: mpsc::Sender<WebSocketCloseCommand>,
    closed: Arc<AtomicBool>,
    code: u16,
    reason: String,
) -> PyResult<()> {
    if closed.load(Ordering::Acquire) {
        return Ok(());
    }
    let (completed_tx, completed_rx) = oneshot::channel();
    if close_commands
        .send((code, reason, completed_tx))
        .await
        .is_err()
    {
        return Ok(());
    }
    completed_rx
        .await
        .map_err(|_| PyRuntimeError::new_err("WebSocket关闭任务已经终止"))?
}

async fn recv_websocket_event(
    events: Arc<AsyncMutex<mpsc::Receiver<WebSocketEvent>>>,
    recv_active: Arc<AtomicBool>,
    terminal_error: Arc<Mutex<Option<String>>>,
) -> PyResult<Option<RawWebSocketMessage>> {
    if recv_active.swap(true, Ordering::AcqRel) {
        return Err(PyRuntimeError::new_err(
            "同一WebSocket同时只允许一个活动接收者",
        ));
    }
    let _guard = ReadGuard(recv_active);
    match events.lock().await.recv().await {
        Some(WebSocketEvent::Message(message)) => Ok(Some(message)),
        Some(WebSocketEvent::Error(error)) => Err(PyRuntimeError::new_err(error)),
        None => {
            if let Some(error) = terminal_error
                .lock()
                .ok()
                .and_then(|mut value| value.take())
            {
                Err(PyRuntimeError::new_err(error))
            } else {
                Ok(None)
            }
        }
    }
}

#[pyclass]
struct NativeWebSocket {
    #[pyo3(get)]
    url: String,
    #[pyo3(get)]
    protocol: Option<String>,
    #[pyo3(get)]
    fingerprint_id: String,
    #[pyo3(get)]
    impersonate: String,
    commands: mpsc::Sender<WebSocketCommand>,
    close_commands: mpsc::Sender<WebSocketCloseCommand>,
    events: Arc<AsyncMutex<mpsc::Receiver<WebSocketEvent>>>,
    closed: Arc<AtomicBool>,
    recv_active: Arc<AtomicBool>,
    terminal_error: Arc<Mutex<Option<String>>>,
}

#[pymethods]
impl NativeWebSocket {
    fn send_text(&self, py: Python<'_>, value: String) -> PyResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PyRuntimeError::new_err("WebSocket已经关闭"));
        }
        let runtime = shared_runtime()?;
        let commands = self.commands.clone();
        py.detach(|| runtime.block_on(send_websocket_command(commands, Message::text(value))))
    }

    fn send_text_async<'py>(&self, py: Python<'py>, value: String) -> PyResult<Bound<'py, PyAny>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PyRuntimeError::new_err("WebSocket已经关闭"));
        }
        let commands = self.commands.clone();
        rust_future_into_py(py, send_websocket_command(commands, Message::text(value)))
    }

    fn send_bytes(&self, py: Python<'_>, value: Vec<u8>) -> PyResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PyRuntimeError::new_err("WebSocket已经关闭"));
        }
        let runtime = shared_runtime()?;
        let commands = self.commands.clone();
        py.detach(|| runtime.block_on(send_websocket_command(commands, Message::binary(value))))
    }

    fn send_bytes_async<'py>(
        &self,
        py: Python<'py>,
        value: Vec<u8>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PyRuntimeError::new_err("WebSocket已经关闭"));
        }
        let commands = self.commands.clone();
        rust_future_into_py(py, send_websocket_command(commands, Message::binary(value)))
    }

    fn ping(&self, py: Python<'_>, value: Vec<u8>) -> PyResult<()> {
        if value.len() > 125 {
            return Err(PyRuntimeError::new_err("WebSocket Ping载荷不能超过125字节"));
        }
        let runtime = shared_runtime()?;
        let commands = self.commands.clone();
        py.detach(|| runtime.block_on(send_websocket_command(commands, Message::ping(value))))
    }

    fn ping_async<'py>(&self, py: Python<'py>, value: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        if value.len() > 125 {
            return Err(PyRuntimeError::new_err("WebSocket Ping载荷不能超过125字节"));
        }
        let commands = self.commands.clone();
        rust_future_into_py(py, send_websocket_command(commands, Message::ping(value)))
    }

    fn pong(&self, py: Python<'_>, value: Vec<u8>) -> PyResult<()> {
        if value.len() > 125 {
            return Err(PyRuntimeError::new_err("WebSocket Pong载荷不能超过125字节"));
        }
        let runtime = shared_runtime()?;
        let commands = self.commands.clone();
        py.detach(|| runtime.block_on(send_websocket_command(commands, Message::pong(value))))
    }

    fn pong_async<'py>(&self, py: Python<'py>, value: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        if value.len() > 125 {
            return Err(PyRuntimeError::new_err("WebSocket Pong载荷不能超过125字节"));
        }
        let commands = self.commands.clone();
        rust_future_into_py(py, send_websocket_command(commands, Message::pong(value)))
    }

    fn recv(&self, py: Python<'_>) -> PyResult<Option<RawWebSocketMessage>> {
        let runtime = shared_runtime()?;
        let events = self.events.clone();
        let recv_active = self.recv_active.clone();
        let terminal_error = self.terminal_error.clone();
        py.detach(|| runtime.block_on(recv_websocket_event(events, recv_active, terminal_error)))
    }

    fn recv_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let events = self.events.clone();
        let recv_active = self.recv_active.clone();
        let terminal_error = self.terminal_error.clone();
        rust_future_into_py(
            py,
            recv_websocket_event(events, recv_active, terminal_error),
        )
    }

    #[pyo3(signature = (code=1000, reason=String::new()))]
    fn close(&self, py: Python<'_>, code: u16, reason: String) -> PyResult<()> {
        let runtime = shared_runtime()?;
        let close_commands = self.close_commands.clone();
        let closed = self.closed.clone();
        py.detach(|| {
            runtime.block_on(close_websocket_command(
                close_commands,
                closed,
                code,
                reason,
            ))
        })
    }

    #[pyo3(signature = (code=1000, reason=String::new()))]
    fn close_async<'py>(
        &self,
        py: Python<'py>,
        code: u16,
        reason: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let close_commands = self.close_commands.clone();
        let closed = self.closed.clone();
        rust_future_into_py(
            py,
            close_websocket_command(close_commands, closed, code, reason),
        )
    }

    #[getter]
    fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_websocket(
    permit: OwnedSemaphorePermit,
    client: Client,
    url: String,
    headers: HeaderMap,
    protocols: Vec<String>,
    version: Version,
    timeout: Duration,
    proxy: Option<Proxy>,
    fingerprint_id: String,
    profile: String,
    max_message_bytes: usize,
) -> PyResult<NativeWebSocket> {
    let mut request = client
        .websocket(&url)
        .headers(headers)
        .protocols(protocols)
        .version(version)
        .max_message_size(max_message_bytes)
        .max_frame_size(max_message_bytes);
    if let Some(proxy) = proxy {
        request = request.proxy(proxy);
    }
    let response = tokio::time::timeout(timeout, request.send())
        .await
        .map_err(|_| PyRuntimeError::new_err("WebSocket握手超时"))?
        .map_err(to_py_error)?;
    let final_url = response.uri().to_string();
    let socket = tokio::time::timeout(timeout, response.into_websocket())
        .await
        .map_err(|_| PyRuntimeError::new_err("WebSocket升级超时"))?
        .map_err(to_py_error)?;
    let protocol = socket
        .protocol()
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (commands_tx, commands_rx) = mpsc::channel(64);
    let (close_commands_tx, close_commands_rx) = mpsc::channel(1);
    let (events_tx, events_rx) = mpsc::channel(256);
    let closed = Arc::new(AtomicBool::new(false));
    let terminal_error = Arc::new(Mutex::new(None));
    tokio::spawn(run_websocket(
        permit,
        socket,
        commands_rx,
        close_commands_rx,
        events_tx,
        closed.clone(),
        terminal_error.clone(),
        timeout,
    ));
    Ok(NativeWebSocket {
        url: final_url,
        protocol,
        fingerprint_id,
        impersonate: profile,
        commands: commands_tx,
        close_commands: close_commands_tx,
        events: Arc::new(AsyncMutex::new(events_rx)),
        closed,
        recv_active: Arc::new(AtomicBool::new(false)),
        terminal_error,
    })
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
    permit: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    size: i64,
) -> PyResult<Vec<u8>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    if closed.load(Ordering::Acquire) {
        permit.lock().ok().and_then(|mut permit| permit.take());
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
        permit.lock().ok().and_then(|mut permit| permit.take());
        return Err(PyRuntimeError::new_err(error.clone()));
    }
    while state.buffered.len().saturating_sub(state.offset) < requested {
        if closed.load(Ordering::Acquire) {
            state.stream = None;
            state.buffered.clear();
            state.offset = 0;
            permit.lock().ok().and_then(|mut permit| permit.take());
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
                permit.lock().ok().and_then(|mut permit| permit.take());
                return Err(PyRuntimeError::new_err(message));
            }
            None => {
                state.stream = None;
                if closed.load(Ordering::Acquire) {
                    state.buffered.clear();
                    state.offset = 0;
                    permit.lock().ok().and_then(|mut permit| permit.take());
                    return Ok(Vec::new());
                }
                permit.lock().ok().and_then(|mut permit| permit.take());
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
    http_version: String,
    #[pyo3(get)]
    history: Vec<NativeHistoryEntry>,
    state: Arc<AsyncMutex<StreamState>>,
    closed: Arc<AtomicBool>,
    close_notify: Arc<Notify>,
    read_active: Arc<AtomicBool>,
    permit: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
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
        let permit = self.permit.clone();
        py.detach(|| {
            runtime.block_on(read_stream(
                state,
                closed,
                close_notify,
                read_active,
                permit,
                size,
            ))
        })
    }

    #[pyo3(signature = (size=-1))]
    fn read_async<'py>(&self, py: Python<'py>, size: i64) -> PyResult<Bound<'py, PyAny>> {
        let state = self.state.clone();
        let closed = self.closed.clone();
        let close_notify = self.close_notify.clone();
        let read_active = self.read_active.clone();
        let permit = self.permit.clone();
        rust_future_into_py(py, async move {
            read_stream(state, closed, close_notify, read_active, permit, size).await
        })
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.close_notify.notify_one();
        if let Some(runtime) = 共享运行时.get() {
            let permit = self.permit.clone();
            let state = self.state.clone();
            let closed = self.closed.clone();
            let close_notify = self.close_notify.clone();
            runtime.spawn(async move {
                close_stream(state, closed, close_notify).await;
                permit.lock().ok().and_then(|mut permit| permit.take());
            });
        }
    }

    fn close_async<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let state = self.state.clone();
        let closed = self.closed.clone();
        let close_notify = self.close_notify.clone();
        let permit = self.permit.clone();
        rust_future_into_py(py, async move {
            close_stream(state, closed, close_notify).await;
            permit.lock().ok().and_then(|mut permit| permit.take());
            Ok(())
        })
    }
}

#[pyclass]
struct NativeSession {
    state: Arc<SessionState>,
}

fn parse_ip(value: &str, field: &str) -> PyResult<IpAddr> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|_| PyRuntimeError::new_err(format!("{field}包含无效IP地址: {value}")))
}

fn parse_dns_server(value: &str) -> PyResult<(IpAddr, u16)> {
    let value = value.trim();
    if let Ok(addr) = value.parse::<SocketAddr>() {
        Ok((addr.ip(), addr.port()))
    } else {
        Ok((parse_ip(value, "dns_servers")?, 53))
    }
}

fn build_custom_dns_resolver(
    dns_servers: Vec<String>,
    dns_timeout: Option<f64>,
) -> PyResult<Option<CustomDnsResolver>> {
    if dns_servers.is_empty() {
        return Ok(None);
    }
    let name_servers = dns_servers
        .iter()
        .map(|value| {
            let (ip, port) = parse_dns_server(value)?;
            let mut udp = ConnectionConfig::udp();
            udp.port = port;
            let mut tcp = ConnectionConfig::tcp();
            tcp.port = port;
            Ok(NameServerConfig::new(ip, true, vec![udp, tcp]))
        })
        .collect::<PyResult<Vec<_>>>()?;
    let config = ResolverConfig::from_parts(None, Vec::new(), name_servers);
    let mut builder = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
    let options = builder.options_mut();
    options.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
    options.try_tcp_on_error = true;
    if let Some(timeout) = parse_optional_timeout("dns_timeout", dns_timeout)? {
        options.timeout = timeout;
    }
    let resolver = builder
        .build()
        .map_err(|error| PyRuntimeError::new_err(format!("无法构建DNS解析器: {error}")))?;
    Ok(Some(CustomDnsResolver { resolver }))
}

fn parse_dns_overrides(
    values: Vec<(String, Vec<String>)>,
) -> PyResult<Vec<(String, Vec<SocketAddr>)>> {
    values
        .into_iter()
        .map(|(domain, values)| {
            let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
            if domain.is_empty() || values.is_empty() {
                return Err(PyRuntimeError::new_err("resolve中的域名和IP列表不能为空"));
            }
            let addrs = values
                .iter()
                .map(|value| parse_ip(value, "resolve").map(|ip| SocketAddr::new(ip, 0)))
                .collect::<PyResult<Vec<_>>>()?;
            Ok((domain, addrs))
        })
        .collect()
}

#[pymethods]
impl NativeSession {
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (impersonate, fingerprint_rotation=true, proxy=None, verify=true, connect_timeout=None, fingerprints_path=None, default_headers=Vec::new(), max_connections=50, happy_eyeballs_timeout=Some(0.3), resolve=Vec::new(), dns_servers=Vec::new(), dns_timeout=Some(5.0), fingerprint_pool=true, fingerprint_pool_size=100, max_cached_origins=4, max_response_bytes=67108864, max_websocket_message_bytes=16777216, cookie_store=true, fingerprints_json=None))]
    fn new(
        py: Python<'_>,
        impersonate: String,
        fingerprint_rotation: bool,
        proxy: Option<String>,
        verify: bool,
        connect_timeout: Option<f64>,
        fingerprints_path: Option<String>,
        default_headers: Vec<(String, String)>,
        max_connections: usize,
        happy_eyeballs_timeout: Option<f64>,
        resolve: Vec<(String, Vec<String>)>,
        dns_servers: Vec<String>,
        dns_timeout: Option<f64>,
        fingerprint_pool: bool,
        fingerprint_pool_size: usize,
        max_cached_origins: usize,
        max_response_bytes: usize,
        max_websocket_message_bytes: usize,
        cookie_store: bool,
        fingerprints_json: Option<String>,
    ) -> PyResult<Self> {
        if max_connections == 0 {
            return Err(PyRuntimeError::new_err("max_connections必须大于0"));
        }
        if max_connections > Semaphore::MAX_PERMITS {
            return Err(PyRuntimeError::new_err(format!(
                "max_connections不能超过{}",
                Semaphore::MAX_PERMITS
            )));
        }
        if fingerprint_pool_size == 0 {
            return Err(PyRuntimeError::new_err("fingerprint_pool_size必须大于0"));
        }
        if max_response_bytes == 0 {
            return Err(PyRuntimeError::new_err("max_response_bytes必须大于0"));
        }
        if max_websocket_message_bytes == 0 {
            return Err(PyRuntimeError::new_err(
                "max_websocket_message_bytes必须大于0",
            ));
        }
        let happy_eyeballs_timeout =
            parse_optional_timeout("happy_eyeballs_timeout", happy_eyeballs_timeout)?;
        let impersonate_path = std::path::Path::new(&impersonate).is_file();
        let inline_fingerprints = fingerprints_json.is_some();
        if (impersonate_path || inline_fingerprints) && fingerprints_path.is_some() {
            return Err(PyRuntimeError::new_err(
                "impersonate使用指纹文件路径或捕获结果对象时不能同时传fingerprints_path",
            ));
        }
        let fingerprints_path = if impersonate_path {
            Some(impersonate.clone())
        } else {
            fingerprints_path
        };
        let default_proxy = proxy
            .map(|identity| {
                Proxy::all(&identity)
                    .map(|proxy| DefaultProxy { identity, proxy })
                    .map_err(to_py_error)
            })
            .transpose()?;
        // 指纹来源：传入路径时由本实例独立读取解析（提前单实例加载），
        // 指纹随SessionState的variants持有，不进入进程级全局缓存，多个实例可各读各的文件；
        // 不传路径时回退到编译进wheel的内置指纹（进程级全局缓存只服务内置数据）。
        let (records, source_name) = match (fingerprints_json, fingerprints_path) {
            (Some(content), None) => {
                let records = py
                    .detach(move || parse_records(&content).context("内存捕获结果解析失败"))
                    .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))?;
                (records, "捕获结果对象")
            }
            (None, Some(path)) => {
                // 构造期文件I/O和JSON反序列化不占用GIL，避免并发创建实例彼此串行。
                let records = py
                    .detach(move || {
                        let content = std::fs::read_to_string(&path)
                            .with_context(|| format!("无法读取指纹文件 {path}"))?;
                        parse_records(&content).with_context(|| format!("指纹文件解析失败 {path}"))
                    })
                    .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))?;
                (records, "指纹文件")
            }
            (None, None) => (embedded_records()?.clone(), "内置指纹"),
            (Some(_), Some(_)) => unreachable!("上方已拒绝重复指纹来源"),
        };
        let normalized = if impersonate_path || inline_fingerprints {
            let profiles: BTreeSet<_> = records
                .iter()
                .map(|record| normalize_profile(&record.profile))
                .collect();
            if profiles.len() != 1 {
                return Err(PyRuntimeError::new_err(format!(
                    "impersonate指纹文件或捕获结果对象必须只包含一个profile，当前包含: {}",
                    profiles.into_iter().collect::<Vec<_>>().join(", ")
                )));
            }
            profiles.into_iter().next().unwrap_or_default()
        } else {
            normalize_profile(&impersonate)
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
        let dns_resolver = build_custom_dns_resolver(dns_servers, dns_timeout)?;
        let dns_overrides = parse_dns_overrides(resolve)?;
        let selected_variant = random_variant(matching.len());
        let effective_fingerprint_pools = if fingerprint_rotation && fingerprint_pool {
            fingerprint_pool_size.min(matching.len())
        } else {
            1
        };
        let client_pool_max_size = max_cached_origins
            .max(1)
            .div_ceil(effective_fingerprint_pools)
            .saturating_mul(2)
            .max(max_connections.div_ceil(effective_fingerprint_pools))
            .max(2);
        let state = Arc::new(SessionState {
            profile: normalized,
            variants: matching
                .into_iter()
                .map(|record| Variant {
                    record,
                    session_cache: Arc::new(LruTlsSessionCache::new(8)),
                    client: ArcSwapOption::empty(),
                    client_init: Mutex::new(()),
                    dns_clients: Mutex::new(LruCache::new(NonZeroUsize::new(8).unwrap())),
                })
                .collect(),
            rotation: fingerprint_rotation,
            fingerprint_pool,
            fingerprint_pool_size,
            fingerprint_pool_members: ArcSwap::from_pointee(Vec::new()),
            fingerprint_pool_init: Mutex::new(()),
            max_cached_origins,
            cached_origins: Mutex::new(HashMap::new()),
            client_pool_max_size,
            default_proxy: ArcSwapOption::from(default_proxy.map(Arc::new)),
            proxy_generation: AtomicU64::new(0),
            default_headers: ArcSwap::from_pointee(parse_headers(default_headers)?),
            verify,
            connect_timeout: parse_optional_timeout("connect_timeout", connect_timeout)?,
            happy_eyeballs_timeout,
            dns_resolver,
            dns_overrides: Arc::new(dns_overrides),
            profile_http3_clients: Mutex::new(LruCache::new(
                NonZeroUsize::new(max_cached_origins.max(1)).unwrap(),
            )),
            http3_capabilities: Mutex::new(LruCache::new(NonZeroUsize::new(64).unwrap())),
            cookie_jar: Arc::new(Jar::default()),
            cookie_store,
            selected_variant: AtomicUsize::new(selected_variant),
            closed: AtomicBool::new(false),
            connection_slots: Arc::new(Semaphore::new(max_connections)),
            max_response_bytes,
            max_websocket_message_bytes,
        });
        Ok(Self { state })
    }

    #[getter]
    fn impersonate(&self) -> String {
        self.state.profile.clone()
    }

    #[getter]
    fn fingerprint_count(&self) -> usize {
        self.state.variants.len()
    }

    #[getter]
    fn fingerprint_pool_count(&self) -> PyResult<usize> {
        Ok(self.state.fingerprint_pool_members.load().len())
    }

    #[getter]
    fn cached_origin_count(&self) -> PyResult<usize> {
        Ok(self
            .state
            .cached_origins
            .lock()
            .map_err(|_| PyRuntimeError::new_err("Origin缓存锁已损坏"))?
            .len())
    }

    #[getter]
    fn request_dns_client_count(&self) -> PyResult<usize> {
        self.state
            .variants
            .iter()
            .try_fold(0usize, |total, variant| {
                variant
                    .dns_clients
                    .lock()
                    .map(|clients| total + clients.len())
                    .map_err(|_| PyRuntimeError::new_err("请求级DNS Client缓存锁已损坏"))
            })
    }

    // PyO3边界保留显式请求选项，避免把参数塞进不透明字典。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, http_version=String::from("http2"), allow_redirects=true, max_redirects=10, transfer_stats=false, dns_override=false, dns_servers=Vec::new(), dns_timeout=Some(5.0)))]
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
        http_version: String,
        allow_redirects: bool,
        max_redirects: usize,
        transfer_stats: bool,
        dns_override: bool,
        dns_servers: Vec<String>,
        dns_timeout: Option<f64>,
    ) -> PyResult<Py<NativeResponse>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let request_version = parse_request_http_version(&http_version)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&state, &url, proxy_url.as_deref())?;
        let result = py.detach(move || {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = runtime.block_on(acquire_connection_slot(&state))?;
            let fingerprint_id = state.variants[index].record.id.clone();
            let profile = state.profile.clone();
            let result = runtime.block_on(execute_preferred_request(
                permit,
                state.clone(),
                index,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy_url,
                proxy,
                allow_redirects,
                max_redirects,
                request_version,
                fingerprint_id,
                profile,
                transfer_stats,
                dns_override,
                dns_servers,
                dns_timeout,
                cache_route,
                proxy_generation,
            ));
            if result.is_ok() {
                reservation.commit();
            }
            result
        })?;
        into_native_response(py, result)
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, http_version=String::from("http2"), allow_redirects=true, max_redirects=10, transfer_stats=false, dns_override=false, dns_servers=Vec::new(), dns_timeout=Some(5.0)))]
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
        http_version: String,
        allow_redirects: bool,
        max_redirects: usize,
        transfer_stats: bool,
        dns_override: bool,
        dns_servers: Vec<String>,
        dns_timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let request_version = parse_request_http_version(&http_version)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&state, &url, proxy_url.as_deref())?;
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        rust_future_into_py(py, async move {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = acquire_connection_slot(&state).await?;
            let response = execute_preferred_request(
                permit,
                state.clone(),
                index,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy_url,
                proxy,
                allow_redirects,
                max_redirects,
                request_version,
                fingerprint_id,
                profile,
                transfer_stats,
                dns_override,
                dns_servers,
                dns_timeout,
                cache_route,
                proxy_generation,
            )
            .await?;
            reservation.commit();
            Python::attach(|py| into_native_response(py, response))
        })
    }

    #[pyo3(signature = (url, headers, protocols=Vec::new(), version=String::from("http1"), timeout=30.0, proxy_override=false, proxy=None, cookies=None))]
    #[allow(clippy::too_many_arguments)]
    fn websocket(
        &self,
        py: Python<'_>,
        url: String,
        headers: Vec<(String, String)>,
        protocols: Vec<String>,
        version: String,
        timeout: f64,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
    ) -> PyResult<NativeWebSocket> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let version = parse_websocket_version(&version)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let cookie_url = websocket_cookie_url(&url)?;
        let headers = prepare_headers(&state, &cookie_url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&state, &url, proxy_url.as_deref())?;
        py.detach(move || {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = runtime.block_on(acquire_connection_slot(&state))?;
            let variant = &state.variants[index];
            let client = selected_request_client(
                &state,
                index,
                false,
                Vec::new(),
                None,
                cache_route,
                proxy_generation,
            )?;
            let result = runtime.block_on(execute_websocket(
                permit,
                client,
                url,
                headers,
                protocols,
                version,
                timeout,
                proxy,
                variant.record.id.clone(),
                state.profile.clone(),
                state.max_websocket_message_bytes,
            ));
            if result.is_ok() {
                reservation.commit();
            }
            result
        })
    }

    #[pyo3(signature = (url, headers, protocols=Vec::new(), version=String::from("http1"), timeout=30.0, proxy_override=false, proxy=None, cookies=None))]
    #[allow(clippy::too_many_arguments)]
    fn websocket_async<'py>(
        &self,
        py: Python<'py>,
        url: String,
        headers: Vec<(String, String)>,
        protocols: Vec<String>,
        version: String,
        timeout: f64,
        proxy_override: bool,
        proxy: Option<String>,
        cookies: Option<Vec<(String, String)>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let version = parse_websocket_version(&version)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let cookie_url = websocket_cookie_url(&url)?;
        let headers = prepare_headers(&state, &cookie_url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&state, &url, proxy_url.as_deref())?;
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        rust_future_into_py(py, async move {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = acquire_connection_slot(&state).await?;
            let client = selected_request_client_async(
                state.clone(),
                index,
                false,
                Vec::new(),
                None,
                cache_route,
                proxy_generation,
            )
            .await?;
            let result = execute_websocket(
                permit,
                client,
                url,
                headers,
                protocols,
                version,
                timeout,
                proxy,
                fingerprint_id,
                profile,
                state.max_websocket_message_bytes,
            )
            .await;
            if result.is_ok() {
                reservation.commit();
            }
            result
        })
    }

    // 流式入口与普通入口使用相同选项，确保两种响应模式语义一致。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, http_version=String::from("http2"), allow_redirects=true, max_redirects=10, dns_override=false, dns_servers=Vec::new(), dns_timeout=Some(5.0)))]
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
        http_version: String,
        allow_redirects: bool,
        max_redirects: usize,
        dns_override: bool,
        dns_servers: Vec<String>,
        dns_timeout: Option<f64>,
    ) -> PyResult<NativeStreamResponse> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let request_version = parse_request_http_version(&http_version)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&state, &url, proxy_url.as_deref())?;
        py.detach(move || {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = runtime.block_on(acquire_connection_slot(&state))?;
            let fingerprint_id = state.variants[index].record.id.clone();
            let profile = state.profile.clone();
            let result = runtime.block_on(execute_preferred_stream_request(
                permit,
                state.clone(),
                index,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy_url,
                proxy,
                allow_redirects,
                max_redirects,
                request_version,
                fingerprint_id,
                profile,
                dns_override,
                dns_servers,
                dns_timeout,
                cache_route,
                proxy_generation,
            ));
            if result.is_ok() {
                reservation.commit();
            }
            result
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, body=None, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, http_version=String::from("http2"), allow_redirects=true, max_redirects=10, dns_override=false, dns_servers=Vec::new(), dns_timeout=Some(5.0)))]
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
        http_version: String,
        allow_redirects: bool,
        max_redirects: usize,
        dns_override: bool,
        dns_servers: Vec<String>,
        dns_timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let request_version = parse_request_http_version(&http_version)?;
        let index = request_variant(&self.state);
        let method = parse_method(&method)?;
        let headers = prepare_headers(&self.state, &url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&self.state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&self.state, &url, proxy_url.as_deref())?;
        let state = self.state.clone();
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        rust_future_into_py(py, async move {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = acquire_connection_slot(&state).await?;
            let result = execute_preferred_stream_request(
                permit,
                state.clone(),
                index,
                method,
                url,
                headers,
                body,
                timeout,
                read_timeout,
                proxy_url,
                proxy,
                allow_redirects,
                max_redirects,
                request_version,
                fingerprint_id,
                profile,
                dns_override,
                dns_servers,
                dns_timeout,
                cache_route,
                proxy_generation,
            )
            .await;
            if result.is_ok() {
                reservation.commit();
            }
            result
        })
    }

    // multipart文件由Tokio直接流式读取，Python只传路径和元数据。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, fields, files, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, http_version=String::from("http2"), allow_redirects=true, max_redirects=10, dns_override=false, dns_servers=Vec::new(), dns_timeout=Some(5.0)))]
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
        http_version: String,
        allow_redirects: bool,
        max_redirects: usize,
        dns_override: bool,
        dns_servers: Vec<String>,
        dns_timeout: Option<f64>,
    ) -> PyResult<Py<NativeResponse>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let request_version = parse_request_http_version(&http_version)?;
        let index = request_variant(&self.state);
        let state = self.state.clone();
        let runtime = shared_runtime()?;
        let method = parse_method(&method)?;
        let headers = prepare_headers(&state, &url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&state, &url, proxy_url.as_deref())?;
        let result = py.detach(move || {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = runtime.block_on(acquire_connection_slot(&state))?;
            let variant = &state.variants[index];
            let client = selected_request_client(
                &state,
                index,
                dns_override,
                dns_servers,
                dns_timeout,
                cache_route,
                proxy_generation,
            )?;
            let fingerprint_id = variant.record.id.clone();
            let profile = state.profile.clone();
            let result = runtime.block_on(execute_multipart_request(
                permit,
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
                request_version,
                fingerprint_id,
                profile,
                state.max_response_bytes,
            ));
            if result.is_ok() {
                reservation.commit();
            }
            result
        })?;
        into_native_response(py, result)
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (method, url, headers, fields, files, timeout=30.0, read_timeout=None, proxy_override=false, proxy=None, cookies=None, http_version=String::from("http2"), allow_redirects=true, max_redirects=10, dns_override=false, dns_servers=Vec::new(), dns_timeout=Some(5.0)))]
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
        http_version: String,
        allow_redirects: bool,
        max_redirects: usize,
        dns_override: bool,
        dns_servers: Vec<String>,
        dns_timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        ensure_open(&self.state)?;
        let timeout = parse_timeout("timeout", timeout)?;
        let read_timeout = parse_optional_timeout("read_timeout", read_timeout)?;
        let request_version = parse_request_http_version(&http_version)?;
        let index = request_variant(&self.state);
        let method = parse_method(&method)?;
        let headers = prepare_headers(&self.state, &url, headers, cookies)?;
        let (proxy_url, proxy, proxy_generation) =
            selected_proxy_snapshot(&self.state, proxy_override, proxy)?;
        let (cache_route, cached_origin_reservation) =
            cache_route_allowed(&self.state, &url, proxy_url.as_deref())?;
        let state = self.state.clone();
        let fingerprint_id = state.variants[index].record.id.clone();
        let profile = state.profile.clone();
        rust_future_into_py(py, async move {
            let mut reservation =
                CachedOriginReservation::new(state.clone(), cached_origin_reservation);
            let permit = acquire_connection_slot(&state).await?;
            let client = selected_request_client_async(
                state.clone(),
                index,
                dns_override,
                dns_servers,
                dns_timeout,
                cache_route,
                proxy_generation,
            )
            .await?;
            let response = execute_multipart_request(
                permit,
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
                request_version,
                fingerprint_id,
                profile,
                state.max_response_bytes,
            )
            .await?;
            reservation.commit();
            Python::attach(|py| into_native_response(py, response))
        })
    }

    fn set_proxy(&self, proxy: Option<String>) -> PyResult<()> {
        ensure_open(&self.state)?;
        let changed = self
            .state
            .default_proxy
            .load_full()
            .as_deref()
            .map(|value| value.identity.as_str())
            != proxy.as_deref();
        let default_proxy = proxy
            .map(|identity| {
                Proxy::all(&identity)
                    .map(|proxy| DefaultProxy { identity, proxy })
                    .map_err(to_py_error)
            })
            .transpose()?;
        if !changed {
            return Ok(());
        }
        self.state.default_proxy.store(default_proxy.map(Arc::new));
        self.state.proxy_generation.fetch_add(1, Ordering::AcqRel);
        if !self.state.rotation {
            self.state
                .selected_variant
                .store(random_variant(self.state.variants.len()), Ordering::Release);
        }
        // 默认代理切换后不能复用旧 client 的连接池，否则存量代理隧道可能继续承载后续请求。
        // 清空只影响下一次按默认代理发包的懒初始化；正在执行的请求仍持有自己的 Client 快照。
        for variant in &self.state.variants {
            variant.session_cache.clear();
            variant.client.store(None);
            variant
                .dns_clients
                .lock()
                .map_err(|_| PyRuntimeError::new_err("请求级DNS Client缓存锁已损坏"))?
                .clear();
        }
        self.state
            .profile_http3_clients
            .lock()
            .map_err(|_| PyRuntimeError::new_err("模板HTTP/3 Client缓存锁已损坏"))?
            .clear();
        self.state
            .http3_capabilities
            .lock()
            .map_err(|_| PyRuntimeError::new_err("HTTP/3能力缓存锁已损坏"))?
            .clear();
        self.state
            .fingerprint_pool_members
            .store(Arc::new(Vec::new()));
        self.state
            .cached_origins
            .lock()
            .map_err(|_| PyRuntimeError::new_err("Origin缓存锁已损坏"))?
            .clear();
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
        self.state.connection_slots.close();
        for variant in &self.state.variants {
            variant.session_cache.clear();
            variant.client.store(None);
            variant
                .dns_clients
                .lock()
                .map_err(|_| PyRuntimeError::new_err("请求级DNS Client缓存锁已损坏"))?
                .clear();
        }
        self.state
            .profile_http3_clients
            .lock()
            .map_err(|_| PyRuntimeError::new_err("模板HTTP/3 Client缓存锁已损坏"))?
            .clear();
        self.state
            .http3_capabilities
            .lock()
            .map_err(|_| PyRuntimeError::new_err("HTTP/3能力缓存锁已损坏"))?
            .clear();
        self.state
            .fingerprint_pool_members
            .store(Arc::new(Vec::new()));
        self.state
            .cached_origins
            .lock()
            .map_err(|_| PyRuntimeError::new_err("Origin缓存锁已损坏"))?
            .clear();
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
    module.add_class::<NativeWebSocket>()?;
    module.add_class::<NativeStreamResponse>()?;
    module.add_class::<NativeHeaders>()?;
    module.add_class::<NativeResponse>()?;
    module.add_function(wrap_pyfunction!(available_profiles, module)?)?;
    module.add_function(wrap_pyfunction!(build_response_headers, module)?)?;
    module.add("readme", 版权说明)?;
    let api_module = PyModule::from_code(
        module.py(),
        API包装源码,
        c"requests_rs._embedded_api",
        c"requests_rs._embedded_api",
    )?;
    for name in [
        "Response",
        "Headers",
        "Cookie",
        "Cookies",
        "CookieTypes",
        "Session",
        "AsyncSession",
        "WebSocket",
        "AsyncWebSocket",
        "WebSocketMessage",
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
