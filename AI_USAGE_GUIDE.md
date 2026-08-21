# requests_rust 0.3.0 AI使用手册

## 定位

`requests_rust`是Python 3.10+的浏览器指纹HTTP、代理和WebSocket原生扩展。Python负责逐请求参数描述和结果消费；Rust负责DNS、IPv4/IPv6、代理、BoringSSL TLS、HTTP/1.1、HTTP/2、WebSocket、连接池、Cookie、超时、流、multipart和并发调度。异步网络路径直接使用进程级Tokio Runtime，不经过 `asyncio.to_thread`。

当前完整回归成品为`requests_rust-0.3.0-cp310-abi3-win_amd64.whl`，支持64位CPython 3.10及以上普通GIL版本。wheel内置BoringSSL、wreq、Tokio、指纹和Python API包装，不需要Rust、CMake、Visual Studio、Playwright、curl_cffi或额外VC++运行库。Windows ARM64、Linux x64、Linux ARM64、macOS Intel和macOS Apple Silicon wheel已在对应原生GitHub runner完成构建和安装冒烟；必须安装与平台、CPU匹配的wheel，安装冒烟不替代完整协议回归。

## 导入

```python
from requests_rust import (
    AsyncSession,
    AsyncWebSocket,
    Cookies,
    Headers,
    Response,
    Session,
    WebSocket,
    WebSocketMessage,
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
```

内置Profile：`chrome142`、`chrome146`、`chrome150`、`firefox151`。

```python
print(available_profiles())
```

## Session

```python
Session(
    *,
    impersonate: str | os.PathLike[str],
    fingerprint_rotation: bool = False,
    headers=None,
    proxy: str | None = None,
    proxies: Mapping[str, str | None] | None = None,
    verify: bool = True,
    timeout: float = 30,
    connect_timeout: float | None = None,
    read_timeout: float | None = None,
    fingerprints_path: str | None = None,
    max_connections: int = 50,
    happy_eyeballs_timeout: float | None = 0.3,
)
```

`fingerprints_path`传入指纹JSON文件路径时，该实例在构造时独立读取并解析文件（提前单实例加载），指纹只属于这个实例，不进入进程级全局缓存；多个实例可同时各读各的文件，互不干扰。文件格式与内置 `fingerprints.json` 一致（`id`、`profile`、`tls`、`http` 字段的指纹记录列表）。不传时回退到编译进wheel的内置指纹。文件不存在或解析失败在构造时直接抛错；实例选择的版本在该文件中不存在时，错误信息会列出该文件里的可用版本。

`impersonate`也可直接传上述指纹JSON路径。该快捷形式要求文件中只有一个profile，但允许该profile包含多个变体；Session自动识别profile，不需要再传`fingerprints_path`。若文件中混有多个profile，或同时传入两种路径参数，构造时会直接报错，避免选错浏览器指纹。

`max_connections`是Session内HTTP、HTTPS、HTTP代理、SOCKS5、流和WebSocket共同使用的Rust/Tokio原生上限。`happy_eyeballs_timeout`默认0.3秒；双栈域名首选地址族未及时连接时，Rust连接器并行尝试另一个地址族，设为`None`可关闭该回退。

profile 默认 Header 只在调用方没有传同名 Header 时兜底。浏览器 Copy as cURL 或业务代码传入的所有 Header，包括 `User-Agent` 与 `sec-ch-ua*`，均原样优先，库不接管或替换。`accept`、`origin`、`referer`、`upgrade-insecure-requests`、`sec-fetch-*`、`priority`、Cookie 与认证 Header 都取决于当前请求上下文，不会从一次浏览器导航采集记录中固化；调用方应按实际请求传入。

同步示例：

```python
from requests_rust import Session

with Session(impersonate="chrome150") as session:
    response = session.post(
        "https://example.com/api",
        params={"page": 1},
        headers=[("X-Test", "one"), ("X-Test", "two")],
        json={"name": "测试"},
        timeout=30,
        read_timeout=10,
        allow_redirects=True,
        max_redirects=10,
    )
    response.raise_for_status()
    print(response.json())
```

