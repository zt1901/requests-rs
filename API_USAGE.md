# requests-rs 使用手册

适用于 `requests-rs 0.4.10`。项目提供接近 `requests` 的同步接口和真正异步的 `AsyncSession`，但不是 `requests` 的完整替代品；不支持的参数会明确报错。源码、示例和更新记录见 [GitHub 仓库](https://github.com/zt1901/requests-rs)。

## 安装与导入

```bash
python -m pip install -U requests-rs
```

要求 CPython 3.10+。PyPI 和 [GitHub Releases](https://github.com/zt1901/requests-rs/releases/latest) 提供 Windows x64/ARM64、Linux x64/ARM64 和 macOS Apple Silicon wheel。Linux wheel 要求 glibc 2.34+；Alpine musl 和 macOS Intel 不在当前构建矩阵内。

```python
from requests_rs import requests

print(requests.available_profiles())
```

当前内置 `chrome146`、`chrome150`、`chrome152`、`edge152` 和 `firefox151`。它们是已采集的固定指纹快照，不会自动追随浏览器升级。模块级 `request/get/post/put/patch/delete/head/options` 默认使用 `chrome152`；连续请求请复用 Session，避免每次重新建连接。

## 同步请求

```python
from requests_rs import requests

with requests.Session(impersonate="chrome152", timeout=30) as session:
    response = session.get(
        "https://example.com/",
        params={"page": 1},
        headers={"Accept": "text/html"},
    )
    response.raise_for_status()
    print(response.status_code, response.http_version, response.text)

# 单次调用也可以使用 requests.get("https://example.com/", impersonate="edge152")。
```

发送表单使用 `data={"key": "value"}`，发送 JSON 使用 `json={"key": "value"}`，发送原始正文使用 `data=b"..."`。`data` 和 `json` 不能同时传入。

## 原生异步与并发

异步代码使用 `AsyncSession`，不要在协程中调用同步 `Session`，也不要为每个请求新建会话。`max_connections` 是同一会话共享的活跃网络操作上限，不是严格的 TCP/QUIC 物理连接数上限。

```python
import asyncio
from requests_rs import requests


async def main():
    async with requests.AsyncSession(
        impersonate="chrome152", max_connections=50, timeout=30,
    ) as session:
        responses = await asyncio.gather(
            session.get("https://example.com/?page=1"),
            session.get("https://example.com/?page=2"),
        )
        for response in responses:
            response.raise_for_status()
            print(response.status_code)


asyncio.run(main())
```

HTTP 请求、流式响应和 WebSocket 共用连接许可。提前停止消费流或 WebSocket 时要关闭对象，避免长期占用许可。

## API 入口与调用层次

```text
requests.request(method, url, **kwargs)      # 同步，一次性创建并关闭 Session
requests.get/post/put/patch/delete/head/options(url, **kwargs)
session.request(method, url, **kwargs)       # 同步，复用 Session
session.get/post/put/patch/delete/head/options(url, **kwargs)
await async_session.request(method, url, **kwargs)
await async_session.get/post/put/patch/delete/head/options(url, **kwargs)
```

`method` 传 `"GET"`、`"POST"` 等 HTTP 方法字符串，`url` 必须是带 `http://` 或 `https://` 和主机名的字符串。快捷方法的 `**kwargs` 传给对应的 `request()`；`head()` 当前默认也跟随重定向，若不希望跟随，明确传 `allow_redirects=False`。异步只有 `AsyncSession`，模块级 `requests.get()` 不能 `await`。

模块级 `requests.request()` 将 `impersonate`、`fingerprint_rotation`、`proxy`、`proxies`、`verify`、`connect_timeout`、`read_timeout` 和 `fingerprints_path` 用于创建临时 Session，剩余关键字传给 `Session.request()`。**流和高频请求请用显式 Session**：模块级调用会在返回时关闭临时 Session，不应依赖返回的流继续保持完整 Session 生命周期。仅属于 Session 构造期的参数（如 `max_connections`）不能直接交给模块级函数；`dns_servers` 和 `http_version` 可作为请求级参数传给模块级函数，但不能用它们配置一个可复用的 Session。

### Session 与 AsyncSession 构造参数

全部都是关键字参数：`requests.Session(参数名=值)`；`requests.AsyncSession(...)` 接受相同的 Session 参数，另可使用 `max_clients` 作为 `max_connections` 的别名。下表的 `None` 表示未单独配置，不是禁用请求总超时。

| 参数 | 类型和默认值 | 怎么传、影响什么 |
|---|---|---|
| `impersonate` | `str`、`PathLike` 或捕获记录对象；`"chrome152"` | `"edge152"` 选内置指纹；`Path("profile.json")` 加载单 profile；也可传同一 profile 的记录序列或 `{schema_version, records}` 对象。 |
| `fingerprint_rotation` | `bool`；`True` | `False` 固定自定义多变体 profile 的选择；内置 profile 不随浏览器版本自动升级。 |
| `headers` | 映射或 `(名称, 值)` 序列；`None` | `{"Accept":"application/json"}`；需要重复 Header 或指定顺序用 `[('X-Tag','a'), ('X-Tag','b')]`。`session.headers.update({...})` 可修改默认头。 |
| `proxy` | 代理 URL 或 `None`；`None` | `"http://user:pass@host:8080"`、`"socks5://host:1080"`、`"socks5h://host:1080"`；与 `proxies` 互斥。 |
| `proxies` | 协议到代理 URL 的映射或 `None`；`None` | `{"http": proxy_url, "https": proxy_url}`；与 `proxy` 互斥。 |
| `verify` | `bool`；`True` | `False` 只用于受控自签名测试；请求级值不能与 Session 值不同。 |
| `timeout` | 正有限秒数；`30` | 整次请求的总时限，例如 `timeout=20`；**不接受** `requests` 的 `(连接, 读取)` 元组。 |
| `connect_timeout` | 正有限秒数或 `None`；`None` | `connect_timeout=5` 设置 Session 级连接时限；不能在单次请求覆盖。 |
| `read_timeout` | 正有限秒数或 `None`；`None` | `read_timeout=10` 设置响应读取时限；可由单次请求覆盖。 |
| `fingerprints_path` | 字符串、`PathLike` 或 `None`；`None` | 多 profile JSON 文件路径，例如 `fingerprints_path="profiles.json"`，与直接传记录对象的 `impersonate` 不可同时用。 |
| `max_connections` | 正整数；`50` | 活跃 HTTP/流/WebSocket 共享上限，例如 `max_connections=100`。 |
| `max_clients` | 正整数或 `None`；`None` | `max_connections` 别名；两者非默认值不能设成不同数字。 |
| `happy_eyeballs_timeout` | 正有限秒数或 `None`；`0.3` | IPv4/IPv6 地址族回退延迟；`None` 禁用并行回退。 |
| `resolve` | 域名到 IP 字符串或 IP 序列的映射；`None` | `{"example.com":["203.0.113.10"]}`；连接目标 IP 固定，但 URL/Host/SNI/证书域名仍是原域名。 |
| `dns_servers` | 字符串序列或 `None`；`None` | `dns_servers=["1.1.1.1", "8.8.8.8"]`，不能传单个字符串；未传时使用系统 DNS。 |
| `dns_timeout` | 正有限秒数或 `None`；`5.0` | 自定义 DNS 查询时限，例如 `dns_timeout=3`。 |
| `fingerprint_pool` | `bool`；`True` | 是否缓存多变体指纹对应的客户端。 |
| `fingerprint_pool_size` | 正整数；`100` | 多变体指纹池上限。 |
| `max_cached_origins` | 非负整数；`4` | 不同 Origin 与代理身份的路由缓存上限。 |
| `max_response_bytes` | 正整数；`67108864` | 响应解压后累计 Body 上限（64 MiB），完整和流式读取都会检查；大文件下载要按需增大。 |
| `max_websocket_message_bytes` | 正整数；`16777216` | 单条 WebSocket 消息/帧上限（16 MiB）。 |
| `cookie_store` | `bool`；`True` | 是否自动把 `Set-Cookie` 写进 Session Cookie Jar。 |
| `http_version` | 字符串；`"http2"` | `"http1.1"`、`"http2"`、`"http3"`、`"auto"`；也接受 `h2`/`h3` 等别名，实际协商结果看 `response.http_version`。 |

```python
with requests.Session(
    impersonate="chrome152",
    headers={"Accept": "application/json"},
    timeout=20,
    connect_timeout=5,
    read_timeout=10,
    max_connections=50,
    http_version="http2",
) as session:
    response = session.get("https://example.com/api", params={"page": 1})
```

会话常用方法：`session.set_proxy(proxy_url)` 修改默认代理，`session.close()` 关闭；异步会话用 `async_session.set_proxy(proxy_url)` 和 `await async_session.close()`。`session.headers.update({...})` 修改默认请求头，`session.cookies` 管理跨请求 Cookie。`fingerprint_count`、`fingerprint_pool_count`、`cached_origin_count` 和 `request_dns_client_count` 是只读统计，不作为构造参数。

### HTTP 请求参数（Session.request / AsyncSession.request）

`request(method, url, *, ...)` 中除 `method`、`url` 外均用关键字传入。`get/post/...` 接受相同关键字；`None` 通常表示沿用 Session 值。

| 参数 | 类型和默认值 | 怎么传、限制 |
|---|---|---|
| `params` | 映射或 `None`；`None` | `params={"q":"rust", "page":2, "tag":["a","b"]}`；序列值编码成重复 Query，原 URL 已有 Query 会追加。 |
| `headers` | 映射、二元组序列或 `None`；`None` | `headers={"Authorization":"Bearer ..."}`；同名请求头覆盖 Session 默认头。 |
| `cookies` | `dict`、二元组列表、`Cookies`、标准库 `CookieJar` 或 `None`；`None` | `cookies={"sid":"abc"}` 只用于本次请求；`CookieJar` 按目标 URL 匹配；可读的 Session Jar 见下文。 |
| `data` | `bytes`、`str`、映射、有序二元组序列或 `None`；`None` | `data={"name":"a"}` 自动编码表单；`data=[("tag","a"),("tag","b")]` 保留重复键；`data=b"raw"` 发原始正文。不可和 `json` 同传。 |
| `json` | JSON 可序列化对象或 `None`；`None` | `json={"id":1}` 自动序列化并添加 `application/json`（除非已有 Content-Type）。不可与 `data`/`files` 同传。 |
| `files` | 字段到文件路径/三元组的映射或 `None`；`None` | `files={"file": "report.bin"}` 或 `{"file": ("upload.bin", Path("report.bin"), "application/octet-stream")}`；`data` 可额外传表单映射，不支持打开的文件对象；不能和 `json`/`stream=True`/`transfer_stats=True` 同传。 |
| `timeout` | 正有限秒数或 `None`；`None` | `timeout=15` 覆盖 Session 总时限；不支持 `(5, 15)`。 |
| `read_timeout` | 正有限秒数或 `None`；`None` | `read_timeout=8` 覆盖 Session 的响应读取时限。 |
| `stream` | `bool`；`False` | `stream=True` 返回可逐块消费的响应；要关闭响应，不支持 multipart；用显式 Session。 |
| `proxy` | 代理 URL、显式 `None` 或未传；未传 | `proxy="socks5h://host:1080"` 仅覆盖本次；**显式 `proxy=None` 表示本次直连**；不能与 `proxies` 同传。 |
| `proxies` | 协议映射或 `None`；`None` | `proxies={"http":url, "https":url}` 仅覆盖本次；不能与 `proxy` 同传。 |
| `allow_redirects` | `bool`；`True` | `False` 返回 3xx 而不跟随；HEAD 若要保持 `requests.head()` 的默认习惯请手动设 `False`。 |
| `max_redirects` | 非负整数；`10` | `max_redirects=3` 设置跳转上限，超过会抛 `TooManyRedirects`。 |
| `transfer_stats` | `bool`；`False` | `True` 返回 `response.transfer_stats`；仅完整 HTTPS 响应、直连或 HTTP 代理；不支持流/multipart。 |
| `dns_servers` | 字符串序列或 `None`；`None` | `dns_servers=["9.9.9.9"]` 只覆盖本次；传 `[]` 本次使用系统 DNS，不能传单个字符串。 |
| `dns_timeout` | 正有限秒数或 `None`；`None` | `dns_timeout=3` 仅在本请求**同时传** `dns_servers` 时有效。 |
| `http_version` | 协议偏好字符串或 `None`；`None` | `http_version="http3"` 仅本次覆盖；不保证对端实际使用 H3。 |
| `discard_cookies` | `bool`；`False` | `True` 不把本次响应的 `Set-Cookie` 写入 Session Jar。 |
| `impersonate` | 与 Session 相同的名称/路径或 `None`；`None` | 不能逐请求换指纹；若显式传入，只传与已创建 Session 一致的字符串或路径。 |
| `verify` | 与 Session 相同的布尔值或 `None`；`None` | 不能逐请求改变 Session 证书验证策略。 |

`params` 只接受映射（例如 `dict`），不是 `requests` 风格的二元组列表；需要有序重复键可直接放进 URL。`files` 的三元组是 `(filename, path, content_type)`，第二项是磁盘路径，不是内容字节。若既要文件又要普通字段，用 `data={...}, files={...}`。本库没有 `auth=...` 参数，认证请通过 `headers` 或代理 URL 传入。

### 按请求类型传参

```python
with requests.Session(impersonate="chrome152") as session:
    # 查询参数编码为 ?q=rust&page=2&tag=a&tag=b。
    search = session.get("https://example.com/search", params={"q": "rust", "page": 2, "tag": ["a", "b"]})
    # JSON 会设置 Content-Type: application/json。
    created = session.post("https://example.com/api/items", json={"name": "demo"}, headers={"Authorization": "Bearer token"})
    # form-urlencoded 表单；重复字段用二元组序列。
    form = session.post("https://example.com/form", data=[("tag", "a"), ("tag", "b")])
    # 本次禁用重定向并强制直连，不修改 Session 默认代理。
    direct = session.get("https://example.com/", proxy=None, allow_redirects=False)
```

```python
from pathlib import Path
from requests_rs import requests

with requests.Session(impersonate="chrome152") as session:
    response = session.post(
        "https://example.com/upload",
        data={"title": "报告"},
        files={"document": ("report.bin", Path("report.bin"), "application/octet-stream")},
    )
    response.raise_for_status()
```

### 代理映射、固定 IP 与超时组合

```python
from requests_rs import requests

代理地址 = "http://user:password@proxy.example:8080"
with requests.Session(
    impersonate="chrome152",
    proxies={"http": 代理地址, "https": 代理地址},
    timeout=30,
    connect_timeout=5,
    read_timeout=10,
) as session:
    # 继承默认代理；按需覆盖为另一条代理或显式直连。
    response = session.get("https://example.com/", params={"q": "test"})
    direct = session.get("https://example.com/", proxy=None)
```

```python
from requests_rs import requests

with requests.Session(
    impersonate="chrome152",
    resolve={"example.com": ["203.0.113.10"]},
    http_version="http3",
) as session:
    response = session.get("https://example.com/", timeout=15)
    print(response.http_version)  # 以实际返回值为准，不保证一定是 HTTP/3。
```

`resolve` 示例 IP 属于文档保留地址，不能直接用于生产访问；换成目标域名实际可达的 IP。调用时 URL 必须仍是域名，不能改成 IP 否则会改变证书和 SNI 语义。

## 响应、JSON 与异常

`Response` 提供 `status_code`、`ok`、`url`、`reason`、`headers`、`cookies`、`history`、`content`、`text`、`json()`、`raise_for_status()`、`request`、`fingerprint_id` 和实际协议 `http_version`。`response.headers` 大小写不敏感，`get_list("set-cookie")` 可读取重复响应头。异步流响应请使用 `await response.aread()`、`await response.atext()` 或 `await response.ajson()`，不能同步读取它的 `content/text/json()`。

```python
from requests_rs import requests

try:
    with requests.Session(impersonate="chrome152") as session:
        response = session.get("https://example.com/")
        response.raise_for_status()
        print(response.text)
except requests.Timeout as error:
    print("超时:", error, error.request.url if error.request else None)
except requests.HTTPError as error:
    print("HTTP 状态:", error.response.status_code)
except requests.RequestException as error:
    print("请求失败:", error)
```

`from requests_rs import exceptions` 与 `requests.exceptions` 均可获取异常类。常见类型有 `DNSError`、`ConnectionError`、`ProxyError`、`SSLError`、`Timeout`、`TooManyRedirects`、`ChunkedEncodingError`、`ContentDecodingError`、`ResponseTooLarge`、`HTTPError` 和 `JSONDecodeError`。原生网络错误携带稳定的 `kind` 和 `source_chain`，公开文本包含底层原因；`error.request` 记录请求方法与 URL。`ConnectTimeout`、`ReadTimeout` 是可捕获的类型，但并非每条超时路径都能细分，通用 `Timeout` 仍应作为兜底。

| 对象 | 读取与传参方式 |
|---|---|
| `response.status_code`, `ok`, `reason`, `url` | 整数状态、`200 <= status < 400`、标准状态短语和最终 URL。 |
| `response.headers` | 大小写不敏感的映射：`response.headers["Content-Type"]`、`.get("content-type")`、`.get_list("set-cookie")`、`.raw` 或 `.items(multi=True)` 读取重复原始头。 |
| `response.content`, `text`, `json()` | `bytes`、按 Content-Type charset 解码的文本、解析 JSON；JSON 解析失败抛 `requests.JSONDecodeError`。异步**流**响应改用 `await aread()/atext()/ajson()`。 |
| `response.cookies`, `history` | 当前响应的 `Set-Cookie` 快照（`.get_dict()`、`.raw`）和重定向响应列表；不是 Session Cookie Jar。 |
| `response.request` | 请求快照：`.method`、`.url`、`.headers`、`.body`；不是可修改后重新发送的 `requests.PreparedRequest`。 |
| `response.fingerprint_id`, `impersonate`, `http_version` | 本次指纹记录 ID、profile 名和实际协议。 |
| `response.transfer_stats` | 默认 `None`；`transfer_stats=True` 时有 `.upload_size`、`.download_size`、`.response_size`、`.scope`。 |
| `response.raise_for_status()` | 4xx/5xx 抛 `HTTPError`，异常含 `.request/.response`；不调用则非 2xx 状态也能正常返回。 |
| `response.iter_content(chunk_size=65536)` | 同步流的字节迭代器；异步流用 `async for chunk in response.aiter_content(chunk_size=65536)`，块大小必须大于 0。 |
| `response.close()/aclose()` | 结束流、释放许可；异步流使用 `await response.aclose()`。同步响应支持 `with`，异步响应支持 `async with`。 |

原生请求异常（例如 `DNSError`）含 `.kind`、`.source_chain`、`.request`；`HTTPError` 还含 `.response`。本库不实现 `requests.Response.raw`、`iter_lines()`、`elapsed`、`encoding` 可写属性，也不实现 `Session.prepare_request()/send()/mount()`；不要从同名风格推断这些 API 已兼容。

## 代理、Cookie 与 DNS

```python
from requests_rs import requests

with requests.Session(
    impersonate="chrome152",
    proxy="http://user:password@proxy.example:8080",
) as session:
    response = session.get("https://example.com/")
    direct = session.get("https://example.com/", proxy=None)
    session.cookies.set("token", "value", url="https://example.com/")
    print(session.cookies.get_dict())
```

代理支持 `http://`、`socks5://`、`socks5h://`；`socks5h://` 由代理解析目标域名。按协议映射时使用 `proxies={"http": url, "https": url}`。代理身份变化时更新请求级 `proxy=`，保留同一个业务 Session 和 Cookie；不要为每次代理 session ID 变化创建新会话。`set_proxy()` 修改 Session 默认代理，不能在多个任务中并发用它切换不同代理。

`session.cookies` 是跨请求的 Cookie Jar。`set(name, value, *, url=None, domain=None, path="/", secure=False, http_only=False)` 必须给 `url` 或 `domain`，例如 `session.cookies.set("sid", "abc", url="https://example.com/", path="/", secure=True, http_only=True)`。`get_dict()` 返回名称到值的快照，`get_all()` 返回每个 Cookie 的 `name/value/domain/path/secure/http_only`；`delete("sid", url="https://example.com/")` 删除匹配项，`clear()` 清空。`session.cookies.update(response.cookies)` 只支持当前响应的 `ResponseCookies` 快照，不能传普通字典（用 `set()`）。Cookie 名相同但域或路径不同，用 `session.cookies["sid"]` 可能报歧义错误。

以下异步片段均需放在 `async def main()` 中执行，示例中的 `requests` 沿用开头的导入。

```python
async with requests.AsyncSession(
    impersonate="chrome152", dns_servers=["1.1.1.1", "8.8.8.8"],
) as session:
    response = await session.get("https://example.com/", dns_servers=["9.9.9.9"])
```

单次请求传 `dns_servers=[]` 可恢复系统 DNS。自定义 `dns_servers` 作用于 TCP HTTP 路径；需要指定 HTTP/3 的目标 IP 时使用 Session 级 `resolve`。HTTP 代理/SOCKS 代理下的目标 DNS 归属因协议而异，不要假设自定义 DNS 能覆盖代理端的解析。

## HTTP/3、流与 WebSocket

`http_version="http3"` 是协议偏好而不是 H3 保证。具备完整 schema 2 模板、直连 HTTPS 时才尝试指纹 H3；代理、multipart、请求级 DNS 等场景使用 H2/H1.1。应检查 `response.http_version` 确认实际协议。H1/H2 `stream=True` 逐块读取；H3 当前先完成网络接收，再提供统一的消费接口。

```python
async with requests.AsyncSession(impersonate="chrome152") as session:
    response = await session.get("https://example.com/file", stream=True)
    try:
        async for chunk in response.aiter_content(64 * 1024):
            print(len(chunk))
    finally:
        await response.aclose()
```

同步版可使用 `with session.get(url, stream=True) as response:` 和 `response.iter_content(64 * 1024)`。同一个流同时只允许一个消费者。

### WebSocket 参数与消息

`session.websocket(url, *, headers=None, cookies=None, protocols=None, version="http1", timeout=None, proxy=未传, proxies=None)`；异步对应 `await async_session.websocket(...)`。这是 Session 方法，**不是**模块级 `requests.websocket()`。

| 参数 | 类型和默认值 | 传法 |
|---|---|---|
| `url` | `str`，必填 | `"wss://example.com/socket"` 或 `"ws://example.com/socket"`。 |
| `headers` | 映射、二元组序列或 `None`；`None` | `headers={"Origin":"https://example.com"}`。 |
| `cookies` | 与 HTTP 请求的 `cookies` 相同；`None` | `cookies={"sid":"abc"}`。 |
| `protocols` | 字符串序列或 `None`；`None` | `protocols=["chat.v1", "chat.v2"]`，不能传单个字符串。 |
| `version` | 字符串；`"http1"` | `"http1"` 使用 Upgrade；`"http2"` 使用 Extended CONNECT，要求服务器声明支持。 |
| `timeout` | 正有限秒数或 `None`；`None` | `timeout=15`，未传沿用 Session。 |
| `proxy`, `proxies` | 与 HTTP 请求同形；均未传 | 单独传 `proxy="http://host:8080"` 或 `proxies={...}`，不可同时传；显式 `proxy=None` 本次直连。 |

```python
async with requests.AsyncSession(impersonate="chrome152") as session:
    async with await session.websocket(
        "wss://example.com/socket", protocols=["chat.v1"], timeout=15,
    ) as ws:
        await ws.send("hello")
        message = await ws.recv()
        if message is not None:
            print(message.type, message.data)
```

同步版使用 `with session.websocket(url) as ws:`、`ws.send()`、`ws.recv()`。WebSocket 支持 HTTP/1.1 Upgrade 和目标服务器支持时的 `version="http2"` Extended CONNECT。单条消息上限由 `max_websocket_message_bytes` 控制，默认 16 MiB；会话关闭前应先关闭活跃 WebSocket。

`ws.send("text")` 或 `ws.send(b"binary")` 发送文本/二进制；也可用 `send_text/send_bytes`、`ping/pong`。同步 `message = ws.recv()`，异步 `message = await ws.recv()`；无消息且连接关闭时返回 `None`。消息的 `.type` 为 `text/binary/ping/pong/close`，数据在 `.data`，关闭事件还可查看 `.code/.reason`。结束时同步 `ws.close(code=1000, reason="")`，异步 `await ws.close(...)`；`ws.closed` 可查询连接状态。

## 自定义指纹与计量

```python
from pathlib import Path
from requests_rs import requests

with requests.Session(
    impersonate=Path("edge152.json"), fingerprint_rotation=False,
) as session:
    response = session.get("https://example.com/")
    print(response.fingerprint_id)
```

路径或捕获记录对象快捷形式只能包含一个 profile，可有多个变体；多 profile 文件使用 `impersonate="edge152", fingerprints_path="profiles.json"`。缺失文件、损坏 JSON 或不支持的传输字段会在构造时失败。业务 Cookie、认证和签名不属于指纹，应由每次实际请求提供。

`transfer_stats=True` 仅支持完整 HTTPS 响应、直连或 HTTP 上游代理；不支持 `stream=True`、multipart、SOCKS/HTTPS 上游代理。它使用独占计量连接，`upload_size`、`download_size` 和 `response_size` 统计 TCP payload 而非解压后正文长度，不宜用于高吞吐热路径。

## 常见问题

- **无可安装 wheel**：检查 Python 是否为 CPython 3.10+、平台与架构是否在上方列表内，以及 Linux glibc 是否至少 2.34。
- **URL 或参数不被接受**：参考上方参数表；例如 `timeout` 不接受 `(connect, read)` 元组，文件上传只接受路径。
- **HTTP/3 返回 HTTP/2**：检查 `response.http_version`、schema 2 模板、代理和请求级 DNS 配置；H3 优先并不保证每个目标都支持 H3。
- **并发任务越来越多**：复用一个 `AsyncSession`、设置有界 `max_connections`，并及时 `aclose()` 流和 WebSocket。
- **需要更详细的报错**：检查异常的 `kind`、`source_chain`、`request.url`；HTTP 状态错误还提供 `response`。

更多可运行示例见 [`examples/`](examples/)；协议边界与工程实现见 [`README.md`](README.md) 和 [`AI_USAGE_GUIDE.md`](AI_USAGE_GUIDE.md)。
