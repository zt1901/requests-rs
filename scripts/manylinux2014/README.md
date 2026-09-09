# 固定 Linux x86_64 构建环境

固定 manylinux2014 镜像 digest、Rust 1.98.0、maturin 1.9.6、libclang 18.1.1。
目标 glibc >= 2.17，保持发行版 opt-level=3 / fat LTO / codegen-units=1，禁止 target-cpu=native。

在仓库根目录 PowerShell 运行（Docker Linux 引擎已启动）：

```powershell
docker build -t requests-rs-manylinux2014:rust1.98 scripts/manylinux2014
$source = (Get-Location).Path
$out = Join-Path $source 'dist-manylinux2014'
New-Item -ItemType Directory -Force $out | Out-Null
$volume = 'requests-rs-build-' + [guid]::NewGuid().ToString('N')
docker run --name $volume --mount "type=bind,source=$source,target=/input,readonly" --mount "type=bind,source=$out,target=/io" --mount "type=volume,source=$volume,target=/build" requests-rs-manylinux2014:rust1.98 bash /input/scripts/manylinux2014/build.sh
```

每次使用独立 volume，避免旧源码残留和多个任务争抢 target；不要对同一 volume 并发构建。
脚本跑 native、release wheel、包回归、运行取消及 auditwheel；产物和日志写入输出目录。
独立 aioquic 互操作另在装有 aioquic 的隔离 Linux 环境运行 `test_audit_h3_interop.py`。
构建不自动发布，不自动删除 volume，保留失败证据。
