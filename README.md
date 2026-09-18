# requests-rs

<p align="center">
  <strong>浏览器级网络指纹，Rust 级传输性能，requests 级开发体验。</strong>
</p>

<p align="center">
  为高并发采集、自动化、代理网络与传输研究打造的 Python 原生 HTTP 客户端。
</p>

<p align="center">
  <a href="https://pypi.org/project/requests-rs/"><img alt="PyPI" src="https://img.shields.io/pypi/v/requests-rs?color=3775A9&logo=pypi&logoColor=white"></a>
  <img alt="Python 3.10+" src="https://img.shields.io/badge/Python-3.10%2B-3776AB?logo=python&logoColor=white">
  <img alt="Rust" src="https://img.shields.io/badge/core-Rust-000000?logo=rust&logoColor=white">
  <img alt="HTTP" src="https://img.shields.io/badge/HTTP-1.1%20%7C%202%20%7C%203-2F6FEB">
  <img alt="Async" src="https://img.shields.io/badge/async-native%20Future-7B42BC">
  <img alt="License" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-2EA44F">
</p>

<p align="center">
  <strong>逐请求 DNS · 指纹运行时加载 · wheel 内置根证书 · Rust 原生并发背压</strong>
</p>

---

`requests-rs` 把 DNS、TCP、TLS、HTTP/1.1、HTTP/2、HTTP/3、代理、Cookie、连接池、流式响应和 WebSocket 全部压进 Rust 网络核心，再通过 PyO3 提供接近 `requests` 的同步与原生异步 API。

它不是给 `requests` 套一层线程池，也不是换个 User-Agent 冒充浏览器。请求直接进入共享 Tokio Runtime，BoringSSL 与真实浏览器采集画像负责 TLS/H2/H3 回放，Python 只保留清晰的调用接口和结果消费。

> **一句话：** 想要 `requests` 的易用性、浏览器的网络画像、Rust 的高并发执行与可控资源边界，就用 `requests-rs`。

## 核心能力

| 能力 | requests-rs 提供什么 |
|---|---|
| **Rust 原生网络核心** | 网络等待不占用 Python 工作线程；DNS、连接、TLS、代理握手、重定向、Body 读写和 Cookie 更新由 Rust/Tokio 执行。 |
| **浏览器级传输画像** | 回放真实 TLS ClientHello、HTTP/2 SETTINGS、Header 顺序；schema 2 进一步覆盖 QUIC Transport Parameters、HTTP/3 SETTINGS 与 QPACK。 |
| **DNS 深度可控** | TCP 请求支持 Session 级和单请求级 `dns_servers`、查询超时、A/AAAA、TTL 缓存、UDP 查询与 TCP 回退，并提供 `resolve` 静态域名/IP绑定。 |
| **动态加载指纹** | Session 构造时可直接加载 JSON 路径、Python 记录列表或 `{schema_version, records}` 对象，不必重新编译 wheel。 |
| **内置根证书库** | wheel 内置并显式注入 Mozilla/WebPKI 根证书，不依赖机器上残缺、过期或配置混乱的系统 CA 环境。 |
| **原生并发信号量** | HTTP、代理请求、流、multipart 与 WebSocket 共用一个 Rust/Tokio `Semaphore`，并发上限在网络核心内执行。 |
| **连接与身份隔离** | Origin、代理身份、DNS配置和指纹变体共同决定连接复用边界；切换代理时同步清理连接池和 TLS ticket 状态。 |
| **同步与异步统一** | `Session`、`AsyncSession`、模块级方法、响应对象、Cookie 与错误边界保持一致。 |

## 为什么更适合高并发

### 请求直接进入 Rust，不绕 Python 线程池

传统 `requests` 是同步模型。要在 asyncio 项目中并发使用，通常需要 `asyncio.to_thread()` 或 `ThreadPoolExecutor`：每个正在等待网络的请求都会占用或等待 Python 工作线程。

`requests-rs` 的 `AsyncSession` 暴露原生 awaitable，请求直接进入共享 Tokio Runtime：

```text
Python coroutine
      │
      ▼
PyO3 native Future
      │
      ▼
Tokio Runtime ── DNS / TCP / TLS / HTTP / Proxy / Cookie / Stream
      │
      ▼
wreq + BoringSSL + quiche
```

这意味着高并发网络等待不需要创建同等数量的 Python 工作线程，减少线程池排队、线程栈、上下文切换和 GIL 周边调度开销。相比“同步 `requests` + 线程池”的并发方式，任务越多，这个架构优势越明显。

