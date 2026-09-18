# requests-rs 0.4.4

本版本修复跨平台 API 一致性问题，使 Windows、Linux 和 macOS 下的 Python API 行为保持一致。

## 修复与兼容性改进

- 支持使用 `max_clients` 作为连接数配置别名。
- 新增 `MutableHeaders`，支持保留重复请求头及顺序，并兼容 `headers.update(...)`。
- 新增独立的 `ResponseCookies` 响应 Cookie 快照，并补充 `response.reason`。
- 支持通过 `discard_cookies` 丢弃当前响应写入 Session Cookie Jar，同时保留响应 Cookie 快照。
- 支持请求级传入与 Session 配置相同的 `impersonate` 和 `verify`，并拒绝身份不一致的覆盖。
- 表单数据支持有序二元组序列，保留重复键和输入顺序。
