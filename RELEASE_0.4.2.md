# 0.4.2（2026-09-09）

PyPI 已发布 0.4.2；本说明用于对应 GitHub v0.4.2 发布。

- 新捕获输出统一为完整 schema 2 envelope；保留库历史输入兼容，不自动伪造缺失模板。
- 修复 Firefox154 HTTP/3 的 TLS group/signature/extension 支持，按原序注册 zlib/zstd/Brotli 证书解压。
- 严格验证 delegated_credentials、record_size_limit、ECH outer 形状及原始 wire 摘要。
- ECH GREASE 保留 AEAD 和长度，继续生成随机密文/临时密钥。
- QUIC 参数按 RFC 缺省值核验；连接 ID 保持长度但重新生成；version_information 不再改动正式版本。
- HTTP/3 SETTINGS 保留捕获顺序与缺省项，不添加 Firefox 未发送的字段，继续使用本地资源守卫。
- 捕获器另修多 Initial 路由与大响应部分写入，避免完整捕获/回放错误超时。

已验证 Firefox154/Chrome148 本地真实 H3 HTTP200，以及归一化 TLS/QUIC/SETTINGS/header 顺序。
最终双平台构建与全量回归结果见本机 prepare-0.4.2 目录，不以旧版 wheel 代替新源码证据。

## 发布包验证

- Windows x86_64 / Linux x86_64 各 65 项原生测试通过。
- 隔离安装各自 release wheel：Windows 11 个包级回归任务、Linux 9 个任务通过；Linux 另行验证运行取消及独立 aioquic 互操作。
- Chrome152 与 Firefox154 均通过独立 aioquic 动态 QPACK 互操作。
- Windows release 包对真实捕获端重跑 Firefox154 / Chrome148，均为 H3 HTTP 200；稳定 TLS payload、QUIC 参数、SETTINGS 和请求头顺序归一化对照通过。
- 两包 `twine check` 通过，隔离安装的 native 与 wheel 内文件摘要一致；14 个关键源文件与 Linux 构建副本一致。
- 优化：opt-level=3、fat LTO、codegen-units=1、strip；未使用 target-cpu=native。

| 安装包 | 字节数 | SHA256 |
| --- | ---: | --- |
| Windows cp310-abi3 win_amd64 | 5068299 | `8ca89fccd98c56e34aa4d0b0f309b3935eb20836113b92071f5cbb304fb3104d` |
| Linux cp310-abi3 manylinux_2_38_x86_64 | 4796823 | `d4127f852d1d48a16506b9541fb73e9bcaf953dfff82e1df8667c190c2335011` |

原 Ubuntu 构建包要求 glibc >= 2.38；现已补充下述 glibc >= 2.17 包。本轮未构建 macOS/ARM；不把两个已验证浏览器扩展为所有浏览器的兼容保证。PyPI 与 GitHub 使用相同的已验证安装包。


## 2026-09-08：固定 manylinux2014 兼容构建

- Docker glibc 2.17 环境：65 项 native 测试、9 个包级回归任务和运行取消通过，auditwheel 确认 manylinux_2_17。
- 同一 wheel 在独立 Ubuntu 环境通过 Chrome152 / Firefox154 aioquic H3 互操作。
- 文件：`requests_rs-0.4.2-cp310-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl`
- 大小：4448147 bytes；SHA256：`34a25e4dc167530a4c003e916bd1e590804c41395ccd296ad22b1506e868431e`。
- 已补上传 PyPI 0.4.2 并核验官方文件摘要；旧 2.38 wheel 保留。
- 可重用构建配置：`scripts/manylinux2014/`。
