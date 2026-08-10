import subprocess
import sys
from pathlib import Path


# 可右键运行：同步指纹、构建支持Python 3.10及以上版本的wheel并安装。
项目目录 = Path(__file__).resolve().parent
输出目录 = 项目目录 / "dist"


def run(*command):
    print("运行:", " ".join(map(str, command)))
    subprocess.run(command, cwd=项目目录, check=True)


def main():
    run(sys.executable, str(项目目录 / "scripts" / "同步指纹.py"))
    run("uv", "run", "maturin", "build", "--release", "--out", str(输出目录))
    wheels = sorted(输出目录.glob("requests_rust-*.whl"), key=lambda path: path.stat().st_mtime)
    if not wheels:
        raise RuntimeError("构建完成但没有找到wheel")
    run("uv", "pip", "install", "--reinstall", str(wheels[-1]))
    print("安装完成:", wheels[-1])


if __name__ == "__main__":
    main()
