import subprocess
import sys
import zipfile
import os
import shutil
import tempfile
from pathlib import Path


# 可右键运行：同步指纹、构建支持Python 3.10及以上版本的wheel并安装。
项目目录 = Path(__file__).resolve().parent
输出目录 = 项目目录 / "dist"
源码原生模块 = 项目目录 / "python" / "requests_rust" / "_native.pyd"
本机构建缓存 = Path(r"D:\BuildCache\requests-rust-target")
本机LLVM目录 = Path(r"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\Llvm\x64\bin")
本机Cargo目录 = Path(r"D:\Rust\cargo")
本机Rustup目录 = Path(r"D:\Rust\rustup")


def run(*command):
    print("运行:", " ".join(map(str, command)))
    environment = os.environ.copy()
    # 本机缓存和 LLVM 不写入仓库级 Cargo 配置，确保 Linux/Windows ARM64 CI 可移植构建。
    environment["CARGO_TARGET_DIR"] = str(本机构建缓存)
    if 本机LLVM目录.is_dir():
        environment["LIBCLANG_PATH"] = str(本机LLVM目录)
    if 本机Cargo目录.is_dir() and 本机Rustup目录.is_dir():
        environment["CARGO_HOME"] = str(本机Cargo目录)
        environment["RUSTUP_HOME"] = str(本机Rustup目录)
        environment["PATH"] = str(本机Cargo目录 / "bin") + os.pathsep + environment.get("PATH", "")
    subprocess.run(command, cwd=项目目录, check=True, env=environment)


def 同步可编辑原生模块(wheel: Path):
    """editable 安装通过 .pth 指向源码目录，需同步 wheel 内的新原生模块。"""
    with zipfile.ZipFile(wheel) as archive:
        with archive.open("requests_rust/_native.pyd") as source:
            临时模块 = 源码原生模块.with_suffix(".pyd.tmp")
            临时模块.write_bytes(source.read())
            os.replace(临时模块, 源码原生模块)
    print("已同步 editable 原生模块:", 源码原生模块)


def main():
    if os.name != "nt":
        raise RuntimeError("build_and_install.py仅用于Windows本机右键构建")
    run(sys.executable, str(项目目录 / "scripts" / "同步指纹.py"))
    maturin = shutil.which("maturin")
    if maturin is None:
        raise RuntimeError("没有找到maturin可执行文件")
    with tempfile.TemporaryDirectory(prefix="requests-rust-wheel-") as temp_dir:
        build_output = Path(temp_dir)
        run(maturin, "build", "--release", "--out", str(build_output))
        wheels = list(build_output.glob("requests_rust-*.whl"))
        if len(wheels) != 1:
            raise RuntimeError(f"本次构建应生成唯一wheel，实际为: {wheels}")
        wheel = wheels[0]
        run("uv", "pip", "install", "--python", sys.executable, "--reinstall", str(wheel))
        同步可编辑原生模块(wheel)
        输出目录.mkdir(parents=True, exist_ok=True)
        final_wheel = 输出目录 / wheel.name
        shutil.copy2(wheel, final_wheel)
        print("安装完成:", final_wheel)


if __name__ == "__main__":
    main()
