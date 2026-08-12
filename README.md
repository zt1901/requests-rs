# requests_rust

`requests_rust` 是一个面向浏览器指纹 HTTP 请求的 Python 原生扩展。Python 负责请求参数和结果消费，Rust 负责网络执行；底层使用 `wreq + btls + BoringSSL`，提供接近 `curl_cffi.requests` 的同步和异步 API。

## API 命名兼容

公开 API 优先沿用 `curl_cffi.requests` 的高频命名与调用习惯：`Session`、`AsyncSession`、`Response`、`Headers`、`Cookies`，以及 `request/get/post/put/patch/delete/head/options`。常用请求参数如 `params`、`headers`、`cookies`、`data`、`json`、`files`、`timeout`、`stream`、`proxy`、`proxies`、`allow_redirects` 同样保持对应名称。

这是迁移便利性的兼容目标，而非逐项复刻承诺。`requests_rust` 不兼容或尚未实现 `curl_cffi` 的专有选项时会明确报错，不会静默忽略；指纹数据模型、TLS/HTTP2 实现、代理参数形式与响应内部对象以本库的 Rust 实现为准。

## 优点

- **Rust 网络路径**：DNS、代理、TLS、HTTP/1.1/2、连接池、Cookie、超时、流式 Body 和 multipart 均由 Rust/Tokio 执行。同步请求等待及自定义指纹文件读取、解析不占用 GIL。
- **浏览器基准指纹**：TLS、HTTP/2、HPACK、请求头和优先级数据由真实浏览器采集后固化为 profile。当前内置 `chrome142`、`chrome146`、`chrome150`、`firefox151`。
- **实例级指纹隔离**：每个 `Session` 可在构造时从不同 JSON 文件独立加载 profile，不修改进程全局指纹数据，也不影响其他实例。
- **长期并发友好**：一个 `AsyncSession` 复用连接池和 Cookie Jar；代理、请求头与请求级 Cookie 可按单条请求独立传入。

## 研究记录

- Chrome 150 真实浏览器与 Rust profile 的采集对照为 41/41 字段一致。
- Firefox 151 的 5 个 profile 变体在相同对照项中均为 41/41。
- 本地代理认证回显测试中，400 条动态代理请求中位吞吐约为 646 请求/秒；实际吞吐受代理延迟、目标站响应和硬件影响。
- 本地 keep-alive 压测中，100 Worker、21 条 profile 轮换连续 2 万请求约为 10.4k 至 12.2k 请求/秒；该数据仅代表本地服务基准，不代表公网目标性能。

## 安装

当前发布产物为 Windows x64、CPython 3.10+ 的稳定 ABI wheel：

```text
requests_rust-0.2.3-cp310-abi3-win_amd64.whl
```

本地开发可直接右键运行 `build_and_install.py`。脚本会同步已采集的指纹数据、构建 wheel 并安装到项目环境。Linux、macOS 和 ARM64 需要各自平台的 wheel；仓库已提供 GitHub Actions 多平台构建矩阵。

## 最小用法

```python
from requests_rust import Session

with Session(
    impersonate="firefox151",
) as session:
    response = session.get("https://example.com/api")
    print(response.status_code, response.fingerprint_id)
```

## 实例级自定义指纹

```python
from requests_rust import get

response = get(
    "https://example.com/",
    impersonate="firefox151",
    fingerprints_path=r"D:\fingerprints\firefox151.json",
)
```

`fingerprints_path` 会在创建 Session 时读取并解析 JSON。文件内容仅属于当前实例，多个实例可同时使用不同文件。文件格式与仓库的 `fingerprints.json` 一致。

profile 默认 Header 仅在调用方未传同名 Header 时兜底。浏览器 Copy as cURL 或业务代码传入的全部 Header，包括 `User-Agent` 与 `sec-ch-ua*`，均原样优先，库不会接管或替换。

`accept`、`origin`、`referer`、`sec-fetch-*`、`priority`、Cookie 与认证 Header 必须由具体请求上下文传入。

## 核心能力

- 同步与原生 asyncio API：`Session`、`AsyncSession`、`get/post/put/patch/delete`
- HTTP/1.1、HTTP/2、重定向、总超时与 Body 读取超时
- 固定 profile 或按请求轮换 profile
- Session Cookie Jar、每请求 Cookie 覆盖、重复 Header 保序
- Session 默认代理和每请求代理覆盖
- 同步/异步流式响应与 Rust Tokio multipart 文件流

## 限制

- 当前不发送 HTTP/3。
- 当前发布 wheel 仅支持 Windows x64；其他平台需要对应构建产物。
- 不同 profile 不共享 TLS/HTTP/2 连接池，这是指纹隔离的必要限制。

详细参数、并发用法和边界说明见 [AI_USAGE_GUIDE.md](AI_USAGE_GUIDE.md)。浏览器 Copy as cURL 请求模板与指纹库的固定边界见 [RESEARCH_BASELINE.md](RESEARCH_BASELINE.md)。

## 查看内置版本

```python
from requests_rust import available_profiles

print(available_profiles())
```
