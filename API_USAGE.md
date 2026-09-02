# requests_rust 使用与 API 手册

`requests_rust` 是 Python 3.10+ 的浏览器指纹 HTTP 与 WebSocket 客户端。网络由 Rust 执行，支持 TLS/HTTP2/QUIC/HTTP3 指纹、HTTP/1.1、HTTP/2、IPv4/IPv6、HTTP 与 SOCKS5 代理、Cookie、重定向、流式响应、multipart 和 WebSocket。schema 2 模板可直接真实回放 H3；旧 schema 自动使用 H2/H1.1。

高频 API 命名接近 `curl_cffi.requests`，但未实现的关键字参数会抛出 `TypeError`，不会被静默忽略。

## 安装

从 [GitHub Releases](https://github.com/zt1901/requests_rust-source/releases) 下载与系统、CPU架构匹配的wheel，再安装：

```bash
python -m pip install requests_rust-0.3.0-cp310-abi3-win_amd64.whl
```

wheel 使用 CPython stable ABI，要求 CPython 3.10 或更高版本。

预定义构建目标：

| 平台 | wheel 标签 |
|---|---|
| Windows x64 | `win_amd64` |
| Windows ARM64 | `win_arm64` |
| Linux x64 | `manylinux_2_34_x86_64` |
| Linux ARM64 | `manylinux_2_34_aarch64` |
| macOS Apple Silicon | `macosx_11_0_arm64` |

wheel 必须与操作系统和 CPU 架构匹配。Windows ARM64、Linux x64、Linux ARM64 和 macOS Apple Silicon wheel 已在对应原生 GitHub runner 完成构建、pip 安装、原生模块导入、profile 读取和 Session 构造冒烟。当前 Windows x64 已完成本文所列完整协议回归；其他平台的安装冒烟不等同于同等级全协议验证。Alpine musl 当前不在发布矩阵中。

Release尚未提供目标平台wheel时，可以从源码构建：

```bash
git clone https://github.com/zt1901/requests_rust-source.git
cd requests_rust-source
python -m pip install "maturin>=1.9,<2"
maturin build --release --out dist
python -m pip install --force-reinstall dist/requests_rust-*.whl
```

源码构建还需要Rust stable、CMake、Clang/libclang和当前平台的C/C++工具链。Windows开发者也可右键运行`build_and_install.py`，它会同步指纹、构建wheel并安装到当前Python。

## 导入与内置指纹

```python
from requests_rust import (
    AsyncSession,
    Session,
    available_profiles,
    delete,
    get,
    head,
    options,
    patch,
    post,
    put,
    request,
)

print(available_profiles())
```

当前内置profile如下。名称表示真实采集并验证的快照，不代表官网当前Stable，也不会自动跟随浏览器升级。

| Profile | 真实来源 | 发布状态 |
|---|---|---|
| `chrome146` | Google Chrome历史采集 | 已验证历史快照 |
| `chrome150` | Google Chrome历史采集 | 已验证历史快照 |
| `chrome152` | Google Chrome `152.0.7977.64`官方正式版 | 已完成`0xca34`线级回放并内置 |
| `edge152` | Microsoft Edge `152.0.4191.53`系统稳定版 | 当前正式版基线 |
| `firefox151` | playwright_rust Firefox Juggler | 研发兼容快照，不代表Mozilla官网Stable |

Google Chrome `152.0.7977.64`的记录ID为`66e05a9d467e4b54b2c2b71f12ced6f7`。内置profile的真实ClientHello已确认完整发送Trust Anchor Identifiers扩展`0xca34`；206字节payload与捕获样本完全相同。新增profile仍必须来自真实正式版二进制与TLS/HTTP2采集，并完整回放新扩展；不能只修改名称、UA或Client Hints。

profile决定TLS/HTTP2传输画像，不应保存业务Cookie、认证Header、CSRF、签名、`Referer`、`Origin`、时间戳或nonce。

捕获记录中的Header名称顺序会完整保留，但只有跨请求稳定的浏览器/平台字段会成为profile默认值：`user-agent`、`sec-ch-ua*`、`accept-encoding`、`accept-language`、`dnt`、`sec-gpc`和`te`。`accept`、`origin`、`referer`、`upgrade-insecure-requests`、`sec-fetch-*`、`priority`及所有业务/认证Header只保留其相对顺序，值必须由当前真实请求传入。调用方显式Header始终优先。

## 五分钟实战

### 选择入口

| 需求 | 推荐入口 |
|---|---|
| 一次简单请求 | 模块级`get/post` |
| 顺序业务或已有同步代码 | `Session` |
| 并发采集、代理池或长任务 | 长期复用一个`AsyncSession` |
| 大文件下载 | `stream=True`与分块读取 |
| 双向长连接 | `Session.websocket()`或`AsyncSession.websocket()` |
| 捕获器导出的单profile JSON | `Session(impersonate=指纹文件)` |

### 第一个可运行脚本

```python
from requests_rust import Session


# 【可调参数】先使用公开测试页面跑通，再换成自己的业务地址。
目标地址 = "https://example.com/"
指纹版本 = "edge152"


with Session(impersonate=指纹版本, timeout=30) as 会话:
    响应 = 会话.get(
        目标地址,
        headers={"accept": "text/html,application/xhtml+xml"},
    )
    响应.raise_for_status()
    print("状态码:", 响应.status_code)
    print("实际协议:", 响应.http_version)
    print("指纹编号:", 响应.fingerprint_id)
```

输出状态码后，先确认`response.fingerprint_id`非空，再把URL、Header、Cookie和Body替换为浏览器真实业务请求。不要为了“像浏览器”自行猜测`Origin`、`Referer`、`Sec-Fetch-*`或认证字段；应从该业务动作的DevTools请求或Copy as cURL得到。

仓库内三个脚本可以直接右键运行：

- [`examples/basic_sync.py`](examples/basic_sync.py)：同步请求。
- [`examples/async_concurrency.py`](examples/async_concurrency.py)：一个Session并发多请求。
- [`examples/custom_fingerprint.py`](examples/custom_fingerprint.py)：加载捕获器JSON。

## 同步请求

`Session` 适合顺序请求或现有同步业务代码。应通过上下文管理器或 `close()` 释放连接池资源。

```python
from requests_rust import Session

with Session(
    impersonate="chrome150",
    headers={"Accept": "application/json"},
    timeout=30,
) as session:
    response = session.get(
        "https://example.com/api/items",
        params={"page": 1, "tag": ["python", "rust"]},
        headers={"Authorization": "Bearer <token>"},
    )
    response.raise_for_status()
    print(response.json())
```

构造参数：

| 参数 | 含义 |
|---|---|
| `impersonate` | 必填：内置profile名称、单profile JSON路径、记录Sequence，或捕获器MCP `read_fingerprint_result`返回的Mapping。 |
| `fingerprint_rotation` | 默认为`True`，按请求自然轮换并复用已建立的指纹池；需要单一固定指纹时显式设为`False`。 |
| `headers` | Session 默认 Header，支持 `dict` 或二元组序列。序列可保留重复 Header 和顺序。 |
| `proxy` | Session 默认代理 URL。 |
| `proxies` | 协议到代理 URL 的映射，不能和 `proxy` 同时传入。 |
| `verify` | 默认为 `True`，验证 HTTPS 证书。仅受控本地自签名测试可设为 `False`。 |
| `timeout` | 请求总超时秒数，默认为 `30`。 |
| `connect_timeout` | 可选连接超时秒数。 |
| `read_timeout` | 可选响应 Body 读取超时秒数。 |
| `http_version` | 唯一的协议选择参数。默认`"http2"`，按HTTP/2→HTTP/1.1降级；`"http3"`在schema 2完整模板和直连HTTPS时优先H3并失败降H2→H1.1，schema 1或不支持H3的场景直接H2→H1.1；`"http1.1"`固定HTTP/1.1。单次请求可覆盖；`auto`等旧别名继续兼容。 |
| `happy_eyeballs_timeout` | IPv4/IPv6 Happy Eyeballs回退延迟，默认 `0.3` 秒；设为 `None` 关闭并行地址族回退。 |
| `resolve` | 可选域名到IPv4/IPv6列表的Session级静态映射，保留原URL、Host、SNI和证书域名。 |
| `dns_servers` | 可选本机DNS服务器列表，支持`IP`或`IP:端口`，使用UDP并在失败时回退TCP。 |
| `dns_timeout` | 自定义DNS单次查询超时，默认`5`秒。 |
| `fingerprints_path` | 可选自定义指纹 JSON 路径，仅在当前 Session 构造时独立加载。 |
| `max_connections` | Session内HTTP、代理、流和WebSocket共同使用的Rust原生活跃连接上限，默认`50`。 |
| `fingerprint_pool` | 是否保存多变体Profile对应的Rust Client，默认`True`；所有Profile都可复用匹配Origin和代理身份的物理连接。 |
| `fingerprint_pool_size` | 自定义多变体Profile最多允许多少个变体进入保存集合，默认`100`；当前内置版本均只有一条火种。 |
| `max_cached_origins` | 最多缓存多少个`Origin + 完整代理身份`路由，默认`4`；超限路由正常请求但使用临时Client。 |
| `max_response_bytes` | 普通和multipart响应解压后的Body上限，默认`64MiB`；超过立即中止并报错，流式响应不受该完整Body上限影响。 |
| `max_websocket_message_bytes` | 单条WebSocket消息和帧的最大字节数，默认`16MiB`；超过后协议层终止连接。 |
| `cookie_store` | 是否自动接收响应`Set-Cookie`并写入Session Jar，默认`True`；并发匿名爬虫可设为`False`并显式传请求Cookie。 |

`impersonate`直接传路径、记录Sequence或MCP Mapping时必须只包含一个`profile`，但可以包含该profile的多个指纹变体。Session自动识别profile，并继续遵循固定或轮换变体策略；这些快捷形式不能再同时传`fingerprints_path`。Mapping和Sequence会序列化到内存并在Rust构造阶段解析，不创建临时文件。

```python
async with AsyncSession(
    impersonate=r"C:\fingerprints\chrome146.json",
    max_connections=50,
) as session:
    response = await session.get("https://example.com")
```

## 原生异步请求

`AsyncSession` 直接等待 Rust 网络 Future，不经过 `asyncio.to_thread`。高并发业务应优先复用一个或多个长期存活的 `AsyncSession`，不要为每个请求重新创建 Session。

`max_connections` 控制同一个 `Session` 或 `AsyncSession` 同时占用的统一网络槽位，默认值为 `50`。该限制由 Rust/Tokio 原生 `Semaphore` 执行，Python 不维护信号量或连接释放回调。HTTP、HTTPS、HTTP 代理、SOCKS5、流式响应和 WebSocket 共用这一个上限，不会分别各获得 50 个槽位。普通请求在响应 Body 完整返回后由 Rust 释放许可；流式响应持有原生许可直到 EOF 或底层关闭；WebSocket 的许可由 Rust actor 持有到真实连接终止。

IPv6字面量使用标准方括号URL，例如 `https://[2001:db8::1]/`。域名同时解析出AAAA和A记录时，Rust连接器先尝试DNS结果中的首选地址族；超过 `happy_eyeballs_timeout` 仍未连接成功，就并行尝试另一个地址族，成功的一路继续，另一路取消。HTTP、HTTPS和WebSocket共用该连接器；`socks5h://` 仍把域名原样交给代理解析。

## 指定DNS

日常指定DNS只使用`dns_servers`：

```python
async with AsyncSession(
    impersonate="chrome146",
    dns_servers=["1.1.1.1", "8.8.8.8"],
) as session:
    response = await session.get("https://example.com/")
```

列表表示当前Session允许使用的上游DNS集合。Rust Hickory resolver负责A/AAAA、TTL缓存、多服务器并发、UDP查询和UDP失败后的TCP回退。单个DNS也必须写成列表，例如`dns_servers=["1.1.1.1"]`。未传时使用系统`getaddrinfo`。

单个`get()`也可以临时指定，只影响这一条请求，不污染Session默认DNS或其他并发请求：

```python
response = await session.get(
    "https://example.com/",
    dns_servers=["1.1.1.1", "8.8.8.8"],
    dns_timeout=3,
)
```

相同请求级DNS配置按指纹变体进入8项LRU Rust Client缓存；后续请求可复用Resolver TTL缓存和匹配路由的TCP/TLS/HTTP2连接。传`dns_servers=[]`表示这一次请求临时恢复系统DNS。`dns_timeout`必须和请求级`dns_servers`同时传入。

`resolve`不是日常指定DNS服务器的接口，只用于已经知道目标IP、但HTTPS/WSS仍必须保留原域名TLS语义的高级场景。例如固定CDN节点、固定代理入口IP或绕过错误DNS答案：

```python
async with AsyncSession(
    impersonate="chrome146",
    resolve={"example.com": ["203.0.113.10", "2001:db8::10"]},
) as session:
    response = await session.get("https://example.com/")
```

这里TCP连接使用映射IP，但URL、HTTP Host、TLS SNI和证书验证仍使用`example.com`。直接`get("https://IP/")`即使手写Host也无法保持相同SNI和证书校验，所以只有这种场景才使用`resolve`；普通DNS需求不要用它。

代理解析边界：

| 场景 | 代理服务器域名 | 最终目标域名 |
|---|---|---|
| HTTP代理 + HTTP/HTTPS/WS/WSS | 本机`resolve`/`dns_servers` | HTTP代理解析 |
| `socks5://` | 本机`resolve`/`dns_servers` | 本机`resolve`/`dns_servers` |
| `socks5h://` | 本机`resolve`/`dns_servers` | SOCKS代理解析 |
| 无代理 | 无 | 本机`resolve`/`dns_servers` |

HTTP代理CONNECT目标强制本机解析尚未公开，因为必须同时分离CONNECT地址与原始TLS SNI、证书域名和HTTP Host；不能通过把URL改成IP或关闭证书验证伪实现。

HTTP/3使用UDP/QUIC，当前HTTP CONNECT和SOCKS5代理链不能承载QUIC。配置这些代理时，`http_version="http3"`会直接在代理隧道内优先协商HTTP/2、必要时降级HTTP/1.1；不会绕过代理直连目标。未来只有支持SOCKS5 UDP ASSOCIATE或MASQUE CONNECT-UDP的代理路径才可能代理QUIC。

DNS性能应按业务实际任务模型测试。仓库的`benchmark_dns_performance.py`一次创建1000个独立`get()`任务并设置`max_connections=1000`，只限制任务总数，不再增加Worker并发层；结果用于比较系统DNS、Session指定DNS和单get指定DNS，不作为公网吞吐承诺。

```python
import asyncio
from requests_rust import AsyncSession


async def main():
    async with AsyncSession(
        impersonate="firefox151",
        max_connections=50,
        proxy="http://user:password@proxy.example:8080",
    ) as session:
        first, second = await asyncio.gather(
            session.get("https://example.com/api/one"),
            session.post("https://example.com/api/two", json={"id": 2}),
        )
        print(first.status_code, second.status_code)


asyncio.run(main())
```

## HTTP/3模式

Session级默认：

```python
with Session(
    impersonate="chrome152",
    http_version="http3",
) as session:
    response = session.get("https://example.com/")
    assert response.http_version in {"HTTP/3", "HTTP/2", "HTTP/1.1"}
```

单次覆盖与异步流：

```python
async with AsyncSession(impersonate="chrome152") as session:
    response = await session.get(
        "https://example.com/",
        http_version="http3",
        stream=True,
    )
    body = await response.aread()
```

`http3`是完整指纹模板下的协议偏好，不是绕过模板的许可。直连HTTPS且记录为schema 2时，回放器用quiche+BoringSSL应用捕获的TLS ClientHello、QUIC Transport Parameters、HTTP/3 SETTINGS、Header名称顺序和QPACK策略，并按指纹变体、Origin与解析目标隔离和复用连接。H3失败后降级H2/H1.1；安全方法可以直接尝试，首次非安全方法先用无业务Body的HEAD探测，因此真实业务Body只发送一次。重定向、Cookie、gzip/br/zstd/deflate解压、流式读取和响应大小限制在H3路径保持相同API语义。

schema 1没有完整H3模板，会直接从H2开始；普通HTTP/SOCKS代理、multipart、transfer_stats和请求级自定义DNS解析也走H2/H1.1，不会绕过代理或启动半指纹QUIC。收到H3响应头后的Body错误不会改走TCP重放。`response.http_version`始终给出实际协议。

同一个 Session 可以按任意比例混合协议和代理。例如保持 25 条 WebSocket，同时运行 15 个 HTTP 代理请求和 10 个 SOCKS5 请求，正好共同占用 50 个槽位。超过上限的任务会异步等待已有请求完成或 WebSocket 关闭，不会阻塞事件循环，也不需要创建额外 `AsyncSession`。

所有当前内置浏览器版本都只保存一条火种。`fingerprint_pool`和`fingerprint_pool_size`仅对调用方提供的多记录自定义Profile保留变体选择与Client缓存语义。

Chrome和Edge与`curl_cffi`采用相同连接模型：同一wreq Client复用匹配Origin和代理身份的TCP、HTTP代理CONNECT、TLS及HTTP/2连接；复用连接不会产生新ClientHello。连接池自然新建TLS连接时，BoringSSL重新随机排列允许变化的扩展，因此原始JA3可变。签名算法GREASE同样每次随机；检测器过滤GREASE后，JA3N和规范JA4保持对应浏览器版本语义。

`firefox151`同样只保存一条火种，五次真实完整握手均保持同一JA3和扩展顺序，因此继续缓存并复用Firefox Client与TLS/H2连接。需要新连接时，首次完整握手使用固定NSS JA3；命中共享TLS票据缓存后，恢复握手会自然增加PSK扩展41并形成对应的第二个JA3，这不是第二条Profile火种。

`max_cached_origins`按`scheme + host + port + 完整代理身份`的哈希计算。不同path、query和`params`不增加名额；换域名、端口、协议或代理session ID会进入独立路由。

业务代码不要发送`Connection: close`或`Connection: keep-alive`。HTTP/2禁止这类hop-by-hop Header，连接复用和自然重连由协议层管理。

普通非流式响应和multipart响应会在Rust中按解压后的字节数执行`max_response_bytes`限制，默认64MiB。gzip、Brotli、zstd或deflate小压缩包解压后超过上限也会被中止，不能依赖压缩前`Content-Length`绕过。需要处理更大响应时应使用`stream=True`分块消费，或在明确评估内存后提高该值。

`Session.close()`/`AsyncSession.close()`关闭连接许可并清理Session缓存，之后拒绝新请求。已经返回给调用方的stream或WebSocket是独立的活跃对象，不会被Session强制异步销毁；调用方仍应使用响应/WebSocket自己的`close()`或`aclose()`结束它们。这样避免另一个任务关闭Session时截断正在消费的数据，同时保持资源所有权明确。


异步方法：

```text
await session.request(method, url, ...)
await session.get(url, ...)
await session.post(url, ...)
await session.put(url, ...)
await session.patch(url, ...)
await session.delete(url, ...)
await session.head(url, ...)
await session.options(url, ...)
await session.close()
```

## 模块级快捷方法

无需手动创建 Session 时可以使用模块级方法；它们会为这一次调用创建并关闭 Session，适合低频简单请求，不适合高并发循环。

```python
from requests_rust import get, post

response = get("https://example.com/", impersonate="chrome150")
created = post(
    "https://example.com/api/items",
    impersonate="chrome150",
    json={"name": "demo"},
)
```

模块级 `request/get/post/put/patch/delete/head/options` 都需要传入 `impersonate`。

## 请求参数

同步和异步请求方法支持相同参数。

| 参数 | 含义 |
|---|---|
| `params` | `dict` 查询参数；列表或元组值会编码为重复 Query。 |
| `headers` | 请求级 Header，覆盖同名 Session 默认 Header。二元组序列保留重复 Header。 |
| `cookies` | `dict`、二元组列表、`Cookies` 或标准库 `CookieJar`。同名值只覆盖本请求。 |
| `data` | `bytes`、`str` 或 `dict`。`dict` 编码为 `application/x-www-form-urlencoded`。 |
| `json` | JSON 请求体，自动补 `application/json`，除非已提供 `content-type`。 |
| `files` | multipart 文件路径映射。不能和 `json` 同时使用。 |
| `timeout` | 覆盖 Session 请求总超时。 |
| `read_timeout` | 覆盖 Session Body 读取超时。 |
| `dns_servers` | 本请求的 DNS 服务器列表；不传时继承 Session，传空列表时仅本请求恢复系统 DNS。 |
| `dns_timeout` | 本请求的 DNS 查询超时；只能与请求级 `dns_servers` 同时传入。 |
| `stream` | `True` 时不预读完整响应 Body。multipart 响应暂不支持。 |
| `proxy` | 本请求代理覆盖；`None` 表示本请求直连。 |
| `proxies` | 本请求协议代理映射；不能和 `proxy` 同时使用。 |
| `allow_redirects` | 是否跟随重定向，默认为 `True`。 |
| `max_redirects` | 最大重定向次数，默认为 `10`。 |

代理映射示例：

```python
response = session.get(
    "https://example.com/",
    proxies={
        "https": "http://user:password@proxy.example:8080",
        "http": "http://user:password@proxy.example:8080",
    },
)
```

代理 URL 支持 `http://`、`socks5://` 和 `socks5h://`。`socks5://` 在本地解析目标域名，`socks5h://` 由代理解析目标域名；URL 中的用户名密码用于 SOCKS5 用户名密码认证。SOCKS5 按协议先协商认证方法，再在同一条连接内立即完成认证和 CONNECT，不使用 HTTP 的 407 挑战流程。

## WebSocket

`Session.websocket()` 和 `AsyncSession.websocket()` 支持 `ws://`、`wss://`、文本、二进制、Ping、Pong、Close、子协议、Cookie、请求 Header、HTTP 代理预认证和 SOCKS5 用户名密码认证。默认使用兼容性最广的 HTTP/1.1 Upgrade；传入 `version="http2"` 可请求 RFC 8441 Extended CONNECT，目标服务器必须明确通过 HTTP/2 SETTINGS 宣告支持该能力。HTTP/2 路径已通过本地端到端测试，实际验证了 Extended CONNECT 请求、`:protocol=websocket` 和双向 WebSocket 帧传输。

同步示例：

```python
from requests_rust import Session

with Session(impersonate="chrome146", proxy="http://user:password@proxy.example:8080") as session:
    with session.websocket(
        "wss://example.com/socket",
        headers={"Origin": "https://example.com"},
        protocols=["chat"],
    ) as websocket:
        websocket.send("hello")
        message = websocket.recv()
        if message is not None and message.type == "text":
            print(message.data)
```

异步示例：

```python
from requests_rust import AsyncSession

async with AsyncSession(impersonate="chrome146") as session:
    async with await session.websocket("wss://example.com/socket") as websocket:
        await websocket.send_bytes(b"payload")
        async for message in websocket:
            print(message.type, message.data)
            if message.type == "close":
                break
```

`WebSocketMessage.type` 为 `text`、`binary`、`ping`、`pong` 或 `close`。文本消息的 `data` 是 `str`，二进制和控制消息的 `data` 是 `bytes`，关闭消息还提供 `code` 与 `reason`。同一连接允许发送和接收并行进行，但同时只允许一个活动接收者；接收事件队列固定为 256 条，消费者长期跟不上时连接会终止，避免内存无限增长。

代理沿用 Session 和请求级覆盖语义。`ws://` 经 HTTP 代理时，Upgrade 请求首包直接携带 Basic 代理认证；`wss://` 经 HTTP 代理时，外层 CONNECT 首包直接携带认证；`ws://` 和 `wss://` 经 SOCKS5 时，在同一条 TCP 连接内完成用户名密码认证和 CONNECT。WebSocket 不支持 `transfer_stats=True`，该参数仅属于普通 HTTP 请求 API。

每次调用 `websocket()` 都会建立一条新的独立 WebSocket，不会复用另一条已经升级的 WebSocket。并发调用 50 次就会得到 50 个可分别收发和关闭的 WebSocket 对象。HTTP/2 WebSocket 在代理身份和目标完全相同时可以由底层协议复用物理 HTTP/2 连接，但每次调用仍是独立 Extended CONNECT stream；代理 URL 中的用户名、密码或 session ID 不同时，完整代理身份参与连接池分区，不会跨代理身份复用物理连接。

WebSocket 长连接会持续占用 `AsyncSession.max_connections` 中的一个槽位。若 `max_connections=50` 且已经保持 25 条 WebSocket，普通 HTTP、HTTPS 或 SOCKS5 请求最多还能同时占用 25 个槽位；这 25 个槽位可以按任意比例混合，不要求预先固定每种协议的数量。

WebSocket单条消息和单帧默认限制为16MiB；接收事件队列最多256条。超大消息或队列溢出都会明确终止连接并返回错误，不会把无限消息堆积到Python内存。需要更大业务消息时应在明确评估峰值内存后提高`max_websocket_message_bytes`。

WebSocket 握手、发送和 Close frame 写入均使用传给 `websocket()` 的 `timeout`。Rust actor 在普通帧发送发生背压时仍会优先接收 Close 命令；Close 可以中断当前发送，并在关闭写入超时后强制结束 actor、释放底层连接和原生连接许可。

## Cookie 与代理会话

`session.cookies` 是 Session 级 Cookie Jar，会自动接收响应中的 `Set-Cookie` 并按域、路径和 Secure 属性匹配后续请求。

```python
session.cookies.set("token", "value", url="https://example.com/")
print(session.cookies.get_dict())
session.cookies.delete("token", url="https://example.com/")
session.cookies.clear()
```

切换 Session 默认代理：

```python
session.set_proxy("http://user:password@proxy-b.example:8080")
```

`set_proxy()` 只应在代理身份真实变化时调用。它会清除旧代理的 Client、CONNECT、TCP、TLS 和 HTTP/2 链路，确保新代理不复用旧代理连接。高并发单请求换代理应使用请求级 `proxy=`，不要在多个协程中并发调用 `set_proxy()`。

请求级代理按完整代理身份隔离连接池。不同session ID绝不会串用代理认证；相同目标与相同代理身份可以复用CONNECT、TLS和HTTP/2连接。业务侧始终保持同一个`AsyncSession`，不要为每个代理session ID新建业务Session。

切换请求级代理时不要附加`Connection: close`。新的代理session ID已经形成独立路由；相同sticky ID继续复用现有链路。

对需保持IP的cursor分页，始终复用一个业务`AsyncSession`和同一个sticky代理session。所有Profile都优先复用匹配的TLS/H2连接，仅请求失败重试时更换sticky session ID。

## 响应对象与流

常用 `Response` 属性和方法：

```text
response.status_code
response.ok
response.url
response.headers
response.headers.get_list("set-cookie")
response.headers.raw
response.content
response.text
response.json()
response.history
response.fingerprint_id
response.impersonate
response.http_version
response.raise_for_status()
```

## TLS 传输层统计

默认不统计传输流量，普通请求不会创建计数器、计量中继或额外连接：

```python
response = await session.get("https://example.com/api", transfer_stats=True)
stats = response.transfer_stats

print(stats.upload_size)
print(stats.download_size)
print(stats.response_size)
print(stats.scope)
```

`transfer_stats=True` 返回的不是 `len(response.content)`、解压后的 Body 长度或 `Content-Length` 推算值。它统计专用计量路径上实际成功读写的 TCP payload 字节：TLS handshake、TLS record、HTTP Header、压缩后的响应 Body、HTTP/2 控制帧，以及使用 HTTP 代理时与上游代理的 CONNECT 请求/响应均会计入。

`upload_size` 和 `download_size` 分别是该请求外部链路的真实上行和下行字节；`response_size` 是两者之和；`scope` 固定为 `tcp_payload_through_metered_tunnel`。

此功能默认关闭。开启时为保证 HTTP/2 多路复用连接中的字节可以严格归属到当前请求，库会使用独占计量连接，因此不会复用该请求的既有 CONNECT、TLS 或 HTTP/2 连接。它适合审计、代理计费核对和传输诊断，不适合高吞吐业务热路径。

当前限制：

- 仅支持 HTTPS 请求。
- 仅支持普通完整响应，不支持 `stream=True` 或 multipart 上传。
- 仅支持直连或 `http://` 上游代理；HTTPS/SOCKS 上游代理无法在不改变代理 TLS 语义的前提下提供同一层级的准确 TCP 统计。

同步流式读取：

```python
with session.get("https://example.com/file", stream=True) as response:
    response.raise_for_status()
    for chunk in response.iter_content(64 * 1024):
        consume(chunk)
```

异步流式读取：

```python
response = await session.get("https://example.com/file", stream=True)
try:
    async for chunk in response.aiter_content(64 * 1024):
        consume(chunk)
finally:
    await response.aclose()
```

一个流式 `Response` 同时只能有一个活动消费者。提前退出循环时应显式 `close()` 或 `await aclose()`。

异步流响应禁止同步读取`response.content/text/json()`或调用同步`close()`，这些操作会阻塞事件循环。使用`await response.aread()`、`await response.atext()`、`await response.ajson()`或`await response.aclose()`。

`Session(proxies=...)`之后调用`set_proxy()`会清除原协议映射，后续默认请求改用新代理；请求级`proxy=`/`proxies=`仍只影响单次请求。

## Multipart 上传

```python
response = session.post(
    "https://example.com/upload",
    data={"title": "report"},
    files={
        "document": (
            "report.bin",
            r"C:\data\report.bin",
            "application/octet-stream",
        ),
    },
)
```

也可简写为：

```python
files = {"document": r"C:\data\report.bin"}
```

文件由 Rust 异步文件流读取，不需要先将整个文件加载为 Python `bytes`。

multipart可继续传同一个`http_version`参数；当前multipart不走UDP/H3，选择`http3`时直接从HTTP/2开始协商。

## 自定义指纹文件

指纹捕获器导出的单profile JSON可以直接交给`impersonate`。先确认文件内每条记录的`profile`相同，并检查`collector_browser.product`与`collector_browser.version`确实属于目标正式浏览器。

捕获器MCP结果可以原对象直传：

```python
# read_fingerprint_result返回的完整dict，包含schema_version和records。
捕获结果 = await mcp_client.call_tool(
    "read_fingerprint_result",
    {"task_id": 任务ID},
)
记录 = 捕获结果["records"][0]

with Session(impersonate=捕获结果, fingerprint_rotation=False) as 会话:
    响应 = 会话.get(
        "https://example.com/",
        headers=记录["http"]["headers"],
    )
```

`http.headers`从JSON解析后是二维list；API会把每项规范化为字符串二元组并保持顺序。Cookie、认证、CSRF、`Origin`、`Referer`和站点签名仍属于具体业务上下文，不应因为对象可直传就无条件复用其历史值。

```python
from pathlib import Path

from requests_rust import Session


# 【可调参数】替换为捕获器生成的真实文件和业务地址。
指纹文件 = Path(r"C:\fingerprints\edge152.json")
目标地址 = "https://example.com/"

# 二元组序列可以表达浏览器Header名称顺序和重复Header。
浏览器请求头 = [
    ("upgrade-insecure-requests", "1"),
    ("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    ("sec-fetch-site", "none"),
    ("sec-fetch-mode", "navigate"),
    ("sec-fetch-user", "?1"),
    ("sec-fetch-dest", "document"),
]

with Session(
    impersonate=指纹文件,
    fingerprint_rotation=False,
    headers=浏览器请求头,
    timeout=30,
) as 会话:
    print("文件内指纹数量:", 会话.fingerprint_count)
    响应 = 会话.get(目标地址)
    响应.raise_for_status()
    print("源记录ID:", 响应.fingerprint_id)
```

路径和对象快捷形式有以下契约：

1. 文件必须只包含一个`profile`，但允许该profile有多个变体。
2. Session自动识别profile；不能同时再传`fingerprints_path`。
3. 文件只影响当前Session，不进入进程级内置缓存。
4. `response.fingerprint_id`是本次实际选择的源记录`id`，可用于审计和对撞。
5. 捕获器的裸数组、Python记录Sequence和完整MCP `{schema_version, records, ...}` Mapping都可直接加载；对象输入不落盘。
6. 文件不存在、JSON损坏、混有多个profile、未知schema或缺少TLS字段时，构造期直接报错。
7. 未知非GREASE cipher/group/key-share/signature/extension、未支持HTTP/2 SETTINGS、重复ID、`header_order`冲突，以及`extension_wire`的Base64、长度或顺序错误都会fail-closed；不会“请求成功但静默漏指纹”。

需要从多profile文件中显式选择时，使用完整形式：

```python
会话 = Session(
    impersonate="edge152",
    fingerprints_path=r"C:\fingerprints\all_profiles.json",
)
```

自定义记录只保存TLS/HTTP2传输画像和跨请求稳定Header。SNI、TLS Random、KeyShare公钥、ticket、PSK binder、Host、Content-Length、Cookie、认证、CSRF和业务签名必须由运行时或业务代码生成。

Chromium会在自然新建TLS连接时随机排列允许变化的扩展，并为GREASE生成新值。线级验证应归一化GREASE和允许乱序的扩展，不能要求每次原始JA3哈希、扩展位置或随机载荷摘要完全相同。

真实闭环验收（2026-09-02，Google Chrome 152.0.7977.64）：捕获器正式输出经MCP形状dict直接构造Session，源记录ID`0edd239e7a79413bbdc407d48562617f`；源与回放ClientHello均为1914字节，规范JA4均为`t13i1516h2_8daaf6152771_cb7bf5808d99`，Trust Anchor Identifiers `0xca34`的206字节payload完全一致，HTTP/2 Akamai参数均为`1:65536;2:0;4:6291456;6:262144|15663105|1:1:0:256|m,a,s,p`。

## 常见问题排查

### `内置指纹中没有 <路径>`

传入的路径不存在时，库会把字符串按profile名称处理。优先使用`Path`或绝对路径，并先检查`指纹文件.is_file()`。

### `无法构建wreq Client`

通常表示捕获JSON中的TLS算法无法映射到当前BoringSSL。不要回退成旧profile名称掩盖问题；保留源JSON、浏览器完整版本和错误信息，提交Issue。

### 请求成功但Header不对

profile不会固化`accept`、`origin`、`referer`、`sec-fetch-*`、`priority`、Cookie或认证字段。把浏览器真实请求Header按二元组序列传入，名称顺序由profile的捕获顺序参与线级发送。

### 使用新浏览器版本但profile名称没变

profile不会自动升级。必须重新采集、线级对撞并新增对应大版本；禁止把旧TLS记录改名后冒充最新版。

### 异步任务越来越多

长期复用一个`AsyncSession`并设置合理的`max_connections`。不要为每条请求创建Session，也不要再叠一层无界Python任务或业务batch。

## 重要边界

- `http3`在schema 2完整模板、直连HTTPS时真实应用TLS/QUIC/H3/QPACK并优先H3；schema 1或不支持H3的场景从H2开始，必要时降级H1.1。WebSocket继续使用自己的`version`参数。
- 不同指纹变体使用独立 Client、连接池和 TLS session cache，这是指纹隔离要求。
- 切换Session默认代理或关闭Session会清空TLS session cache，避免旧代理身份签发的ticket用于新代理路径。
- 调用方传入的 `User-Agent`、`sec-ch-ua*` 与其他同名 Header 优先，库不会覆盖。
- `Accept`、`Origin`、`Referer`、`Sec-Fetch-*`、Cookie、Authorization、CSRF 和业务签名必须根据当前业务请求传入。
- `verify=False` 只应用于受控测试；生产 HTTPS 请求应保持证书验证。
