# requests_rust 0.3.0 AI使用手册

## 定位

`requests_rust`是Python 3.10+的浏览器指纹HTTP、HTTP/3直连、代理和WebSocket原生扩展。Python负责逐请求参数描述和结果消费；Rust通过wreq/BoringSSL执行HTTP/1.1/2及浏览器TLS指纹，通过Reqwest/Quinn/Rustls执行HTTP/3，通过统一Tokio Runtime管理DNS、IPv4/IPv6、连接池、Cookie、超时、流、multipart和并发调度。异步网络路径不经过`asyncio.to_thread`。

当前完整回归成品为`requests_rust-0.3.0-cp310-abi3-win_amd64.whl`，支持64位CPython 3.10及以上普通GIL版本。wheel内置BoringSSL、wreq、Rustls、Quinn、Reqwest、Tokio、指纹和Python API包装，不需要Rust、CMake、Visual Studio、Playwright、curl_cffi或额外VC++运行库。Windows ARM64、Linux x64、Linux ARM64和macOS Apple Silicon wheel由对应原生GitHub runner构建和安装冒烟；必须安装与平台、CPU匹配的wheel。

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

内置Profile：`chrome146`、`chrome150`、`edge152`、`firefox151`。这些名称是已采集并验证的固定快照，不代表官网当前Stable，也不会自动随浏览器升级。`edge152`的火种来自本机Edge 152.0.4191.53，UA为`Edg/152.0.0.0`。

```python
print(available_profiles())
```

## Session

```python
Session(
    *,
    impersonate: str | os.PathLike[str],
    fingerprint_rotation: bool = True,
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
    resolve: Mapping[str, str | Sequence[str]] | None = None,
    dns_servers: Sequence[str] | None = None,
    dns_timeout: float | None = 5.0,
    fingerprint_pool: bool = True,
    fingerprint_pool_size: int = 100,
    max_cached_origins: int = 4,
    max_response_bytes: int = 64 * 1024 * 1024,
    max_websocket_message_bytes: int = 16 * 1024 * 1024,
    cookie_store: bool = True,
)
```

`fingerprints_path`传入指纹JSON文件路径时，该实例在构造时独立读取并解析文件（提前单实例加载），指纹只属于这个实例，不进入进程级全局缓存；多个实例可同时各读各的文件，互不干扰。文件格式与内置 `fingerprints.json` 一致（`id`、`profile`、`tls`、`http` 字段的指纹记录列表）。不传时回退到编译进wheel的内置指纹。文件不存在或解析失败在构造时直接抛错；实例选择的版本在该文件中不存在时，错误信息会列出该文件里的可用版本。

`impersonate`也可直接传上述指纹JSON路径。该快捷形式要求文件中只有一个profile，但允许该profile包含多个变体；Session自动识别profile，不需要再传`fingerprints_path`。若文件中混有多个profile，或同时传入两种路径参数，构造时会直接报错，避免选错浏览器指纹。

`max_connections`是Session内HTTP、HTTPS、HTTP代理、SOCKS5、流和WebSocket共同使用的Rust/Tokio原生上限。`happy_eyeballs_timeout`默认0.3秒；双栈域名首选地址族未及时连接时，Rust连接器并行尝试另一个地址族，设为`None`可关闭该回退。

`fingerprint_pool`和`fingerprint_pool_size`保留给调用方自定义的多记录Profile。当前所有内置版本各一条火种；所有Profile都缓存Client，并按Origin和完整代理身份隔离可复用路由。Session关闭和`set_proxy()`会释放全部共享状态。

Chrome和Edge与`curl_cffi`一致：同一Client可复用已有TCP、CONNECT、TLS和HTTP/2连接，复用连接的请求不会重新发送ClientHello；连接池确实新建TLS连接时，BoringSSL随机排列允许变化的扩展并产生新的原始JA3。业务Session、Cookie Jar、DNS配置、TLS Session Cache和Rust连接许可持续复用。

`firefox151`也只保留一条火种，不启用BoringSSL扩展乱序。五次独立完整握手的JA3、JA4、ClientHello长度、扩展顺序和HTTP/2参数全部一致；Firefox复用Client与连接，必须新建连接时允许TLS票据恢复自然增加PSK扩展41，因此最多出现固定完整握手JA3和固定恢复握手JA3两种结构。

