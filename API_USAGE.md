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

# 单次调用也可以使用 requests.get(url, impersonate="edge152")。
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

## 常用参数

`Session` 和 `AsyncSession` 均支持以下初始化参数；`AsyncSession` 也接受 `max_clients` 作为 `max_connections` 的别名。

| 参数 | 用途 |
|---|---|
| `impersonate` | 内置 profile、单 profile JSON 路径或捕获记录对象；默认 `chrome152`。 |
| `headers` | 默认请求头；二元组序列可保留重复项及顺序。 |
| `timeout` | 请求总超时秒数，默认 30；只接受有限正数，不接受 `requests` 的 `(connect, read)` 元组。 |
| `connect_timeout`, `read_timeout` | 单独指定连接/正文读取超时秒数。 |
| `proxy`, `proxies` | 单个代理 URL 或按协议的代理映射，不能同时传入。 |
| `verify` | HTTPS 证书验证，默认 `True`。 |
| `http_version` | 默认 `"http2"`；可选 `"http1.1"`、`"http3"` 或 `"auto"`。 |
| `max_connections` | 共享活跃网络操作上限，默认 50。 |
| `max_response_bytes` | 完整响应和流式响应累计解压正文上限，默认 64 MiB。 |
| `cookie_store` | 是否自动保存响应 Cookie，默认 `True`。 |
| `dns_servers`, `dns_timeout` | 自定义 DNS 上游及查询超时。 |
| `resolve` | 固定域名连接 IP，保留原始 URL、Host、SNI 和证书域名。 |
| `fingerprint_rotation`, `fingerprints_path` | 指纹变体轮换及自定义多 profile 文件。 |

`session.request(method, url, ...)` 及同名 HTTP 方法常用请求参数：

| 参数 | 用途 |
|---|---|
| `params`, `headers`, `cookies` | 本请求的查询参数、请求头与 Cookie。 |
| `data`, `json`, `files` | 表单/原始正文、JSON 正文、文件路径 multipart 上传。`files` 不能与 `json` 同时使用。 |
| `timeout`, `read_timeout` | 覆盖本请求超时；`timeout` 是数字秒数。 |
| `proxy`, `proxies` | 仅覆盖当前请求；`proxy=None` 表示本次直连。 |
| `allow_redirects`, `max_redirects` | 是否跟随重定向及最大跳转数，默认 `True` 和 10。 |
| `stream` | 分块消费正文；multipart 响应暂不支持。 |
| `http_version` | 仅本请求覆盖协议偏好。 |
| `dns_servers`, `dns_timeout` | 仅本请求覆盖 DNS；请求级 `dns_timeout` 必须与 `dns_servers` 同传。 |
| `discard_cookies` | 不把这次响应 Cookie 写入 Session Cookie Jar。 |
| `transfer_stats` | 统计专用 TCP 计量链路的流量，详见下方限制。 |

`files` 接受文件路径或 `(filename, path, content_type)`；不等同于 `requests` 支持的任意文件对象或字节元组。请求级 `impersonate` 和 `verify` 可以显式传入，但必须与 Session 设置一致。

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

```python
async with requests.AsyncSession(impersonate="chrome152") as session:
    async with await session.websocket("wss://example.com/socket") as ws:
        await ws.send("hello")
        message = await ws.recv()
        if message is not None:
            print(message.type, message.data)
```

同步版使用 `with session.websocket(url) as ws:`、`ws.send()`、`ws.recv()`。WebSocket 支持 HTTP/1.1 Upgrade 和目标服务器支持时的 `version="http2"` Extended CONNECT。单条消息上限由 `max_websocket_message_bytes` 控制，默认 16 MiB；会话关闭前应先关闭活跃 WebSocket。

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
