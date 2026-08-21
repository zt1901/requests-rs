# Facebook Groups requests_rust 替换验证

这是从 `new_crawler/crawler/src/crawler_app/facebook_groups` 迁入的发包替换测试副本，用于验证 Facebook 群组首页与 GraphQL 分页在 `requests_rust` 传输层下的实际表现。

默认验收规模为 30 条帖子。当前 GraphQL 每页最多返回 3 条，因此一次默认运行至少验证 10 次 cursor 分页；后续传输层改动不得只以两页分页作为通过标准。

保留的能力：

- 群组首页协议字段提取：`lsd`、`jazoest`、`__spin_*`、`__hsi`、cursor。
- Facebook GraphQL 表单请求与 cursor 顺序分页。
- 帖子、作者、群组、互动和附件解析。
- 每请求粘性代理、5 次即时重试、GraphQL 限流错误重试。
- 浏览器 Copy as cURL 的 Cookie、页面 Header、GraphQL Header 显式传入。

唯一的运行时替换是：

| 原项目 | 此测试副本 |
|---|---|
| `curl_cffi.requests.AsyncSession` | `requests_rust.AsyncSession` |
| `curl_cffi` 的 `proxies={...}` | `requests_rust` 的请求级 `proxy=` |
| `curl_cffi` 的 `impersonate` | `requests_rust` 的 profile 名称或单 profile 指纹文件路径；兼容旧式 `fingerprints_path`，测试基类保留按 profile 覆盖 UA 的原逻辑 |

原 `groups.py` 的 Relay 变量、页面 Header、GraphQL Header、表单字段、解析路径、重试次数、分页顺序和每 50 个群组的批处理逻辑均保留。右键运行 `run_facebook_groups_live.py`；顶部参数区用于填写群组 URL、代理和指纹文件路径。

测试入口强制使用代理：未传 `--proxy` 时回退到原项目同一份 IPIPGO 默认代理，并为每条请求生成随机 sticky session。启动时只打印代理主机，不打印认证信息。

Cookie、`x-fb-lsd`、`jazoest`、`__spin_*` 等仍由原 Facebook 业务流程产生，不属于 `fingerprints.json`；不要写入指纹库。

Cookie 对照只发生在本测试运行时：`--cookie-mode home` 将主页 `Set-Cookie` 原样用于 GraphQL 分页；`--cookie-mode fake` 使用相同 Cookie 名称和值长度的伪值。两种模式都不会打印真实 Cookie，也不会写入 `fingerprints.json`。