`max_response_bytes`默认64MiB，限制普通和multipart响应解压后的Body；所有压缩编码都必须在解压后计数。超限返回明确RuntimeError并关闭该响应，避免压缩炸弹或高并发大Body耗尽进程内存。流式响应由调用方分块消费，不受完整Body上限约束。

`max_websocket_message_bytes`默认16MiB，同时传给WebSocket协议层的消息和帧限制。接收事件队列固定256条；队列满或消息超限时actor立即退出、释放Rust permit，并在调用方后续recv时返回明确错误。

`cookie_store=False`关闭响应`Set-Cookie`自动写入共享Jar，适合多源并发匿名爬虫；业务需要的Cookie仍可按请求显式传入。异步stream必须使用`aiter_content/aread/atext/ajson/aclose`，同步`content/text/json/close`会明确拒绝，避免阻塞Python事件循环。
项目对wreq使用`default-features=false`。根证书由本库显式注入`CertStore`，自定义DNS由本库Hickory Resolver注入，因此不启用wreq重复的`webpki-roots`和`hickory-dns`默认分支；Cookie、stream、multipart、gzip/Brotli/zstd/deflate、SOCKS5、WebSocket和Tokio均有公开API实际使用，不能裁剪。

极端调试结果：8MiB gzip解压体在1MiB上限下被中止，stream模式完整分块读取；`max_connections=1`被长流占用时1000个不同DNS/Origin等待任务RSS只增加约17MB、线程增加0，Session关闭后全部快速失败；WebSocket突发300帧返回明确256条队列溢出错误；默认代理A/B高频切换期间500请求全部成功，身份和Proxy来自同一原子快照；IPv6计量代理首行确认为`CONNECT [::1]:443`。

第二轮极端协议覆盖：响应Body恰好上限成功、上限+1字节失败；Body提前EOF、非法chunk、截断gzip和重定向环均稳定失败，随后同Session正常sentinel请求成功；非法Header名称/值、NaN/Infinity超时、超大Semaphore参数均在边界拒绝；1000等待任务取消后无后台Client构建；DNS UDP截断后TCP回退、9项配置对8项LRU淘汰、100并发同DNS配置去重和等价DNS地址归一化通过；WebSocket非法UTF-8、服务端掩码帧、2MiB消息对1MiB限制和无消费者300帧溢出均明确失败并释放permit。

长期稳定性和性能数据只用于同一构建、同一机器、同一后端及同一并发配置下的版本回归。不得把本地吞吐、线程、句柄、RSS或TIME_WAIT绝对值写成跨环境发布承诺。

Session关闭语义：关闭Semaphore并清除Client、DNS和Origin缓存，所有等待者快速失败，之后拒绝新请求；已经返回给调用方的stream和WebSocket保持对象所有权，由调用方自身close/aclose，不会被其他任务关闭Session时强制截断。并发调用close 100次已验证幂等。

Chrome142源记录原有21条，其中14条首次握手记录仅扩展顺序不同，7条为带PSK扩展41的恢复握手。稳定TLS/HTTP2字段和扩展集合完全一致，现已选择一条首次握手火种；恢复握手由共享TLS Session Cache自然产生。

Chrome/Edge连接复用避免了每请求重复CONNECT、TLS和HTTP/2握手；自然新建连接时仍保留扩展乱序指纹。性能对比必须区分HTTP请求数与实际TLS握手数。

`requests_rust`与`curl_cffi`的性能比较只允许使用同一构建、后端、代理、并发和连接复用条件；环境变化后的历史请求/秒和倍数结论不再作为维护依据。

`resolve`提供Session级静态域名到IPv4/IPv6覆盖；连接IP改变但URL、Host、TLS SNI和证书域名保持不变。`dns_servers`接入Rust Hickory resolver，支持IP或IP:端口、A/AAAA、TTL缓存、UDP和TCP回退；`dns_timeout`控制单次查询。未指定时继续使用系统getaddrinfo。

日常DNS配置只推荐`AsyncSession(dns_servers=[...])`。普通请求也支持`await session.get(url, dns_servers=[...], dns_timeout=...)`请求级覆盖；相同配置使用每指纹变体8项LRU Rust Client缓存，Session关闭或默认代理切换时清空。`resolve`只用于HTTPS/WSS固定连接IP但保持原域名SNI、证书和Host的高级场景，不要把它写成常规DNS服务器入口。