对 `httpx`、`aiohttp`、`curl_cffi` 等已经具备异步或原生后端的客户端，实际吞吐仍取决于目标站、TLS、代理、响应大小、连接复用与机器配置；`requests-rs` 的优势在于把浏览器指纹、DNS、代理、Cookie、连接池和资源背压统一收进同一个 Rust Session。

### 原生信号量，更短调度路径，更快进入网络核心

`max_connections` 不是 Python 外层装饰器，而是 Rust/Tokio 原生 `Semaphore`。请求不需要先经过 Python 锁、许可队列或释放回调，再转交底层客户端；并发许可直接在网络核心入口获取，调度路径更短，取消和释放也由同一所有权模型收口：

- 请求进入网络核心前原子获取许可；
- 等待许可不会阻塞 Python 事件循环；
- 等待时间计入请求总超时；
- 普通响应读取完成后自动释放；
- 流式响应持有许可到 EOF 或关闭；
- WebSocket 持有许可到真实连接终止；
- 取消 Future 会自动释放尚未移交的许可；
- Session 关闭会唤醒全部等待任务并快速失败。

HTTP、HTTPS、HTTP 代理、SOCKS5、流、multipart 和 WebSocket 共享同一个并发池，不会出现“HTTP 50 个 + WebSocket 50 个 + 下载 50 个”分别突破上限的隐形资源膨胀。

```python
import asyncio
from requests_rs import requests


async def main() -> None:
    urls = [f"https://example.com/?id={index}" for index in range(1000)]

    async with requests.AsyncSession(
        impersonate="chrome152",
        max_connections=100,
        timeout=30,
    ) as session:
        responses = await asyncio.gather(*(session.get(url) for url in urls))

    print(len(responses))


asyncio.run(main())
```

在仓库的极端回归中，`max_connections=1` 被长流占用时，1000 个等待任务没有膨胀成 1000 个 Python 线程；测试门槛限制 RSS 增量小于 32 MiB、线程增量不超过 4。该数据用于同环境回归，不是跨机器固定承诺，但验证了当前实现不是 worker-per-request，而是共享异步原语管理的大规模等待队列。

## 30 秒开始

### 安装

```bash
python -m pip install -U requests-rs
```

当前 wheel 使用 CPython Stable ABI，支持 CPython 3.10 及以上版本。PyPI 已提供 Windows x64 与 manylinux2014 x64 wheel；其他平台请查看下方支持矩阵或从源码构建。

唯一公开导入入口：

```python
from requests_rs import requests
```

### 同步请求

```python
from requests_rs import requests

with requests.Session(
    impersonate="chrome152",
    timeout=30,
) as session:
    response = session.get("https://example.com/")
    response.raise_for_status()

    print(response.status_code)
    print(response.http_version)
    print(response.fingerprint_id)
    print(response.text)
```

### 原生异步请求

```python
import asyncio
from requests_rs import requests


async def main() -> None:
    async with requests.AsyncSession(
        impersonate="edge152",
        max_connections=50,
    ) as session:
        first, second = await asyncio.gather(
            session.get("https://example.com/api/one"),
            session.post("https://example.com/api/two", json={"id": 2}),
        )
        print(first.status_code, second.status_code)


asyncio.run(main())
```

## DNS 可改，而且可以逐请求切换

`requests-rs` 不把 DNS 锁死在操作系统配置里。你可以在 Session 级指定长期上游，也可以为某一次请求临时切换解析器。

### Session 级自定义 DNS

```python
from requests_rs import requests

with requests.Session(
    impersonate="chrome152",
    dns_servers=["1.1.1.1", "8.8.8.8"],
    dns_timeout=3.0,
) as session:
    response = session.get("https://example.com/")
```

底层使用 Rust Hickory Resolver，支持：

- IPv4、IPv6 与自定义端口，例如 `127.0.0.1:5353`、`[::1]:5353`；
- A/AAAA 双栈查询；
- DNS TTL 缓存；
- UDP 查询失败或截断后的 TCP 回退；
- 多个上游 DNS；
- Session 关闭或代理切换时清理关联 Client 与缓存。

### 单请求覆盖 DNS

```python
response = await session.get(
    "https://example.com/",
    dns_servers=["9.9.9.9", "149.112.112.112"],
    dns_timeout=2.0,
)
```

