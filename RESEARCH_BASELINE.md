# 研究基线：浏览器 cURL 请求模板与指纹库边界

## 结论

本项目采用两层模型，边界固定如下：

1. 内置 `fingerprints.json`、`fingerprints_path`，或直接作为 `impersonate` 传入的单profile指纹文件/记录Sequence/MCP Mapping，只提供浏览器传输画像和跨请求稳定声明。
2. 用户在浏览器中人工完成真实操作后，通过 DevTools **Copy as cURL** 得到的请求，转换为 `requests_rust` 调用时，请求模板字段允许按网站和该动作原样固定。
3. 库不猜测、不自动补齐、不覆盖业务请求模板中的上下文、认证和业务字段。
4. 不能固定的 TLS/HTTP 连接运行态始终由 Rust、wreq、BTLS/BoringSSL 与当前 URL 生成。

这里的“可固定”不表示值永远有效，只表示它属于可由调用方在该网站、该业务动作中原样复用的请求模板；过期、轮换或与账号绑定的值仍应由业务流程更新。

## 指纹库范围

| 数据 | 来源 | 是否由指纹库自动配置 | 规则 |
|---|---|---:|---|
| TLS cipher suites、扩展、Supported Groups、Signature Algorithms、KeyShare groups | 浏览器采集 JSON | 是 | 稳定算法固化为profile；GREASE只保存存在性，运行时生成新值；Chromium允许新连接重排扩展。 |
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
from requests_rs import requests

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