DNS性能测试必须固定服务、任务模型和连接状态，并同时记录成功率与资源峰值。千级冷连接一次起跑主要测量Socket、accept和临时端口容量，不能把连接拒绝归因于DNS覆盖本身。

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
| `http_version` | `auto`、`http1`、`http2`或`http3`；Session默认和单请求覆盖均支持 |
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

## HTTP/3

`http_version="http3"`或`"h3"`进入Reqwest/Quinn/Rustls后端，只发送UDP/QUIC HTTP/3，不执行`Alt-Svc`探测或HTTP/2回退。普通请求、重定向、Cookie、Body上限和同步/异步流继续使用现有公开对象与统一Rust Semaphore。

HTTP/3只支持直连`https://`。当前HTTP CONNECT、SOCKS5、multipart、WebSocket和`transfer_stats`都是TCP语义，与HTTP/3组合时必须明确报错。代理失败时仍只轮换代理session ID并复用业务Session；不得为了绕过该限制在库内重建Session。

HTTP/3后端不读取wreq的BoringSSL TLS配置。Profile默认Header仍生效，`fingerprint_id`仍表示所选记录，但HTTP/3的Rustls ClientHello、QUIC Transport Parameters、连接ID及QPACK状态不等同于真实Chrome/Edge，不能宣称浏览器QUIC指纹对撞。

## 本地HTTP/3出口网关

`http3_local_gateway.py`提供回环HTTP/1.1代理入口。CONNECT成功后由本地CA终止客户端TLS，解析出的GET/POST等请求通过每条入站连接独有的长期HTTP/3 Session直连目标；连续请求复用同一QUIC连接。网关返回`X-Local-HTTP3-Gateway: 1`和`X-Upstream-HTTP-Version: HTTP/3`用于核验。

该工具是显式MITM协议转换器，不是MASQUE或透明UDP代理。必须保护`.http3_gateway_ca`中的CA私钥；默认禁止非回环监听。当前完整缓冲Body，仅支持入站HTTP/1.1和出站HTTPS，不支持WebSocket、HTTP/2入站、代理认证、流式上传或匿名出口。请求失败返回502，不因失败重建出站Session。

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

代理服务器自身的域名始终可由`resolve`或`dns_servers`控制。`socks5://`最终目标也使用本机指定DNS；`socks5h://`和HTTP代理最终目标按协议由代理解析。HTTP CONNECT目标本机解析与原始SNI/证书/Host分离尚未公开，禁止通过IP URL或`verify=False`伪实现。

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

`fingerprint_rotation`只控制多记录Profile的变体选择。当前内置版本各一条火种，因此该开关不改变火种ID；所有Profile都会复用匹配路由的现有连接。Chrome/Edge只在自然新建TLS连接时随机扩展顺序，Firefox完整握手顺序固定并允许PSK恢复自然增加扩展41。

业务代码禁止添加`Connection: close`或`Connection: keep-alive`。连接池按协议和服务端生命周期自然复用或新建连接，HTTP/2不接受这类hop-by-hop Header。

当该 Python 发包对象调用 `set_proxy()` 将代理会话从 A 切换到 B 时，会清空旧代理的连接链路，并在同一 Profile 中重新随机选择一个变体；重复设置同一个代理会话不会改变当前指纹。依赖服务端既有 TLS ticket 的 PSK 恢复握手记录不会作为新代理会话的首个随机指纹。

当前内置`chrome146`、`chrome150`、`edge152`、`firefox151`均只有一条火种。所有Profile缓存单一Client；Chrome/Edge的新TLS连接随机扩展顺序，Firefox完整握手JA3固定且票据恢复仅因扩展41形成固定恢复JA3。同一业务Session共享Cookie、代理配置、DNS、TLS Session Cache和连接许可。

## GIL模型

需要GIL的阶段：解析Python参数、Python JSON编码、创建Python awaitable、把Rust结果转换为Python对象、执行用户Python代码。

