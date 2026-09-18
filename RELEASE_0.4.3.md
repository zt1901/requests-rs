# requests-rs 0.4.3

0.4.3 是面向现有 Python 爬虫和自动化项目的兼容性版本，公开发布 Windows x64 与 manylinux2014 x64 wheel。

## 主要变化

- 补充常用 `curl_cffi` 迁移接口：`max_clients`、可变 `session.headers.update()`、`response.cookies.get_dict()`、`response.reason` 和请求级 `discard_cookies`。
- 请求级 `impersonate`、`verify` 与 Session 配置一致时允许透传，不一致时明确失败，避免静默切换传输身份。
- 保持 Rust/Tokio 原生异步请求、统一 Session 并发信号量、动态指纹加载、自定义 DNS 和内置 Mozilla/WebPKI 根证书能力。
- Linux x64 wheel 使用 manylinux2014 标签，最低兼容 glibc 2.17。

## 发布文件

- `requests_rs-0.4.3-cp310-abi3-win_amd64.whl`
- `requests_rs-0.4.3-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl`

两个 wheel 均使用 CPython Stable ABI，支持 CPython 3.10 及以上版本。

## 安装

```bash
python -m pip install -U requests-rs==0.4.3
```
