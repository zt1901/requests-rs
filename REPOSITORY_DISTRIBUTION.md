# 开源仓库与发布约定

## 权威仓库

项目的权威源码仓库为：

<https://github.com/zt1901/requests_rust-source>

公开仓库保存完整Rust/PyO3源码、Python API、指纹记录、vendored依赖、测试、构建配置、示例和研究文档。Issue、Pull Request、Git标签和GitHub Release均以该仓库为准。

原`zt1901/requests_rust` wheel分发仓库可以保留为兼容镜像，但不能成为第二个源码真值；README、版本号、Release说明和wheel摘要必须从权威仓库同步生成。

## 开源许可证

项目原创代码采用`MIT OR Apache-2.0`双许可证。使用者可以任选其一：

- [`LICENSE-MIT`](LICENSE-MIT)
- [`LICENSE-APACHE`](LICENSE-APACHE)

vendored第三方组件继续遵循各自许可证和NOTICE。修改或分发vendor内容时不得删除原版权和许可证文件。

## 仓库内容

应提交：

- `src/`与`python/`中的Rust/Python实现。
- `Cargo.toml`、`Cargo.lock`、`pyproject.toml`和构建配置。
- `fingerprints.json`及其可公开的采集元数据。
- 直接保护公开行为的测试与本地测试服务。
- `examples/`、用户手册、维护手册和研究边界。
- `.github/workflows/build-wheels.yml`。
- vendored依赖及其许可证、补丁和上游说明。

不得提交：

- 真实账号、Cookie、Token、API Key、代理密码或可用私钥。
- 与个人账号绑定的请求Body、CSRF、签名、时间戳或业务抓包。
- 本机构建缓存、虚拟环境、`.pyd`、`target/`、临时wheel和测试下载目录。
- 包含真实代理身份或客户数据的基准结果。

公开测试需要凭据时，用户在本地文件顶部填写；仓库中的默认值必须为空或不可用示例值。

## 构建矩阵

| 平台 | Rust target | wheel标签 | 当前验证级别 |
|---|---|---|---|
| Windows x64 | `x86_64-pc-windows-msvc` | `win_amd64` | 完整协议回归 |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `win_arm64` | 原生构建与安装冒烟 |
| Linux x64 | `x86_64-unknown-linux-gnu` | `manylinux_2_34_x86_64` | 原生构建与安装冒烟 |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `manylinux_2_34_aarch64` | 原生构建与安装冒烟 |
| macOS Apple Silicon | `aarch64-apple-darwin` | `macosx_11_0_arm64` | 原生构建与安装冒烟 |

“构建成功”“原生安装冒烟”和“完整协议回归”是三个不同状态。Release说明必须逐平台写明实际达到的级别，不能把导入成功描述成代理、WebSocket、IPv6和指纹均已验证。

## 本机构建

Windows x64开发环境可右键运行：

```text
build_and_install.py
```

该脚本同步指纹、设置本机Cargo/LLVM路径、构建CPython 3.10 stable ABI wheel、安装到当前Python，并同步editable源码目录中的原生模块。

通用构建：

```bash
python -m pip install "maturin>=1.9,<2"
maturin build --release --out dist
```

## 发布流程

1. 更新版本号、README、API手册和变更说明。
2. 运行与改动直接相关的本地回归；浏览器profile变更还需线级对撞。
3. 推送提交并等待GitHub Actions矩阵完成。
4. 收集每个平台实际生成的wheel，记录文件名和SHA-256。
5. 创建`vX.Y.Z`标签；工作流生成draft GitHub Release。
6. 在Release说明中分别列出平台、验证级别、内置profile和已知限制。
7. 核对wheel中不含凭据、个人路径、测试输出或本机构建缓存。
8. 由维护者把draft发布为正式Release。

当前版本wheel命名示例：

```text
requests_rs-0.4.0-cp310-abi3-win_amd64.whl
requests_rs-0.4.0-cp310-abi3-win_arm64.whl
requests_rs-0.4.0-cp310-abi3-manylinux_2_34_x86_64.whl
requests_rs-0.4.0-cp310-abi3-manylinux_2_34_aarch64.whl
requests_rs-0.4.0-cp310-abi3-macosx_11_0_arm64.whl
```

## Profile发布门禁

新增或更新浏览器profile时必须提供：

1. 浏览器正式产品名称、完整版本、来源和二进制摘要。
2. TLS、HTTP/2、Header顺序和生命周期采集记录。
3. 对GREASE、随机KeyShare、ECH载荷和Chromium扩展乱序的正确归一化。
4. `from requests_rs import requests`直接加载捕获JSON的回放结果。
5. 内置profile同步脚本结果和对应回归。

禁止通过修改profile名称、UA或Client Hints让旧记录冒充新浏览器。Juggler、Nightly、Chromium和其他研发构建必须明确标注，不能写成Chrome或Firefox官网Stable。

## 贡献检查

提交Pull Request前至少检查：

```bash
cargo fmt --check
cargo check --lib
python test_python_api.py
```

如果当前平台缺少`libclang`，应设置`LIBCLANG_PATH`后再构建BTLS；Windows本机优先使用`build_and_install.py`。测试范围应随改动风险扩大，不能用与改动无关的全量压力测试代替直接行为验证。