支持方法：`request`、`get`、`post`、`put`、`patch`、`delete`、`head`、`options`。

`requests_rust` 优先与 `curl_cffi.requests` 保持高频名称和参数一致，方便替换既有调用；但不承诺兼容其全部专有参数、底层对象或 TLS 行为。未实现的关键字参数会抛出 `TypeError`，不会被静默忽略。

请求关键字参数：

| 参数 | 语义 |
|---|---|
| `params` | 使用Python `urlencode(..., doseq=True)`追加Query |
| `headers` | Mapping或二元组序列；序列保留重复Header及顺序 |
| `cookies` | curl_cffi兼容的 `dict[str, str]`、`list[tuple[str, str]]`、`Cookies`或标准 `CookieJar` |
| `data` | `bytes`、`str`或Mapping；Mapping编码为表单 |
| `json` | 由Python `json.dumps(ensure_ascii=False, separators=(",", ":"))`编码，兼容优先 |
| `files` | 文件路径Mapping；由Rust Tokio文件流读取 |
| `timeout` | 请求总超时，必须是有限正数 |
| `read_timeout` | Body读取超时，必须是有限正数 |
| `stream` | `True`时不预读完整Body |
| `proxy` | 单请求代理覆盖；`None`表示该请求直连；省略表示使用Session代理 |
| `proxies` | curl_cffi 风格代理映射，如 `{"http": "http://...", "https": "http://..."}`；按目标 URL 协议选择，也接受 `http://`、`https://`、`all://`、`all` 键；不能与 `proxy` 同时传入 |
| `allow_redirects` | 是否跟随重定向 |
| `max_redirects` | 最大跳转次数，不能小于0 |

`json`与 `files`不能同时使用。multipart的 `data`必须是Mapping或 `None`。multipart响应暂不支持 `stream=True`。

## AsyncSession

异步接口参数与同步接口一致，普通请求、流建立、Body异步读取和multipart均直接等待Rust Future。

```python
import asyncio
from requests_rust import AsyncSession


async def main():
    async with AsyncSession(
        impersonate="firefox151",
    ) as session:
        first, second = await asyncio.gather(
            session.get("https://example.com/1"),
            session.post("https://example.com/2", json={"id": 2}),
        )
        print(first.status_code, second.status_code)


asyncio.run(main())
```

取消Python awaitable会丢弃对应Rust Future。已经发出的网络数据不能撤回，但不会占用Python工作线程。

## WebSocket

同步入口为`Session.websocket()`，异步入口为`await AsyncSession.websocket()`。支持`ws://`、`wss://`、HTTP/1.1 Upgrade、RFC 8441 Extended CONNECT、文本、二进制、Ping、Pong、Close、子协议、Cookie、请求Header、HTTP代理Basic预认证和SOCKS5用户名密码认证。

```python
async with AsyncSession(impersonate="chrome146", max_connections=50) as session:
    websocket = await session.websocket(
        "wss://example.com/socket",
        proxy="http://user:password@proxy.example:8080",
    )
    await websocket.send("hello")
    message = await websocket.recv()
    await websocket.close()
```

每次`websocket()`调用创建独立逻辑WebSocket。WebSocket的Rust permit由actor持有到真实连接终止；普通发送发生背压时，独立Close通道可以抢占发送。握手、发送和Close frame写入均受该连接`timeout`约束。

## 独立任务与重试

真实爬虫应复用一个 `AsyncSession`，由固定数量 Worker 持续取得单条任务并独立发包。每一条请求都可带自己的 Header、Cookie、代理和超时；成功任务立即确认，失败任务只重试自身，不会因同批其他任务成功或失败而被阻塞。

禁止实现或建议Rust原生`batch()`、`submit_many()`和库内常驻业务Worker队列。该路线已经证伪：不同代理session ID仍必须建立独立代理链路，batch不能减少主要网络成本；当前请求已经是Rust Future并受统一Rust `Semaphore`约束；额外batch只会重复并发、取消、错误、重试、Cookie和关闭语义。业务批量调度必须保持为Python有限Worker逐条请求，网络库不接管业务队列。

