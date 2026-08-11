# 研究基线：浏览器 cURL 请求模板与指纹库边界

## 结论

本项目采用两层模型，边界固定如下：

1. `fingerprints.json` / `fingerprints_path` 只提供浏览器传输画像和跨请求稳定声明。
2. 用户在浏览器中人工完成真实操作后，通过 DevTools **Copy as cURL** 得到的请求，转换为 `requests_rust` 调用时，请求模板字段允许按网站和该动作原样固定。
3. 库不猜测、不自动补齐、不覆盖业务请求模板中的上下文、认证和业务字段。
4. 不能固定的 TLS/HTTP 连接运行态始终由 Rust、wreq、BTLS/BoringSSL 与当前 URL 生成。

这里的“可固定”不表示值永远有效，只表示它属于可由调用方在该网站、该业务动作中原样复用的请求模板；过期、轮换或与账号绑定的值仍应由业务流程更新。

## 指纹库范围

| 数据 | 来源 | 是否由指纹库自动配置 | 规则 |
|---|---|---:|---|
| TLS cipher suites、扩展顺序、Supported Groups、Signature Algorithms、KeyShare groups | 浏览器采集 JSON | 是 | 固定为 profile 的传输画像；实际随机字节不写入 JSON。 |
| ALPN、ALPS、HTTP/2 SETTINGS、HPACK、伪 Header 顺序、初始优先级 | 浏览器采集 JSON | 是 | 固定为 profile 的连接画像。 |
| `User-Agent`、`sec-ch-ua*`、`accept-language`、`accept-encoding`、`te` | 浏览器采集 JSON | 是 | 作为稳定默认 Header。`auto_profile_headers=True` 时，UA/Client Hints 强制采用 profile 值。 |
| SNI | 当前请求 URL | 是 | 每次 TLS 握手根据当前 URL host 生成；不读取采集 JSON 的历史 `server_name`。 |
| TLS Random、Session ID、KeyShare 公钥、ticket、PSK binder | TLS 运行时 | 是 | 禁止从 JSON 或 cURL 固定；由 BTLS/BoringSSL 与 Session ticket cache 自然生成。 |
| `Host`、`Content-Length`、`Connection` | HTTP 运行时 | 是 | 禁止由 profile 或模板强制固定。 |
| HTTP/2 后续 Stream ID、动态 HPACK table、窗口剩余量 | HTTP/2 连接运行时 | 是 | 禁止从采集 JSON 或 cURL 重放。 |

## 浏览器 Copy as cURL 模板范围

下表字段**允许在转换后的业务代码中原样固定**。它们不由指纹库自动配置，但调用方传入 `headers=`、`cookies=`、`json=`、`data=` 后必须按模板发送，库不作猜测或替换。

| 字段类别 | cURL 常见形式 | 是否允许业务模板写死 | 指纹库是否自动配置 | 备注 |
|---|---|---:|---:|---|
| 方法、URL、Query | `-X POST`、`https://...?...` | 是 | 否 | 属于具体网站动作。 |
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
    # 复制的cURL自带真实UA/Client Hints时，库不覆盖它们。
    auto_profile_headers=False,
) as session:
    response = session.post(
        "https://example.com/api/action",
        headers=浏览器请求头,
        cookies={"session": "当前浏览器会话值"},
        json={"id": "123"},
    )
```

若只保存业务 Header、未保存或不信任 cURL 的 UA/Client Hints，则改为 `auto_profile_headers=True`，由 `chrome150` profile 自动注入匹配的 UA 与 `sec-ch-ua*`；其他业务 Header 仍按调用方传入值发送。

## 实现原则

1. **业务真实快照优先**：人工浏览器操作后复制的 cURL 是该网站、该动作最可靠的应用层请求模板。
2. **传输状态运行时生成**：SNI、TLS 随机值、会话恢复、HTTP/2 stream 与动态压缩表不能从 cURL 或 JSON 固定。
3. **不自动猜业务**：库不根据 URL 猜 `Referer`、`Origin`、`Sec-Fetch-*`、签名、Cookie 或 CSRF。
4. **不把会话态伪装成指纹**：认证、Cookie、CSRF、实验分桶和签名应留在业务模板/业务流程，不能污染可复用 profile。
