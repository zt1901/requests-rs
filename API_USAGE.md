# requests_rust 使用与 API 手册

`requests_rust` 是 Python 3.10+ 的 HTTP 客户端。网络请求由 Rust 执行，支持浏览器 TLS/HTTP2 指纹、HTTP/1.1/HTTP/2、Cookie Jar、代理、重定向、流式响应和 multipart 上传。

高频 API 命名接近 `curl_cffi.requests`，但未实现的关键字参数会抛出 `TypeError`，不会被静默忽略。

## 安装

从私有 `requests_rust` 发布仓库的 Release 下载与系统、CPU 架构匹配的 wheel，再安装：

```bash
pip install requests_rust-0.2.3-cp310-abi3-win_amd64.whl
```

wheel 使用 CPython stable ABI，要求 CPython 3.10 或更高版本。

支持的 wheel：

| 平台 | wheel 标签 |
|---|---|
| Windows x64 | `win_amd64` |
| Windows ARM64 | `win_arm64` |
| Linux x64 | `manylinux_2_34_x86_64` |
| Linux ARM64 | `manylinux_2_34_aarch64` |

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

当前内置 profile 为 `chrome142`、`chrome146`、`chrome150` 和 `firefox151`。profile 决定 TLS/HTTP2 指纹行为，不应当用它保存业务 Cookie、认证 Header、CSRF、签名、`Referer`、`Origin`、时间戳或 nonce。

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
| `impersonate` | 必填，内置或自定义指纹文件中的 profile 名称。 |
| `fingerprint_rotation` | 默认为 `False`。关闭时，一个代理会话内固定选择一个可独立复现变体；开启时按请求轮换变体。 |
| `headers` | Session 默认 Header，支持 `dict` 或二元组序列。序列可保留重复 Header 和顺序。 |
| `proxy` | Session 默认代理 URL。 |
| `proxies` | 协议到代理 URL 的映射，不能和 `proxy` 同时传入。 |
| `verify` | 默认为 `True`，验证 HTTPS 证书。仅受控本地自签名测试可设为 `False`。 |
| `timeout` | 请求总超时秒数，默认为 `30`。 |
| `connect_timeout` | 可选连接超时秒数。 |
| `read_timeout` | 可选响应 Body 读取超时秒数。 |
| `fingerprints_path` | 可选自定义指纹 JSON 路径，仅在当前 Session 构造时独立加载。 |

## 原生异步请求

`AsyncSession` 直接等待 Rust 网络 Future，不经过 `asyncio.to_thread`。高并发业务应优先复用一个或多个长期存活的 `AsyncSession`，不要为每个请求重新创建 Session。

```python
import asyncio
from requests_rust import AsyncSession


async def main():
    async with AsyncSession(
        impersonate="firefox151",
        proxy="http://user:password@proxy.example:8080",
    ) as session:
        first, second = await asyncio.gather(
            session.get("https://example.com/api/one"),
            session.post("https://example.com/api/two", json={"id": 2}),
        )
        print(first.status_code, second.status_code)


asyncio.run(main())
```

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

response = get("https://example.com/", impersonate="chrome142")
created = post(
    "https://example.com/api/items",
    impersonate="chrome142",
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

对需保持 IP 的 cursor 分页，推荐：一个代理会话对应一个 `AsyncSession`，`fingerprint_rotation=False`，正常分页不切代理；仅请求失败重试时更换 sticky session。

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

## 自定义指纹文件

```python
session = Session(
    impersonate="firefox151",
    fingerprints_path=r"D:\fingerprints\firefox151.json",
)
```

自定义文件在 Session 构造时读取并解析，仅影响该实例；不同 Session 可以同时使用不同文件。文件记录应只包含 TLS/HTTP2 指纹数据。SNI、TLS Random、KeyShare、ticket、PSK binder、Host、Content-Length、业务 Header、Cookie、认证态和签名参数都是运行态数据，不应固定进指纹文件。

## 重要边界

- 当前不发送 HTTP/3。
- 不同指纹变体使用独立 Client、连接池和 TLS session cache，这是指纹隔离要求。
- 调用方传入的 `User-Agent`、`sec-ch-ua*` 与其他同名 Header 优先，库不会覆盖。
- `Accept`、`Origin`、`Referer`、`Sec-Fetch-*`、Cookie、Authorization、CSRF 和业务签名必须根据当前业务请求传入。
- `verify=False` 只应用于受控测试；生产 HTTPS 请求应保持证书验证。
