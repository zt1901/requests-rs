import subprocess
import sys
import zipfile
import os
from pathlib import Path


# 可右键运行：同步指纹、构建支持Python 3.10及以上版本的wheel并安装。
项目目录 = Path(__file__).resolve().parent
输出目录 = 项目目录 / "dist"
源码原生模块 = 项目目录 / "python" / "requests_rust" / "_native.pyd"
本机构建缓存 = Path(r"D:\BuildCache\requests-rust-target")
本机LLVM目录 = Path(r"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\Llvm\x64\bin")


def run(*command):
    print("运行:", " ".join(map(str, command)))
    environment = os.environ.copy()
    # 本机缓存和 LLVM 不写入仓库级 Cargo 配置，确保 Linux/Windows ARM64 CI 可移植构建。
    environment["CARGO_TARGET_DIR"] = str(本机构建缓存)
    if 本机LLVM目录.is_dir():
        environment["LIBCLANG_PATH"] = str(本机LLVM目录)
    subprocess.run(command, cwd=项目目录, check=True, env=environment)


def 同步可编辑原生模块(wheel: Path):
    """editable 安装通过 .pth 指向源码目录，需同步 wheel 内的新原生模块。"""
    with zipfile.ZipFile(wheel) as archive:
        with archive.open("requests_rust/_native.pyd") as source:
            源码原生模块.write_bytes(source.read())
    print("已同步 editable 原生模块:", 源码原生模块)


def main():
    run(sys.executable, str(项目目录 / "scripts" / "同步指纹.py"))
    run("uv", "run", "maturin", "build", "--release", "--out", str(输出目录))
    wheels = sorted(输出目录.glob("requests_rust-*.whl"), key=lambda path: path.stat().st_mtime)
    if not wheels:
        raise RuntimeError("构建完成但没有找到wheel")
    wheel = wheels[-1]
    同步可编辑原生模块(wheel)
    run("uv", "pip", "install", "--reinstall", str(wheel))
    print("安装完成:", wheel)


if __name__ == "__main__":
    main()
