# 仓库职责与发布约定

本项目拆分为两个 GitHub 私有仓库。后续维护、构建和发布必须严格遵守本文件，避免将源码泄露到 wheel 分发仓库。

## 1. 源码与构建仓库

仓库：`zt1901/requests_rust-source`

地址：<https://github.com/zt1901/requests_rust-source>

本地项目目录：

```text
C:\Users\admin\Desktop\实验项目\指纹对撞机\请求捕获器\wreq_replayer
```

本地 Git `origin` 必须始终指向该源码仓库：

```text
https://github.com/zt1901/requests_rust-source.git
```

该仓库存放：

- 完整 Rust/PyO3 源码、Python API 外壳、指纹记录和 vendored 依赖。
- `Cargo.toml`、`Cargo.lock`、`pyproject.toml`、`uv.lock` 与本机构建脚本。
- 所有测试、Facebook 真实回归、性能基准和研究文档。
- `.github/workflows/build-wheels.yml` 自动构建流水线。
- 四个平台 wheel 的构建产物和源码仓库内的构建 Release。

自动构建目标：

| 平台 | Rust target | wheel 标签 | 构建位置 |
|---|---|---|---|
| Windows x64 | `x86_64-pc-windows-msvc` | `win_amd64` | 本机 `build_and_install.py` |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `win_arm64` | GitHub Actions |
| Linux x64 | `x86_64-unknown-linux-gnu` | `manylinux_2_34_x86_64` | GitHub Actions |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `manylinux_2_34_aarch64` | GitHub Actions |

Windows x64 本机构建入口：

```bash
python build_and_install.py
```

每次变更源码、测试、指纹、构建配置或 Actions 工作流时，只能提交并推送至 `requests_rust-source`。

## 2. Wheel 分发仓库

仓库：`zt1901/requests_rust`

地址：<https://github.com/zt1901/requests_rust>

该仓库必须保持私有，并且只用于 Release 分发。

允许内容：

- 简短的 `README.md`，仅说明安装方式、支持平台和源码仓库边界。
- GitHub Release。
- 以下四种已构建的 `.whl` 文件。

禁止内容：

- Rust 源码、Python 源码、Cargo 文件、测试、研究文档、指纹 JSON、vendored 依赖。
- GitHub Actions workflow、构建日志或任何能还原源码的资料。
- 将本地项目目录设置为该仓库的 `origin`。

发布仓库的 `main` 分支只保留上述简短 `README.md`；wheel 仅作为 GitHub Release asset 上传。

## 3. 发布流程

发布新版本时按以下顺序执行：

1. 在 `requests_rust-source` 完成源码、测试与多平台构建验证。
2. 收集四个 wheel，确认文件名和 SHA-256 digest。
3. 在源码仓库保留构建记录和 wheel Release，用于可重复构建与审计。
4. 在 `zt1901/requests_rust` 创建同版本 draft Release，并只上传四个 wheel。
5. 核对分发 Release 中没有源码文件、压缩源码包或 workflow artifact。
6. 由用户决定是否将 draft Release 发布为正式 Release。

对于 `v0.2.3`，wheel 名称为：

```text
requests_rust-0.2.3-cp310-abi3-win_amd64.whl
requests_rust-0.2.3-cp310-abi3-win_arm64.whl
requests_rust-0.2.3-cp310-abi3-manylinux_2_34_x86_64.whl
requests_rust-0.2.3-cp310-abi3-manylinux_2_34_aarch64.whl
```

## 4. 交接检查

后续 AI 开始工作前应先检查：

```bash
git remote -v
git status --short
```

预期 `origin` 为：

```text
https://github.com/zt1901/requests_rust-source.git
```

如需更新 wheel 分发仓库，必须使用显式仓库参数，例如：

```bash
gh release upload vX.Y.Z <wheel-files> --repo zt1901/requests_rust
```

不得用 `git push` 向 `zt1901/requests_rust` 推送项目源码。
