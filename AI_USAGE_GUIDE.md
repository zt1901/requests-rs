# requests_rust 0.2.3 AI使用手册

## 定位

`requests_rust`是Windows x64上的Python浏览器指纹HTTP客户端。Python负责逐请求参数描述和结果消费；Rust负责DNS、代理、BoringSSL TLS、HTTP/1.1、HTTP/2、连接池、Cookie、超时、流、multipart和并发调度。异步网络路径直接使用进程级Tokio Runtime，不经过 `asyncio.to_thread`。

安装文件：`requests_rust-0.2.3-cp310-abi3-win_amd64.whl`。支持64位CPython 3.10及以上普通GIL版本。wheel静态链接BoringSSL和MSVC运行库，不需要Rust、CMake、Visual Studio、Playwright、curl_cffi或额外VC++运行库。

## 导入

```python
from requests_rust import (
    AsyncSession,
    Cookies,
    Headers,
    Response,
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
```

内置Profile：`chrome142`、`chrome146`、`chrome150`、`firefox151`。

```python
print(available_profiles())
```

## Session

```python
Session(
    *,
    impersonate: str,
    fingerprint_rotation: bool = False,
    headers=None,
    proxy: str | None = None,
    verify: bool = True,
    timeout: float = 30,
    connect_timeout: float | None = None,
    read_timeout: float | None = None,
    fingerprints_path: str | None = None,
    auto_profile_headers: bool = False,
)
```

`fingerprints_path`传入指纹JSON文件路径时，该实例在构造时独立读取并解析文件（提前单实例加载），指纹只属于这个实例，不进入进程级全局缓存；多个实例可同时各读各的文件，互不干扰。文件格式与内置 `fingerprints.json` 一致（`id`、`profile`、`tls`、`http` 字段的指纹记录列表）。不传时回退到编译进wheel的内置指纹。文件不存在或解析失败在构造时直接抛错；实例选择的版本在该文件中不存在时，错误信息会列出该文件里的可用版本。

`auto_profile_headers=False`是默认行为，调用方传入的 `User-Agent`、`sec-ch-ua`、`sec-ch-ua-mobile`、`sec-ch-ua-platform` 可覆盖 profile 默认 Header。设为 `True` 时，这些调用方覆盖会被忽略，始终使用当前 profile JSON 采集的默认 Header，避免把不同浏览器版本的 TLS 与 UA/Client Hints 混搭。其他请求 Header 不受影响。

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
        fingerprint_rotation=True,
    ) as session:
        first, second = await asyncio.gather(
            session.get("https://example.com/1"),
            session.post("https://example.com/2", json={"id": 2}),
        )
        print(first.status_code, second.status_code)


asyncio.run(main())
```

取消Python awaitable会丢弃对应Rust Future。已经发出的网络数据不能撤回，但不会占用Python工作线程。

## 独立任务与重试

真实爬虫应复用一个 `AsyncSession`，由固定数量 Worker 持续取得单条任务并独立发包。每一条请求都可带自己的 Header、Cookie、代理和超时；成功任务立即确认，失败任务只重试自身，不会因同批其他任务成功或失败而被阻塞。

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

本地代理回显实测，400条请求的Header、Cookie和代理用户名全部逐条匹配：固定指纹中位约 `646请求/秒`，21条指纹轮换中位约 `640请求/秒`，线程约23至32，RSS增量约1至26MB。每条请求使用不同代理认证session ID，因此吞吐主要受独立代理连接影响。

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

项目性能、资源和稳定性脚本均采用这种普通请求Worker模式。自启动本地keep-alive服务实测：400条动态普通请求在并发10至100时约 `5.8k至9.7k请求/秒`；连续2万条、100 Worker、21条指纹轮换时约 `10.4k至12.2k请求/秒`，线程稳定49，RSS增量最终约31.6MB。真实代理场景仍主要受代理连接延迟限制。

## 指纹轮换与隔离

`fingerprint_rotation=False`固定使用Profile首个变体并复用Client、连接池和TLS Session Cache。

`fingerprint_rotation=True`按原子计数器循环变体。每个变体独立拥有Client、连接池和TLS Session Cache，不同指纹绝不共享H2/TLS连接；同一Session共享Cookie Jar。多个Session不共享Cookie、代理、连接池或TLS Session Cache。

## GIL模型

需要GIL的阶段：解析Python参数、Python JSON编码、创建Python awaitable、把Rust结果转换为Python对象、执行用户Python代码。

不持有GIL的阶段：DNS、TCP、代理握手、TLS、HTTP、连接池、重定向、Body读写、multipart文件读取、Cookie更新、超时和指纹轮换。

同步请求等待期间通过PyO3释放GIL。异步请求不使用Python线程池。冷Client构建在Tokio `spawn_blocking`中执行，不阻塞Python事件循环。进程级Tokio Runtime将blocking线程上限固定为32，防止大量异步结果转换扩张到数百个线程。

## 关闭语义

`Session.close()`和 `await AsyncSession.close()`禁止新请求并释放缓存的空闲Client。关闭前已经接受的请求持有独立Client引用，可以继续完成。关闭后请求、代理修改和Cookie操作统一失败。

## 错误边界

- `timeout`、`connect_timeout`、`read_timeout`必须是有限正数；NaN、Infinity、0和负数会被拒绝，不会触发Rust panic。
- 无效方法、Header、代理、URL、重定向、连接、TLS和Body错误转换为Python异常。
- `raise_for_status()`在状态码不属于200至399时抛出 `RuntimeError`。
- `Response.ok`定义为 `200 <= status_code < 400`。

## 已验证范围

- Chrome 150真实浏览器与requests_rust：41/41，100%。
- Firefox 151真实浏览器与requests_rust：5个变体均41/41，100%。
- HTTP/2 fork：431 passed、0 failed、1 ignored；文档测试40 passed、0 failed、1 ignored。
- Python API覆盖动态代理、Cookie、重复Header、重定向、超时、同步/异步流、取消、并发读拒绝、multipart、Session关闭竞态和JSON兼容。

## 已知边界

- 当前wheel仅支持Windows x64；其他系统需各自构建wheel。
- 当前不实现HTTP/3发送。
- 不支持跨指纹连接池复用，这是保证指纹真实性的必要限制。
- TCP字段来自浏览器和requests_rust共享的Windows内核网络栈，不代表能在其他系统伪造Windows TCP SYN。
- 普通响应Body最终需要复制为Python `bytes`；Rust内部直接保留wreq返回的 `Bytes`，已去掉中间 `Bytes -> Vec<u8>`完整拷贝。大文件仍应使用流式接口降低峰值内存。