这只影响当前请求，不修改 Session 默认值，也不污染其他并发任务。默认启用 `fingerprint_pool` 时，相同请求级配置会进入按指纹隔离的有界 Rust Client 缓存，可继续复用 Resolver TTL、TCP、TLS 与 HTTP/2 连接。

传入空列表可让单次请求临时恢复系统 DNS：

```python
response = await session.get(url, dns_servers=[])
```

### 固定连接 IP，同时保留 HTTPS 域名语义

`resolve` 适合固定 CDN 节点、测试指定 IP 或绕过错误 DNS 答案：

```python
async with requests.AsyncSession(
    impersonate="chrome152",
    resolve={
        "example.com": ["203.0.113.10", "2001:db8::10"],
    },
) as session:
    response = await session.get("https://example.com/")
```

实际连接使用指定 IP，但 URL、HTTP Host、TLS SNI 与证书域名仍然保持 `example.com`。这比直接请求 IP 再手写 Host 更完整，也不会为了固定节点而牺牲 HTTPS 校验。

> **HTTP/3 提示：** `dns_servers` 当前作用于 TCP HTTP 路径，因此配置后会安全使用 H2/H1.1；需要固定 QUIC/H3 目标地址时使用 Session 级 `resolve`。最终协议始终以 `response.http_version` 为准。

## 指纹不必写死，运行时动态加载

内置 profile 适合开箱即用，自定义 profile 则可以在创建 Session 时动态加载。无需修改 Rust 源码，无需重编 wheel，也不会污染进程级内置指纹缓存。

### 直接加载单 profile JSON

```python
from pathlib import Path
from requests_rs import requests

fingerprint_file = Path(r"C:\fingerprints\edge152.json")

with requests.Session(
    impersonate=fingerprint_file,
    fingerprint_rotation=False,
) as session:
    response = session.get("https://example.com/")
    print(session.impersonate)
    print(session.fingerprint_count)
    print(response.fingerprint_id)
```

### 捕获结果不落盘，直接从内存加载

```python
capture_result = await mcp_client.call_tool(
    "read_fingerprint_result",
    {"task_id": task_id},
)

async with requests.AsyncSession(
    impersonate=capture_result,
    fingerprint_rotation=False,
) as session:
    response = await session.get("https://example.com/")
```

`impersonate` 支持：

- 单 profile JSON 文件路径；
- Python `Sequence[Mapping]` 指纹记录；
- 捕获器/MCP 返回的 `{schema_version, records, ...}` 对象；
- 一个 profile 内的多个变体与随机轮换。

多 profile 文件可以通过 `fingerprints_path` 显式选择：

```python
with requests.Session(
    impersonate="edge152",
    fingerprints_path=r"C:\fingerprints\all_profiles.json",
    fingerprint_rotation=True,
    fingerprint_pool=True,
    fingerprint_pool_size=20,
) as session:
    response = session.get("https://example.com/")
```

每个 Session 独立拥有自己的自定义指纹。不同实例可以同时加载不同文件，互不覆盖；未知 schema、未知非 GREASE TLS/H2/H3 字段、损坏 payload 或协议字段冲突会在构造阶段 fail-closed，不会悄悄降级成一份看起来成功、实际上已经变形的画像。

动态加载指的是 **Session 构造期运行时加载**。已创建的 Session 不监视文件变化；需要更新画像时创建新的 Session，可保持生命周期和连接身份边界清晰。

## 内置根证书，跨环境保持一致的浏览器级信任基线

公开 wheel 内置 Mozilla/WebPKI 根证书集合，并由网络核心显式构建和复用 `CertStore`。证书信任基线跟随 wheel 交付，不依赖新机器、精简容器或异常系统环境中可能缺失、过期、路径错误的 CA 文件。

实际收益：

- 减少 `CERTIFICATE_VERIFY_FAILED`、`unable to get local issuer certificate` 等由运行环境 CA 不完整造成的问题；
- Windows 与 Linux 使用一致的公开根信任基线，迁移环境时行为更稳定；
- HTTP/1.1、HTTP/2 与 HTTP/3 使用同一套明确的证书信任策略；
- `resolve` 固定 IP 时仍保持原始 SNI、Host 与证书域名校验；
- 不需要为了“先跑起来”而全局关闭 HTTPS 验证。

内置根证书解决的是环境型 CA 问题，不会错误放行过期证书、域名不匹配、未知私有 CA 或真正不可信的证书链。根集合随 wheel 版本更新，不自动读取系统企业私有 CA。生产环境应始终保持 `verify=True`；`verify=False` 只适用于受控的本地自签名测试。

