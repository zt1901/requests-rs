# 研究基线：浏览器 cURL 请求模板与指纹库边界

## 结论

本项目采用两层模型，边界固定如下：

1. 内置 `fingerprints.json`、`fingerprints_path` 或直接作为 `impersonate` 传入的单 profile 指纹文件，只提供浏览器传输画像和跨请求稳定声明。
2. 用户在浏览器中人工完成真实操作后，通过 DevTools **Copy as cURL** 得到的请求，转换为 `requests_rust` 调用时，请求模板字段允许按网站和该动作原样固定。
3. 库不猜测、不自动补齐、不覆盖业务请求模板中的上下文、认证和业务字段。
4. 不能固定的 TLS/HTTP 连接运行态始终由 Rust、wreq、BTLS/BoringSSL 与当前 URL 生成。

这里的“可固定”不表示值永远有效，只表示它属于可由调用方在该网站、该业务动作中原样复用的请求模板；过期、轮换或与账号绑定的值仍应由业务流程更新。

## 指纹库范围

| 数据 | 来源 | 是否由指纹库自动配置 | 规则 |
|---|---|---:|---|
| TLS cipher suites、扩展顺序、Supported Groups、Signature Algorithms、KeyShare groups | 浏览器采集 JSON | 是 | 固定为 profile 的传输画像；实际随机字节不写入 JSON。 |
| ALPN、ALPS、HTTP/2 SETTINGS、HPACK、伪 Header 顺序、初始优先级 | 浏览器采集 JSON | 是 | 固定为 profile 的连接画像。 |
| `User-Agent`、`sec-ch-ua*`、`accept-language`、`accept-encoding`、`te` | 浏览器采集 JSON | 仅缺失时兜底 | 都是普通 HTTP Header。cURL/调用方的同名 Header 始终原样优先；仅未传时使用 profile 默认值。 |
| SNI | 当前请求 URL | 是 | 每次 TLS 握手根据当前 URL host 生成；不读取采集 JSON 的历史 `server_name`。 |
| TLS Random、Session ID、KeyShare 公钥、ticket、PSK binder | TLS 运行时 | 是 | 禁止从 JSON 或 cURL 固定；由 BTLS/BoringSSL 与 Session ticket cache 自然生成。 |
| `Host`、`Content-Length`、`Connection` | HTTP 运行时 | 是 | 禁止由 profile 或模板强制固定。 |
| HTTP/2 后续 Stream ID、动态 HPACK table、窗口剩余量 | HTTP/2 连接运行时 | 是 | 禁止从采集 JSON 或 cURL 重放。 |

## 浏览器 Copy as cURL 模板范围

下表字段**允许在转换后的业务代码中原样固定**。它们不由指纹库自动配置，但调用方传入 `headers=`、`cookies=`、`json=`、`data=` 后必须按模板发送，库不作猜测或替换。

| 字段类别 | cURL 常见形式 | 是否允许业务模板写死 | 指纹库是否自动配置 | 备注 |
|---|---|---:|---:|---|
| 方法、URL、Query | `-X POST`、`https://...?...` | 是 | 否 | 属于具体网站动作。 |
| `User-Agent`、`sec-ch-ua*`、`accept-language`、`accept-encoding`、`te` | `-H 'user-agent: ...'` 等 | 是 | 仅缺失时兜底 | cURL 已指定时原样优先；应选择与 TLS profile 相同浏览器版本/平台的值。 |
| `Accept` | `-H 'accept: application/json, text/plain, */*'` | 是 | 否 | 页面、XHR、图像、脚本的值不同，以浏览器复制值为准。 |
| `Origin` | `-H 'origin: https://example.com'` | 是 | 否 | CORS/POST 业务上下文。 |
| `Referer` | `-H 'referer: https://example.com/page'` | 是 | 否 | 若该动作固定来自同一页面，可原样固定。 |
| `Sec-Fetch-*` | `sec-fetch-site/mode/dest/user` | 是 | 否 | 是具体浏览器动作的真实快照；库不推断。 |
| `Upgrade-Insecure-Requests` | `upgrade-insecure-requests: 1` | 是 | 否 | 页面导航复制到就保留；API cURL 没有就不要补。 |
| `Priority` | `priority: u=1, i` | 是 | 否 | 是该请求调度快照，可按动作固定。 |
| 业务 Header | `x-client-version`、`x-requested-with`、GraphQL Header 等 | 是 | 否 | 由网站协议决定。 |
| 请求 Body | `--data-raw`、`--data-binary`、`--form` | 是 | 否 | JSON、表单、GraphQL operation 等按浏览器请求复现。 |
| Cookie | `-b 'sid=...'` 或 `cookie:` | 是，短期 | 否 | 放入 `cookies=` 或请求 Header；不写进指纹库。服务端轮换后由业务流程更新。 |
| Authorization / API Key | `authorization: Bearer ...` | 是，短期 | 否 | 属于认证状态，不写进指纹库。 |
| CSRF / XSRF | `x-csrf-token: ...` | 是，短期 | 否 | 通常与 Cookie、页面或登录步骤关联。 |
| 签名、时间戳、nonce | `x-signature`、`timestamp`、`nonce` | 可暂时固定 | 否 | 若服务端校验时效，必须由业务算法重新计算；网络库不猜测算法。 |

