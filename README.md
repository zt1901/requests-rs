# requests_rust

`requests_rust` 是一个面向浏览器指纹 HTTP、代理和 WebSocket 的 Python 原生扩展。Python 负责请求参数和结果消费，Rust 负责 DNS、TCP、代理、BoringSSL TLS、HTTP/1.1、HTTP/2、WebSocket、连接池和并发调度；公开接口保持接近 `curl_cffi.requests` 的同步和异步调用习惯。

## API 命名兼容

公开 API 优先沿用 `curl_cffi.requests` 的高频命名与调用习惯：`Session`、`AsyncSession`、`Response`、`Headers`、`Cookies`，以及 `request/get/post/put/patch/delete/head/options`。常用请求参数如 `params`、`headers`、`cookies`、`data`、`json`、`files`、`timeout`、`stream`、`proxy`、`proxies`、`allow_redirects` 同样保持对应名称。

这是迁移便利性的兼容目标，而非逐项复刻承诺。`requests_rust` 不兼容或尚未实现 `curl_cffi` 的专有选项时会明确报错，不会静默忽略；指纹数据模型、TLS/HTTP2 实现、代理参数形式与响应内部对象以本库的 Rust 实现为准。

## 优点

- **Rust 网络路径**：DNS、代理、TLS、HTTP/1.1/2、WebSocket、连接池、Cookie、超时、流式 Body 和 multipart 均由 Rust/Tokio 执行。同步请求等待及自定义指纹文件读取、解析不占用 GIL。
- **浏览器基准指纹**：TLS、HTTP/2、HPACK、请求头和优先级数据由真实浏览器采集后固化为 profile。当前内置 `chrome146`、`chrome150`、`edge152`、`firefox151`；这些名称代表已采集并验证的固定快照，不等同于官网当前 Stable，也不会自动随浏览器更新。
- **实例级指纹隔离**：每个 `Session` 可在构造时从不同 JSON 文件独立加载 profile，不修改进程全局指纹数据，也不影响其他实例。
- **长期并发状态**：一个`AsyncSession`持续复用Cookie Jar、DNS配置、Client和匹配路由的连接池；Chrome/Edge只在自然新建TLS连接时随机ClientHello扩展顺序。代理、请求头与请求级Cookie仍可逐条独立传入。
- **Rust 原生统一连接上限**：HTTP、HTTPS、HTTP 代理、SOCKS5、流和 WebSocket 共同使用 Tokio `Semaphore`，Python 不维护连接许可队列。
- **双栈网络**：支持 IPv4、IPv6 字面量和双栈域名；Happy Eyeballs 默认在 300ms 后并行尝试另一个地址族。

## 研究记录

- Chrome 150 真实浏览器与 Rust profile 的采集对照为 41/41 字段一致。
- Firefox 151五次独立完整握手的JA3、JA4、ClientHello长度、扩展顺序和HTTP/2参数全部一致，因此只保留一条火种；完整握手保持固定NSS JA3，命中TLS票据后的恢复握手会自然增加PSK扩展41并形成第二个恢复JA3。
- Edge 152.0.4191.53使用`playwright_rust`操作本机正式版采集单条完整火种；同一连接上的请求复用握手，强制新建TLS连接时Chrome/Edge扩展顺序随机而JA3N、JA4和火种ID保持稳定。
- 本地动态代理回显测试已验证 400 条请求的 Header、Cookie 和代理认证 session ID 逐条隔离；吞吐随机器负载和连接建立时序波动，不作为固定性能承诺。
- 本地 keep-alive、动态代理和长任务脚本继续用于版本间回归；绝对吞吐受硬件、`max_connections`、指纹变体数量和目标服务影响，不作为发布承诺。
- 同一个 `AsyncSession(max_connections=50)` 已验证同时保持 25 条 WebSocket、15 个 HTTP 代理请求和 10 个 SOCKS5 请求；第 51 个操作会在 Rust 等待 permit。
- Webshare 真实外网代理已验证 HTTPS 出口和 WSS 文本帧回显。