## 与常见 Python HTTP 方案相比

| 维度 | `requests` | `httpx` / `aiohttp` | `curl_cffi` | `requests-rs` |
|---|---|---|---|---|
| 调用体验 | 简单同步 | 原生异步 | requests 风格 | requests 风格，同步与原生异步统一 |
| asyncio 网络等待 | 通常需要线程池包装 | 原生异步 | 提供异步接口 | PyO3 原生 Future 直入 Tokio |
| 浏览器 TLS/H2 画像 | 无 | 无 | 支持 impersonate | 真实采集画像，支持动态加载与严格校验 |
| HTTP/3 画像 | 无 | 通常无 | 依构建能力 | schema 2 驱动 TLS/QUIC/H3/QPACK 回放 |
| 自定义 DNS | 依赖外部适配 | 各库接口不同 | 能力依后端 | Session 级 + 单请求级 + 静态 resolve |
| 并发上限 | 连接池外通常需业务限流 | 提供连接池限制，业务操作常另行限流 | 提供客户端并发参数 | 一个 Rust Semaphore 统一普通请求、流、multipart 与 WebSocket 的完整许可生命周期 |
| 根证书环境 | 通常依赖 certifi/运行环境配置 | 通常依赖 certifi/SSL context | 取决于构建和系统 | 根集合随 wheel 固化，由 Rust TLS 核心显式注入，跨环境可复现 |
| 代理身份切换 | 业务自行隔离 | 业务自行隔离 | 支持代理 | 原子快照并清理连接池、TLS ticket 与路由缓存 |
| Python 工作线程 | 同步并发常依赖线程池 | 异步模式无需逐请求线程 | 取决于使用模式 | 异步请求不按请求占用 Python worker |

Rust 并不自动等于任何场景都更快，但 `requests-rs` 把最重的协议路径和资源调度移出 Python：在高并发、长连接、代理、动态 DNS、流式响应和复杂指纹场景中，可以避免同步 `requests` 的 Python 线程池成本，也减少业务层重复实现锁、信号量、连接身份隔离和清理逻辑。

仓库提供 [`benchmark_async_bridge.py`](benchmark_async_bridge.py)、[`benchmark_async_resources.py`](benchmark_async_resources.py) 等本地 benchmark，用于在相同机器、相同后端、相同代理、相同并发和相同连接复用条件下对比 native Future、线程池桥接与资源峰值。性能数字会随网络和目标服务显著变化，因此 README 不展示脱离环境的倍数结论。

## 浏览器画像与协议

### 内置 Profile

| Profile | 来源 | 状态 |
|---|---|---|
| `chrome146` | Google Chrome 历史采集 | 已验证固定快照 |
| `chrome150` | Google Chrome 历史采集 | 已验证固定快照 |
| `chrome152` | Google Chrome `152.0.7977.64` | 官方正式版基线，Trust Anchor IDs 已完成线级回放 |
| `edge152` | Microsoft Edge `152.0.4191.53` | 系统稳定版基线，已完成采集与回放 |
| `firefox151` | playwright_rust Firefox Juggler | 已验证研发兼容快照，不代表 Mozilla 官网 Stable |

Profile 是已采集、验证并冻结的传输快照，不是自动追随浏览器官网版本变化的字符串别名。

### HTTP/3

`http_version="http3"` 是完整模板下的 H3 优先策略：

- 直连 HTTPS 且所选记录为 schema 2 时，应用 BoringSSL ClientHello、QUIC Transport Parameters、HTTP/3 SETTINGS、Header顺序与 QPACK 策略；
- schema 1、代理、multipart、transfer stats 或自定义 `dns_servers` 场景安全使用 H2/H1.1；
- H3 失败时，安全方法可在同一总超时内降级；
- 请求已经发出后的失败不会自动重放有副作用的业务操作；
- 只有完整模板实际应用时，响应才会返回对应源 `fingerprint_id`。

HTTP/3 仍在持续进行独立互操作验证，不应把“请求成功”误认为与任意历史浏览器百分之百线级等价。

## 连接、代理与资源生命周期