## 禁止混入指纹库的字段

以下字段即使浏览器 cURL 中出现，也只属于一次业务请求或连接运行态，禁止放到 `fingerprints.json` 的 `http.headers` 作为自动默认 Header：

| 字段 | 原因 |
|---|---|
| `accept`、`origin`、`referer` | 依赖具体网站动作和页面来源。 |
| `sec-fetch-site`、`sec-fetch-mode`、`sec-fetch-dest`、`sec-fetch-user` | 依赖导航、XHR、资源加载等具体发起语义。 |
| `upgrade-insecure-requests`、`priority` | 依赖具体请求类型和浏览器调度。 |
| `cookie`、`authorization`、`proxy-authorization` | 依赖账号、代理或当前会话。 |
| `host`、`content-length`、`connection` | 必须由 URL、Body 与协议栈实时决定。 |
| `x-csrf-token`、签名、时间戳、nonce | 依赖当前登录态、业务算法或时效。 |

运行时会从 profile 默认 Header 中排除上述字段；这不会删除调用方通过 `headers=` 显式传入的 cURL 值。

## 标准转换示例

浏览器已人工完成登录和业务操作后，Copy as cURL 的请求可转换为：

```python
from requests_rust import Session

浏览器请求头 = {
    "accept": "application/json, text/plain, */*",
    "origin": "https://example.com",
    "referer": "https://example.com/dashboard",
    "sec-fetch-site": "same-origin",
    "sec-fetch-mode": "cors",
    "sec-fetch-dest": "empty",
    "user-agent": "Mozilla/5.0 (...) Chrome/150.0.0.0 Safari/537.36",
    "sec-ch-ua": '"Not;A=Brand";v="8", "Chromium";v="150", "Google Chrome";v="150"',
    "sec-ch-ua-mobile": "?0",
    "sec-ch-ua-platform": '"Windows"',
    "x-csrf-token": "浏览器当前操作得到的值",
}

with Session(
    impersonate="chrome150",
) as session:
    response = session.post(
        "https://example.com/api/action",
        headers=浏览器请求头,
        cookies={"session": "当前浏览器会话值"},
        json={"id": "123"},
    )
```

若未保存 cURL 的 UA/Client Hints，则不传这些 Header，`chrome150` profile 会仅在缺失时提供自身默认值；库不会覆盖任何调用方显式传入的 Header。

## 实现原则

1. **业务真实快照优先**：人工浏览器操作后复制的 cURL 是该网站、该动作最可靠的应用层请求模板。
2. **传输状态运行时生成**：SNI、TLS 随机值、会话恢复、HTTP/2 stream 与动态压缩表不能从 cURL 或 JSON 固定。
3. **不自动猜业务**：库不根据 URL 猜 `Referer`、`Origin`、`Sec-Fetch-*`、签名、Cookie 或 CSRF。
4. **不把会话态伪装成指纹**：认证、Cookie、CSRF、实验分桶和签名应留在业务模板/业务流程，不能污染可复用 profile。