```python
async def worker(session, queue):
    while task := await queue.get():
        try:
            response = await session.get(
                task["url"],
                headers=task.get("headers"),
                cookies=task.get("cookies"),
                proxy=task.get("proxy"),
            )
            await confirm_success(task, response)
        except Exception as error:
            await retry_task(task, error)
        finally:
            queue.task_done()
```

## 流式响应

同步：

```python
with session.get(url, stream=True) as response:
    for chunk in response.iter_content(64 * 1024):
        consume(chunk)
```

异步：

```python
response = await session.get(url, stream=True)
try:
    async for chunk in response.aiter_content(64 * 1024):
        consume(chunk)
finally:
    await response.aclose()
```

流生命周期：

- 同一个 `Response`只允许一个活动消费者。
- `close()`立即发出非阻塞关闭通知，不等待网络读取结束。
- `await aclose()`等待Rust Body资源完成释放。
- 迭代正常结束、异常或取消时通过 `finally`清理。
- Python在 `for/async for`中 `break`不保证立即关闭生成器；提前退出时显式调用 `close()`或 `aclose()`。
- Body错误是终态，后续读取返回相同错误。

## Multipart

```python
response = session.post(
    "https://example.com/upload",
    data={"title": "demo"},
    files={
        "document": (
            "report.bin",
            r"C:\data\report.bin",
            "application/octet-stream",
        )
    },
)
```

简写：`files={"document": r"C:\data\report.bin"}`。文件内容由Rust异步文件流读取，不先复制到Python `bytes`。

## Response

主要属性和方法：

```text
status_code: int
headers: Headers
content: bytes
text: str
url: str
history: list[Response]
fingerprint_id: str
impersonate: str
ok: bool
json() -> Any
raise_for_status() -> None
iter_content(chunk_size) -> Iterator[bytes]
aiter_content(chunk_size) -> AsyncIterator[bytes]
close() -> None
aclose() -> Awaitable[None]
```

`Headers`大小写不敏感。`headers.get_list(name)`返回重复值列表；`headers.raw`保留完整二元组序列。

## Cookie与代理

```python
session.cookies.set("token", "value", url="https://example.com/")
session.cookies.get("token")
session.cookies.get_all()
session.cookies.get_dict()
session.cookies.delete("token", url="https://example.com/")
session.cookies.clear()

session.set_proxy("http://proxy-a:8080")
session.get(url, proxy="http://proxy-b:8080")
session.get(url, proxy=None)
session.get(url, cookies={"token": "request-only"})
```

Session默认代理在设置时解析一次。单请求代理只影响该请求。Cookie Jar在一个Session的所有指纹变体间共享。`cookies=`按curl_cffi语义与Session Jar合并：本次同名值覆盖Session值，未同名Session Cookie保留，响应 `Set-Cookie`仍写回Jar；同时存在手写 `Cookie` Header时以 `cookies=`合并结果为准。

代理URL支持`http://`、`socks5://`和`socks5h://`。HTTP/HTTPS CONNECT首包直接携带Basic认证；SOCKS5在同一TCP连接内协商并发送用户名密码。`socks5://`本地解析目标域名，`socks5h://`把域名交给代理。代理字符串中的协议仍表示代理协议，WebSocket目标使用`ws://`或`wss://`，HTTP代理不能为了“更快”改写成`ws://`。

## 400并发动态请求

同一个业务Session可以执行多个原生请求 Future，每次切换 Header、请求 Cookie 覆盖值和代理：

```python
responses = await asyncio.gather(*(
    session.get(
        url,
        headers={"X-Request-Id": str(index)},
        cookies={"request_cookie": index},
        proxy=f"http://session-{index}:password@proxy.example:8080",
    )
    for index in range(400)
))
```

并发安全规则：