不持有GIL的阶段：DNS、TCP、UDP/QUIC、代理握手、TLS、HTTP、连接池、重定向、Body读写、multipart文件读取、Cookie更新、超时和指纹轮换。

同步请求等待期间通过PyO3释放GIL。异步请求不使用Python线程池。冷Client构建仍在Tokio `spawn_blocking`中执行，进程级Tokio Runtime将该阻塞池上限固定为32；Python Future结果由一条进程级专用线程取得GIL并用每次调用捕获的事件循环执行`call_soon_threadsafe`，不再与冷Client构建争抢阻塞池。用户回调仍由对应Python事件循环执行，不在完成线程中执行。

## 关闭语义

`Session.close()`和 `await AsyncSession.close()`禁止新请求并释放缓存的wreq与Reqwest Client、TCP和QUIC连接。关闭前已经接受的请求持有独立Client引用，可以继续完成。关闭后请求、代理修改和Cookie操作统一失败。

## 错误边界

- `timeout`、`connect_timeout`、`read_timeout`和非空`happy_eyeballs_timeout`必须是有限正数；NaN、Infinity、0和负数会被拒绝，不会触发Rust panic。
- 无效方法、Header、代理、URL、重定向、连接、TLS、QUIC和Body错误转换为Python异常；HTTP/3不允许代理、明文URL、multipart、WebSocket或TCP传输计量。
- `raise_for_status()`在状态码不属于200至399时抛出 `RuntimeError`。
- `Response.ok`定义为 `200 <= status_code < 400`。

## 已验证范围

- Chrome 150真实浏览器与requests_rust：41/41，100%。
- Firefox 151五次独立完整握手的核心TLS/HTTP2字段全部一致，已收敛为一条火种；同路由50请求只出现固定完整握手JA3及增加PSK扩展41的恢复JA3，`fingerprint_id`始终唯一。
- Chrome/Edge在本地keep-alive代理上连续请求只建立1条物理连接；指纹捕获器主动关闭连接后50次自然重连得到50个不同JA3，强制20条新TLS连接时JA3N、JA4和`fingerprint_id`保持唯一。Firefox保持固定完整握手与PSK恢复结构。
- 本地真实UDP/QUIC服务验证HTTP/3同步/异步GET与POST、同连接并发、静态DNS覆盖、重定向、Set-Cookie、Body上限、普通/流式Body、版本字段、取消和拒绝边界；公网证书验证直连`cloudflare-quic.com`返回`HTTP/3`响应。
- HTTP/2 fork：431 passed、0 failed、1 ignored；文档测试40 passed、0 failed、1 ignored。
- Python API覆盖动态代理、Cookie、重复Header、重定向、超时、同步/异步流、取消、并发读拒绝、multipart、Session关闭竞态和JSON兼容。
- WebSocket覆盖同步/异步、WS/WSS、HTTP代理、WSS CONNECT预认证、SOCKS5、服务端Close、控制帧边界、50条不同代理身份和25 WS + 15 HTTP + 10 SOCKS5混合上限。
- RFC 8441本地端到端测试验证CONNECT、`:protocol=websocket`和双向帧；Webshare真实外网代理验证HTTPS出口与WSS文本回显。
- IPv6在`[::1]`完成同步和异步HTTP直连；双栈域名由wreq默认RFC 6555 Happy Eyeballs连接器处理。

## 已知边界

- 原生wheel必须与操作系统和CPU架构匹配；Windows ARM64、Linux x64、Linux ARM64和macOS Apple Silicon已通过原生安装冒烟，当前完整协议回归以Windows x64为准。
- 当前manylinux目标依赖glibc，不代表Alpine musl支持。
- HTTP/3使用Reqwest/Quinn/Rustls直连，不支持当前TCP代理、multipart、WebSocket和`transfer_stats`，也不复现wreq/BoringSSL浏览器TLS或QUIC指纹。
- 不支持跨指纹连接池复用，这是保证指纹真实性的必要限制。
- TCP字段来自浏览器和requests_rust共享的Windows内核网络栈，不代表能在其他系统伪造Windows TCP SYN。
- 普通响应Body最终需要复制为Python `bytes`；Rust内部直接保留wreq返回的 `Bytes`，已去掉中间 `Bytes -> Vec<u8>`完整拷贝。大文件仍应使用流式接口降低峰值内存。
