# requests_rust 使用与 API 手册

`requests_rust` 是 Python 3.10+ 的浏览器指纹 HTTP 与 WebSocket 客户端。网络由 Rust 执行，支持 TLS/HTTP2 指纹、HTTP/1.1、HTTP/2、HTTP/3直连、IPv4/IPv6、HTTP 与 SOCKS5 代理、Cookie、重定向、流式响应、multipart 和 WebSocket。

高频 API 命名接近 `curl_cffi.requests`，但未实现的关键字参数会抛出 `TypeError`，不会被静默忽略。

## 安装

从私有 `requests_rust` 发布仓库的 Release 下载与系统、CPU 架构匹配的 wheel，再安装：

```bash
pip install requests_rust-0.3.0-cp310-abi3-win_amd64.whl
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

当前内置 profile 为 `chrome146`、`chrome150`、`edge152` 和 `firefox151`。它们是已采集并验证的固定浏览器大版本快照，不代表官网当前 Stable，也不会自动跟随浏览器升级。`edge152`来自本机Edge 152.0.4191.53，产品UA为`Edg/152.0.0.0`。profile 决定 TLS/HTTP2 指纹行为，不应当用它保存业务 Cookie、认证 Header、CSRF、签名、`Referer`、`Origin`、时间戳或 nonce。

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
| `impersonate` | 必填，内置/自定义指纹中的 profile 名称，或直接传指纹 JSON 文件路径。 |
| `fingerprint_rotation` | 默认为`True`，按请求自然轮换并复用已建立的指纹池；需要单一固定指纹时显式设为`False`。 |
| `headers` | Session 默认 Header，支持 `dict` 或二元组序列。序列可保留重复 Header 和顺序。 |
| `proxy` | Session 默认代理 URL。 |
| `proxies` | 协议到代理 URL 的映射，不能和 `proxy` 同时传入。 |
| `verify` | 默认为 `True`，验证 HTTPS 证书。仅受控本地自签名测试可设为 `False`。 |
| `timeout` | 请求总超时秒数，默认为 `30`。 |
| `connect_timeout` | 可选连接超时秒数。 |
| `read_timeout` | 可选响应 Body 读取超时秒数。 |
| `http_version` | 默认`"auto"`；可选`"http1"`、`"http2"`或`"http3"`。单次请求可用同名参数覆盖。 |
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

`impersonate` 直接传路径时，文件必须只包含一个 `profile`，但可以包含该 profile 的多个指纹变体。Session 会自动识别 profile，并继续遵循固定或轮换变体策略。此时不能再同时传 `fingerprints_path`。

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

HTTP/3使用UDP/QUIC直连，不经过当前HTTP CONNECT或SOCKS5 TCP代理栈。Session配置了代理或单次请求传入代理时，`http_version="http3"`会明确报错，不会降级为HTTP/2。

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
    impersonate="chrome150",
    http_version="http3",
) as session:
    response = session.get("https://example.com/")
    assert response.http_version == "HTTP/3"
```

单次覆盖与异步流：

```python
async with AsyncSession(impersonate="chrome150") as session:
    response = await session.get(
        "https://example.com/",
        http_version="h3",
        stream=True,
    )
    body = await response.aread()
```

HTTP/3是prior-knowledge模式，只接受`https://`，不会先发HTTP/1.1请求探测`Alt-Svc`，也不会失败后静默回退。它使用Reqwest、Quinn和Rustls，与HTTP/1.1/2的wreq、BoringSSL后端分离。`impersonate`在HTTP/3模式中仍提供默认Header和公开元数据，但不复现Chrome/Edge的BoringSSL ClientHello、QUIC Transport Parameters或QPACK指纹。

当前HTTP/3支持普通同步/异步请求、Body、Cookie、重定向、读取超时、`max_response_bytes`和同步/异步流式响应。不支持HTTP/SOCKS代理、multipart、WebSocket及基于TCP隧道的`transfer_stats`；这些组合会在调用边界明确失败。

同一个 Session 可以按任意比例混合协议和代理。例如保持 25 条 WebSocket，同时运行 15 个 HTTP 代理请求和 10 个 SOCKS5 请求，正好共同占用 50 个槽位。超过上限的任务会异步等待已有请求完成或 WebSocket 关闭，不会阻塞事件循环，也不需要创建额外 `AsyncSession`。

所有当前内置浏览器版本都只保存一条火种。`fingerprint_pool`和`fingerprint_pool_size`仅对调用方提供的多记录自定义Profile保留变体选择与Client缓存语义。

Chrome和Edge与`curl_cffi`采用相同连接模型：同一wreq Client复用匹配Origin和代理身份的TCP、HTTP代理CONNECT、TLS及HTTP/2连接；复用连接不会产生新ClientHello。连接池自然新建TLS连接时，BoringSSL重新随机排列允许变化的扩展，因此原始JA3可变，JA3N和JA4保持对应浏览器版本语义。

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

HTTP/3模式暂不支持multipart；文件上传继续使用`auto`、`http1`或`http2`模式。

## 自定义指纹文件

```python
session = Session(
    impersonate=r"D:\fingerprints\firefox151.json",
)
```

路径形式要求文件只包含一个 profile，可包含该 profile 的多个变体；Session 自动识别 profile。需要从多 profile 文件中显式选择时，继续使用 `impersonate="profile名称", fingerprints_path=路径`。自定义文件只影响该实例。文件记录应只包含 TLS/HTTP2 指纹数据；SNI、TLS Random、KeyShare、ticket、PSK binder、Host、Content-Length、业务 Header、Cookie、认证态和签名参数都是运行态数据。

## 重要边界

- HTTP/3为显式QUIC直连模式，不支持当前TCP代理、multipart、WebSocket或`transfer_stats`，也不宣称复现浏览器QUIC指纹。
- 不同指纹变体使用独立 Client、连接池和 TLS session cache，这是指纹隔离要求。
- 调用方传入的 `User-Agent`、`sec-ch-ua*` 与其他同名 Header 优先，库不会覆盖。
- `Accept`、`Origin`、`Referer`、`Sec-Fetch-*`、Cookie、Authorization、CSRF 和业务签名必须根据当前业务请求传入。
- `verify=False` 只应用于受控测试；生产 HTTPS 请求应保持证书验证。
