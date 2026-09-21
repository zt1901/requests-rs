# requests-rs 0.4.10

本版本将 Rust 网络错误通过结构化 PyO3 协议映射为 requests 风格异常，不再依赖 Python 对错误文本进行关键词猜测。

## 错误 API

- 原生层新增 `NativeRequestError(kind, message, source_chain)`。
- DNS、连接、代理、TLS、超时、重定向、正文解码及响应体上限均携带稳定 `kind`。
- 新增 `requests_rs.exceptions`，并公开 `DNSError`、`ConnectTimeout`、`ReadTimeout`、`SSLError`、`TooManyRedirects`、`ChunkedEncodingError`、`ContentDecodingError` 等异常。
- 请求异常携带 `request`，HTTP 状态异常同时携带 `request` 和 `response`。
- Rust `source()` 错误链通过 `source_chain` 保留，并追加到公开错误文本中。

## 性能回归

Windows x64 本机使用 release wheel、回环 keep-alive 服务实测：

| 场景 | 0.4.10 实测 | 历史基线 |
|---|---:|---:|
| Firefox 热连接，20,000 请求，并发 10，中位吞吐 | 8411.20 req/s | 2935.40 req/s |
| 同场景 CPU | 267.97 us/request | 731.25 us/request |
| Chrome keep-alive，5,000 请求，并发 100 | 2020.80 req/s | 851.81 req/s |
| 并发 50，requests-rs / curl_cffi | 8740.46 / 3396.32 req/s | 同机相邻测试 |
| 并发 500，requests-rs / curl_cffi | 8228.94 / 3309.87 req/s | 同机相邻测试 |

错误结构化改造未造成成功请求热路径性能回归。绝对值仅用于同机同配置回归，不作为公网吞吐承诺。