with requests.Session(
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

Firefox、Chrome和Edge官网安装器的完整产品版本、降熵后的UA版本和本库Profile是三层不同数据。当前内置`chrome146`、`chrome150`、`chrome152`、`edge152`、`firefox151`均只有一条火种，不是“自动指向官网最新版”的别名；其中`firefox151`是Juggler研发兼容快照，不是Mozilla官网Stable。Google Chrome 152.0.7977.64的Trust Anchor Identifiers扩展`0xca34`已按捕获payload完成线级回放并通过发布门禁。Chrome/Edge的JA3变化来自BoringSSL在每条新TLS连接上的扩展排列和GREASE，检测器过滤GREASE后计算规范JA4；复用连接的HTTP请求不会产生新握手。Firefox完整握手保持固定扩展顺序，票据恢复握手仅自然增加PSK扩展41。不能删除新扩展、复制随机连接或恢复握手记录伪装成受支持Profile。

新增 Profile 必须以对应正式浏览器构建的真实 TLS/HTTP2 采集和回归为依据。不能只根据官网版本列表改名称、替换 UA，或让旧指纹冒充新版本；产品补丁号和 Build ID 属于采集元数据，公共 Profile 名称按已验证的浏览器大版本管理。

HTTP/3必须作为独立传输指纹边界管理。通用Reqwest/Quinn/Rustls不能读取wreq/BoringSSL的Chrome、Edge或Firefox TLS参数，因此已从模板请求路径移除。schema 2记录只有在BoringSSL ClientHello、QUIC Transport Parameters、HTTP/3 SETTINGS、Header顺序与QPACK均可严格解析和应用时才能进入quiche H3；schema 1以及代理等不支持完整H3回放的场景直接使用H2/H1.1。

## 已证伪路线：Rust原生batch与同tick隐式合批

本项目明确不新增`session.batch()`、`submit_many()`、Rust常驻业务Worker队列或其他把一批业务请求整体搬进Rust调度的接口。

证伪依据：

1. 动态住宅代理场景的主要成本是每个不同代理身份对应的DNS、TCP、代理认证、CONNECT和TLS握手。batch不能合并不同session ID的代理连接，也不能减少这些网络往返。
2. 当前每条请求已经是直接进入Rust/Tokio的原生Future，不经过`asyncio.to_thread`；Session级`max_connections`已经由Rust `Semaphore`统一执行。batch只能减少少量Python任务对象，不能改变主要网络瓶颈。
3. batch会引入第二套并发上限、排队、取消、超时、结果顺序、部分失败、重试、Cookie更新和Session关闭语义，与现有单请求Future和统一Rust permit模型重复并冲突。
4. 爬虫需要逐条确认成功、逐条失败重试、逐条切换代理session ID。把业务队列和重试策略放入网络库会混淆业务生命周期，并增加整批滞留和内存占用。
5. WebSocket、流式响应和multipart具有不同生命周期，无法放入一个一致且不误导的batch返回模型；人为限制batch只支持部分请求又会形成第二套API边界。

同tick隐式合批同样不采用。`benchmark_async_bridge.py`使用Firefox热连接、本地独立服务、20,000请求、同tick并发10、三轮独立客户端进程相邻复测：旧通用完成路径中位吞吐为1820.54请求/秒、CPU为757.03微秒/请求；专用Python完成线程为2935.40请求/秒、731.25微秒/请求，吞吐提高61.2%，CPU下降3.4%。两条路径都创建20,000个Python Task和20,000个Future；收益来自Python结果完成不再与冷Client构建争用Tokio阻塞池，不是batch。

连接复用改为与`curl_cffi`一致后，`benchmark_async_resources.py`在Chrome本地keep-alive、5000请求、并发100下由每请求临时Client的68.14请求/秒提高到851.81请求/秒；`benchmark_async_400_dynamic.py`固定模式中位吞吐从587.09提高到612.40请求/秒，轮换模式从586.90提高到601.49请求/秒，峰值线程从50降至30。差异说明短任务中重复Client构建是主要成本；绝对值仍只用于同机同配置回归。隐式合批仍需重新实现单项取消、部分异常、ContextVars、Cookie更新顺序、流和WebSocket生命周期，不具备引入条件。

真实Facebook群组同一sticky代理、同时起跑、连续50页复测：连接复用前`requests_rust`总耗时217.85秒、平均3715.43毫秒、P95 7852.45毫秒；连接复用后为121.77秒、平均2266.59毫秒、P95 2707.57毫秒，50/50成功。同期`curl_cffi`为119.93秒、平均2265.57毫秒、P95 2902.86毫秒；Rust总耗时相差1.54%，平均单页相差0.05%。

固定正确路线：Python使用有限数量的长期Worker逐条调用同一个`AsyncSession`；Rust负责单请求Future、连接复用、代理身份隔离和全Session`max_connections`。正常分页复用sticky代理身份和HTTP/2连接，只有请求失败重试时轮换代理session ID；Chrome/Edge在自然新建TLS连接时保留扩展乱序。WebSocket继续按长连接生命周期管理，不增加batch抽象。

## HTTP/3透明UDP中继性能

`benchmark_http3_proxy.py`在本机H3服务前建立逐客户端UDP NAT映射，只转发QUIC密文，并模拟双向RTT、抖动、独立随机丢包和带宽整形。每组执行5次冷连接、20次1KiB热顺序请求、100次/并发20的1KiB请求，以及20次/并发20的256KiB响应；服务端固定增加5毫秒处理时间，大响应超时60秒。以下为`requests_rust / curl_cffi`，所有最终样本各阶段成功率均为100%。

| 场景 | 冷连接P50 ms | 热顺序P50 ms | 并发吞吐 req/s | 256KiB批量 Mbps |
|---|---:|---:|---:|---:|
| 回环，0ms，无丢包 | 16.1 / 16.2 | 15.7 / 30.8 | 1270.3 / 1223.9 | 129.08 / 82.20 |
| 城域，20ms，无丢包，100Mbps | 78.4 / 78.8 | 47.2 / 54.6 | 372.6 / 288.7 | 17.35 / 18.81 |
| 住宅，80ms，0.2%丢包，50Mbps | 232.8 / 222.6 | 109.8 / 123.8 | 133.9 / 133.5 | 4.42 / 4.77 |
| 稳定移动，150ms，0.2%丢包，20Mbps | 376.4 / 420.4 | 186.2 / 200.9 | 81.9 / 64.2 | 4.84 / 6.89 |
| 高丢包移动，150ms，1%丢包，20Mbps | 390.0 / 394.2 | 192.9 / 195.6 | 78.0 / 51.9 | 1.00 / 0.98 |

该模型支持的预测边界：无0-RTT恢复时，冷连接首请求约为本机15毫秒加`2.4-3.1 × RTT`及服务端处理时间；复用QUIC连接的顺序小请求约为本机15毫秒加`1.1-1.6 × RTT`及服务端处理时间。真实代理还需叠加代理到目标的RTT、出口拥塞、认证和业务响应时间。这是早期通用H3后端的历史实验数据；当前公开H3路径已替换为由schema 2捕获模板驱动的quiche+BoringSSL实现，不能把旧数据直接当作新路径性能结论。

该中继不是MASQUE、CONNECT-UDP或供应商认证代理；丢包是独立随机而非突发，未模拟NAT重绑定、跨流量竞争和运营商队列。Python中继线程与客户端在同一进程，因此CPU列不是纯客户端CPU；逐包调度也使批量Mbps只适合两库相对比较，不能外推为线路带宽。该测试比较两种HTTP/3实现，不等同于HTTP/3对HTTP/2的协议收益测试。