- 长期复用一个 Session，可以复用匹配 Origin 与代理身份的 TCP、CONNECT、TLS 和 HTTP/2 连接；
- 每个指纹变体拥有隔离的 Client、TLS session 与请求级 DNS Client 缓存；
- `set_proxy()` 通过原子快照切换默认代理，并清理旧 Client、DNS、H3、Origin 与 TLS ticket 状态；
- `max_cached_origins` 为高基数域名和代理身份提供有界缓存边界；
- `max_response_bytes` 默认限制解压后的响应累计大小，阻止压缩炸弹和并发大 Body 无限吃内存；
- H1/H2 流式响应支持分块消费，EOF、错误、取消或关闭时释放原生许可；
- Session 关闭后拒绝新请求，并唤醒正在等待并发槽位的任务。

库不提供业务 `batch()`、任务重试或常驻业务 Worker 队列。业务层应该用有限任务调用一个长期 `AsyncSession`，让网络库负责连接、协议和资源背压，让业务代码负责重试策略、配额和数据流程。

## 支持平台

| 平台 | Rust target | wheel 标签 | 当前发布状态 |
|---|---|---|---|
| Windows x64 | `x86_64-pc-windows-msvc` | `win_amd64` | PyPI wheel |
| Linux x64 | `x86_64-unknown-linux-gnu` | `manylinux2014_x86_64` | PyPI wheel，glibc >= 2.17 |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `win_arm64` | CI 构建与安装验证，暂未发布 PyPI |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `manylinux_2_34_aarch64` | CI 构建与安装验证，暂未发布 PyPI |
| macOS Apple Silicon | `aarch64-apple-darwin` | `macosx_11_0_arm64` | CI 构建与安装验证，暂未发布 PyPI |

Alpine musl 与 macOS Intel 当前不在公开发布矩阵中。安装时必须选择与操作系统和 CPU 匹配的 wheel。

## 示例与文档

- [`examples/basic_sync.py`](examples/basic_sync.py)：同步请求与响应字段。
- [`examples/async_concurrency.py`](examples/async_concurrency.py)：复用一个 `AsyncSession` 进行原生异步并发。
- [`examples/custom_fingerprint.py`](examples/custom_fingerprint.py)：动态加载捕获器 JSON 并保留 Header 顺序。
- [API 使用手册](API_USAGE.md)：完整参数、DNS、代理、Cookie、WebSocket、流和 multipart。
- [AI 与维护者手册](AI_USAGE_GUIDE.md)：并发语义、资源边界、错误模型与工程决策。
- [指纹研究边界](RESEARCH_BASELINE.md)：profile 与业务 Header/Cookie/Token 的职责边界。
- [仓库与发布约定](REPOSITORY_DISTRIBUTION.md)：构建矩阵、发布和交付规则。
- [Chrome 152 验收记录](CHROME152_SUPPORT_FEEDBACK.md)：Trust Anchor IDs 与线级回放证据。

## 从源码构建

需要 Rust stable、Python 3.10+、maturin、CMake、Clang/libclang 与平台原生 C/C++ 工具链：

```bash
git clone https://github.com/zt1901/requests-rs.git
cd requests-rs
python -m pip install "maturin>=1.9,<2"
maturin build --release --out dist
python -m pip install --force-reinstall dist/requests_rs-*.whl
```

Windows 也可直接运行 `build_and_install.py`。脚本默认使用仓库内置指纹完成构建；只有显式传入 `--sync-fingerprints` 时才从本机来源同步指纹。

## 使用边界

- `requests_rs.requests` 兼容 `curl_cffi.requests` 的高频命名，但不承诺兼容全部专有参数；未知关键参数会明确失败。
- 业务 Cookie、认证、CSRF、签名、时间戳和 nonce 不属于浏览器传输画像，应通过 `headers`、`cookies`、`data` 或 `json` 传入。
- `verify=False` 只适用于受控测试，不应作为解决生产证书问题的方案。
- 指纹动态加载发生在 Session 构造阶段，不是已创建 Session 的文件热重载。
- `max_connections` 限制活跃网络操作，不等价于严格的物理 TCP/QUIC 连接总数。
- H1/H2 `stream=True` 是增量读取；当前 H3 路径仍先完成响应接收，再提供统一的消费接口。
- 性能比较必须固定机器、版本、后端、代理、并发、连接复用和响应大小；不要把本地请求/秒直接外推到公网。

## 贡献

欢迎提交问题和改进。网络能力变更应覆盖同步/异步生命周期、取消、关闭、错误与资源释放；指纹变更应附浏览器版本、真实采集记录和线级对照证据。

## 许可证

项目原创代码采用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可证，使用者可任选其一。Vendored 第三方组件继续遵循各自许可证与 NOTICE。