- 保持并复用同一个 `AsyncSession`，不要为了切代理重建业务Session。
- 动态Header使用每请求 `headers=`。
- 动态Cookie使用每请求 `cookies=`，同名值只覆盖本请求，未同名公共Jar Cookie继续保留；不要在400个协程中并发修改共享Jar。
- 动态代理使用每请求 `proxy=`，不要并发调用修改全局状态的 `set_proxy()`。
- 代理供应商通过用户名中的session ID换IP时，只替换代理URL中的session ID。
- 进入Rust前，Header、本请求Cookie覆盖值和代理都已成为该请求的独立快照。
- 连接池键包含目标、协议版本和完整代理身份；同一代理地址仅session ID不同时也不会跨身份复用HTTP、CONNECT或SOCKS5连接。

本地代理回显实测已确认400条请求的Header、Cookie和代理用户名逐条匹配。每条请求使用不同代理认证session ID，因此吞吐主要受独立代理连接和机器调度影响；不再把单轮本地吞吐记录作为固定性能承诺。

WebSocket并发遵循相同代理隔离规则，但连接生命周期不同：每次 `websocket()` 调用都创建一条独立逻辑WebSocket，不从普通HTTP空闲池中取出一条既有WebSocket。已用同一个 `AsyncSession` 同时保持50条连接进行本地协议验证；HTTP代理和SOCKS5代理两组测试均确认50个不同session ID对应50次独立代理认证、50个不同客户端TCP连接，并且所有WebSocket可同时收发和关闭。若代理身份与目标完全相同，RFC 8441允许多个独立WebSocket stream复用一条物理HTTP/2连接，这不等于复用同一个WebSocket对象。

`Session(max_connections=50)` 和 `AsyncSession(max_connections=50)` 使用Rust/Tokio原生 `Semaphore` 约束活跃网络操作，Python层不维护许可队列或释放回调。HTTP、HTTPS、HTTP代理、SOCKS5、流式响应和WebSocket共同竞争这50个槽位，不是每种协议各自50个。WebSocket permit由Rust actor持有到真实连接终止；普通非流式请求在Body完成后自动释放；流式响应的原生对象持有permit到EOF或底层关闭。取消Rust Future会自动丢弃尚未交给长生命周期对象的 `OwnedSemaphorePermit`，Session关闭会在Rust中关闭Semaphore并唤醒等待者。本地混合测试已确认25条WebSocket、15个HTTP代理请求和10个SOCKS5请求可同时占满50个槽位。

IPv4和IPv6由同一个Rust连接器支持。`happy_eyeballs_timeout`默认0.3秒：双栈域名的首选地址族在该时间内未连接成功时，并行尝试另一个地址族；设为`None`可关闭并行回退。IPv6字面量URL必须写成`http://[::1]:端口/`。`socks5://`在本机解析后可以连接IPv4或IPv6目标，`socks5h://`将域名交给代理解析。

WebSocket握手、普通帧发送和Close frame写入都受本次连接的 `timeout` 约束。Rust actor使用可抢占发送路径：连接发生写背压时，Close命令可以中断当前普通帧发送；关闭写入超时后actor直接退出并通过RAII归还permit，避免一条半开连接永久占住整个Session容量。

长期任务推荐固定Worker模式：

```python
next_index = 0
index_lock = asyncio.Lock()


async def worker():
    global next_index
    while True:
        async with index_lock:
            if next_index >= task_count:
                return
            index = next_index
            next_index += 1
        response = await session.get(
            url,
            headers={"X-Request-Id": str(index)},
            cookies={"request_cookie": str(index)},
            proxy=build_proxy(index),
        )
        response.raise_for_status()


await asyncio.gather(*(worker() for _ in range(concurrency)))
```

项目性能、资源和稳定性脚本均采用这种普通请求Worker模式。本地keep-alive基准只用于版本内回归比较，不能外推到公网目标或不同代理供应商；公开文档不再保留容易脱离硬件、并发上限和测试版本语境的绝对吞吐承诺。

## 指纹轮换与隔离

`fingerprint_rotation=False` 时，一个 `AsyncSession`/`Session` Python 发包对象会在当前 `impersonate` Profile 的可独立复现变体中随机选择一个。该对象使用同一代理会话时始终保持该指纹，以复用同一变体的 Client、连接池和 TLS Session Cache，适合 Facebook 等连续 cursor 分页。