## 浏览器产品版本与 Profile 命名

Firefox、Chrome和Edge官网安装器的完整产品版本、降熵后的UA版本和本库Profile是三层不同数据。当前内置`chrome146`、`chrome150`、`edge152`、`firefox151`均只有一条火种，不是“自动指向官网最新版”的别名。Chrome/Edge后续JA3变化由BoringSSL每请求新连接扩展排列产生；Firefox完整握手保持固定扩展顺序，票据恢复握手仅自然增加PSK扩展41。不能复制随机连接或恢复握手记录伪装成多个Profile变体。

新增 Profile 必须以对应正式浏览器构建的真实 TLS/HTTP2 采集和回归为依据。不能只根据官网版本列表改名称、替换 UA，或让旧指纹冒充新版本；产品补丁号和 Build ID 属于采集元数据，公共 Profile 名称按已验证的浏览器大版本管理。

## 已证伪路线：Rust原生batch与同tick隐式合批

本项目明确不新增`session.batch()`、`submit_many()`、Rust常驻业务Worker队列或其他把一批业务请求整体搬进Rust调度的接口。

证伪依据：

1. 动态住宅代理场景的主要成本是每个不同代理身份对应的DNS、TCP、代理认证、CONNECT和TLS握手。batch不能合并不同session ID的代理连接，也不能减少这些网络往返。
2. 当前每条请求已经是直接进入Rust/Tokio的原生Future，不经过`asyncio.to_thread`；Session级`max_connections`已经由Rust `Semaphore`统一执行。batch只能减少少量Python任务对象，不能改变主要网络瓶颈。
3. batch会引入第二套并发上限、排队、取消、超时、结果顺序、部分失败、重试、Cookie更新和Session关闭语义，与现有单请求Future和统一Rust permit模型重复并冲突。
4. 爬虫需要逐条确认成功、逐条失败重试、逐条切换代理session ID。把业务队列和重试策略放入网络库会混淆业务生命周期，并增加整批滞留和内存占用。
5. WebSocket、流式响应和multipart具有不同生命周期，无法放入一个一致且不误导的batch返回模型；人为限制batch只支持部分请求又会形成第二套API边界。

同tick隐式合批同样不采用。`benchmark_async_bridge.py`使用Firefox热连接、本地独立服务、20,000请求、同tick并发10、三轮独立客户端进程相邻复测：旧通用完成路径中位吞吐为1820.54请求/秒、CPU为757.03微秒/请求；专用Python完成线程为2935.40请求/秒、731.25微秒/请求，吞吐提高61.2%，CPU下降3.4%。两条路径都创建20,000个Python Task和20,000个Future；收益来自Python结果完成不再与冷Client构建争用Tokio阻塞池，不是batch。

并发100、5000次Chrome本地请求中，专用完成线程吞吐从61.93提高到68.14请求/秒，峰值线程均为51，因为Chrome每请求冷Client构建仍会占满32条blocking线程；Firefox冷并发吞吐从686.42提高到831.73请求/秒。并发400动态代理下，固定指纹中位吞吐从563.74提高到587.09请求/秒，轮换从550.16提高到586.90请求/秒。隐式合批即使把10次PyO3桥接缩成1次，也仍需10个调用方Future分别交付结果，并重新实现单项取消、部分异常、ContextVars、Cookie更新顺序、流和WebSocket生命周期；每次调用还会增加至少一个事件循环tick延迟。专用完成线程已经在不改变单请求语义的前提下收敛桥接开销，不具备再引入batch的性能前提。

固定正确路线：Python使用有限数量的长期Worker逐条调用同一个`AsyncSession`；Rust负责单请求Future、代理身份隔离和全Session`max_connections`。Firefox优先复用sticky代理与HTTP/2连接；Chrome/Edge按每请求新JA3要求故意新建TCP/CONNECT/TLS，不能再把连接复用作为这两个Profile的优化目标。WebSocket仍按长连接生命周期管理，而不是增加batch抽象。