## 安装

当前本机产物为 Windows x64、CPython 3.10+ 的稳定 ABI wheel：

```text
requests_rust-0.3.0-cp310-abi3-win_amd64.whl
```

Windows x64 本地开发可直接右键运行 `build_and_install.py`。当前 GitHub Actions 矩阵原生构建并安装冒烟验证 Windows ARM64、Linux x64、Linux ARM64 和 macOS Apple Silicon wheel。云端冒烟确认原生模块可导入、内置 profile 可读取并可构造 Session；HTTP、代理、SOCKS5、WebSocket、IPv6 和 Facebook 完整协议回归仍以 Windows x64 为准。

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
    impersonate=r"D:\fingerprints\firefox151.json",
)
```

`impersonate` 直接传路径时，文件必须只包含一个 profile，但可以包含该 profile 的多个变体；Session 会自动识别 profile。旧写法 `impersonate="firefox151", fingerprints_path=...` 仍保留。文件内容仅属于当前实例，不进入内置指纹全局缓存。

profile 默认 Header 仅在调用方未传同名 Header 时兜底。浏览器 Copy as cURL 或业务代码传入的全部 Header，包括 `User-Agent` 与 `sec-ch-ua*`，均原样优先，库不会接管或替换。

`accept`、`origin`、`referer`、`sec-fetch-*`、`priority`、Cookie 与认证 Header 必须由具体请求上下文传入。

## 核心能力

- 同步与原生 asyncio API：`Session`、`AsyncSession`、`get/post/put/patch/delete`
- HTTP/1.1、HTTP/2、重定向、总超时与 Body 读取超时
- IPv4、IPv6 和 RFC 6555 Happy Eyeballs
- Session 级静态 `resolve` 映射，以及 Session/请求级 Rust Hickory 自定义 DNS 服务器
- 固定 profile 或按请求轮换 profile
- Session Cookie Jar、每请求 Cookie 覆盖、重复 Header 保序
- Session 默认代理和每请求代理覆盖
- `ws://`、`wss://` 同步/异步 WebSocket，支持 HTTP CONNECT 预认证与 SOCKS5 认证
- `http://`、`socks5://`、`socks5h://`代理，按完整代理身份隔离连接池；相同sticky代理身份可复用CONNECT、TLS和HTTP/2链路
- Session 级 Rust 原生 `max_connections`，允许 HTTP、SOCKS5 和 WebSocket 混合占用
- 所有内置浏览器版本各一条火种；多变体指纹池仅用于调用方自定义的多记录Profile
- `max_cached_origins`限制可保留连接的Origin与完整代理身份组合，query、params和path不额外计数
- 同步/异步流式响应与 Rust Tokio multipart 文件流

## 限制

- 当前不发送 HTTP/3。
- 原生 wheel 必须与操作系统和 CPU 架构匹配；当前五个平台目标均已有构建产物，其中四个云端平台通过原生安装冒烟，完整协议回归以 Windows x64 为准。
- Linux 构建目标为 glibc manylinux，不等同于 Alpine musl 支持。
- 不同 profile 不共享 TLS/HTTP/2 连接池，这是指纹隔离的必要限制。
- 不提供Rust原生`batch()`或库内业务Worker队列；该路线不能减少不同代理身份的握手成本，并会重复现有单请求Future与Rust统一连接上限的生命周期语义。批量业务使用Python有限Worker逐条调用同一个`AsyncSession`。

完整安装、同步/异步 API、请求参数、Cookie、代理、WebSocket、流和 multipart 示例见 [API_USAGE.md](API_USAGE.md)。面向维护者的并发用法和边界说明见 [AI_USAGE_GUIDE.md](AI_USAGE_GUIDE.md)。浏览器 Copy as cURL 请求模板与指纹库的固定边界见 [RESEARCH_BASELINE.md](RESEARCH_BASELINE.md)。

## 查看内置版本

```python
from requests_rust import available_profiles

print(available_profiles())
```