当该 Python 发包对象调用 `set_proxy()` 将代理会话从 A 切换到 B 时，会清空旧代理的连接链路，并在同一 Profile 中重新随机选择一个变体；重复设置同一个代理会话不会改变当前指纹。依赖服务端既有 TLS ticket 的 PSK 恢复握手记录不会作为新代理会话的首个随机指纹。

`fingerprint_rotation=True` 按原子计数器循环该 Profile 的全部变体。每个变体独立拥有 Client、连接池和 TLS Session Cache，不同指纹绝不共享 H2/TLS 连接；同一 Session 共享 Cookie Jar。多个 Session 不共享 Cookie、代理、连接池或 TLS Session Cache。

## GIL模型

需要GIL的阶段：解析Python参数、Python JSON编码、创建Python awaitable、把Rust结果转换为Python对象、执行用户Python代码。

不持有GIL的阶段：DNS、TCP、代理握手、TLS、HTTP、连接池、重定向、Body读写、multipart文件读取、Cookie更新、超时和指纹轮换。

同步请求等待期间通过PyO3释放GIL。异步请求不使用Python线程池。冷Client构建在Tokio `spawn_blocking`中执行，不阻塞Python事件循环。进程级Tokio Runtime将blocking线程上限固定为32，防止大量异步结果转换扩张到数百个线程。

## 关闭语义

`Session.close()`和 `await AsyncSession.close()`禁止新请求并释放缓存的空闲Client。关闭前已经接受的请求持有独立Client引用，可以继续完成。关闭后请求、代理修改和Cookie操作统一失败。

## 错误边界

- `timeout`、`connect_timeout`、`read_timeout`和非空`happy_eyeballs_timeout`必须是有限正数；NaN、Infinity、0和负数会被拒绝，不会触发Rust panic。
- 无效方法、Header、代理、URL、重定向、连接、TLS和Body错误转换为Python异常。
- `raise_for_status()`在状态码不属于200至399时抛出 `RuntimeError`。
- `Response.ok`定义为 `200 <= status_code < 400`。

## 已验证范围

- Chrome 150真实浏览器与requests_rust：41/41，100%。
- Firefox 151真实浏览器与requests_rust：5个变体均41/41，100%。
- HTTP/2 fork：431 passed、0 failed、1 ignored；文档测试40 passed、0 failed、1 ignored。
- Python API覆盖动态代理、Cookie、重复Header、重定向、超时、同步/异步流、取消、并发读拒绝、multipart、Session关闭竞态和JSON兼容。
- WebSocket覆盖同步/异步、WS/WSS、HTTP代理、WSS CONNECT预认证、SOCKS5、服务端Close、控制帧边界、50条不同代理身份和25 WS + 15 HTTP + 10 SOCKS5混合上限。
- RFC 8441本地端到端测试验证CONNECT、`:protocol=websocket`和双向帧；Webshare真实外网代理验证HTTPS出口与WSS文本回显。
- IPv6在`[::1]`完成同步和异步HTTP直连；双栈域名由wreq默认RFC 6555 Happy Eyeballs连接器处理。

## 已知边界

- 原生wheel必须与操作系统和CPU架构匹配；Windows ARM64、Linux x64、Linux ARM64、macOS Intel和Apple Silicon已通过原生安装冒烟，当前完整协议回归以Windows x64为准。
- 当前manylinux目标依赖glibc，不代表Alpine musl支持。
- 当前不实现HTTP/3发送。
- 不支持跨指纹连接池复用，这是保证指纹真实性的必要限制。
- TCP字段来自浏览器和requests_rust共享的Windows内核网络栈，不代表能在其他系统伪造Windows TCP SYN。
- 普通响应Body最终需要复制为Python `bytes`；Rust内部直接保留wreq返回的 `Bytes`，已去掉中间 `Bytes -> Vec<u8>`完整拷贝。大文件仍应使用流式接口降低峰值内存。
