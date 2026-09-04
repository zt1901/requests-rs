from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path


# 【可调参数】右键运行；每次新建Session以强制产生新的Edge TLS握手。
项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parent
请求次数 = 8


def 是GREASE(值: int) -> bool:
    return 值 & 0x0F0F == 0x0A0A and 值 >> 8 == 值 & 0xFF


def 归一化签名算法(列表: list[int]) -> list[int | str]:
    return ["GREASE" if 是GREASE(值) else 值 for 值 in 列表]


def 获取空闲端口() -> int:
    with socket.socket() as 服务:
        服务.bind(("127.0.0.1", 0))
        return 服务.getsockname()[1]


def 等待端口(端口: int, 进程: subprocess.Popen) -> None:
    截止时间 = time.monotonic() + 15
    while time.monotonic() < 截止时间:
        if 进程.poll() is not None:
            raise RuntimeError("指纹服务提前退出")
        try:
            with socket.create_connection(("127.0.0.1", 端口), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise TimeoutError("指纹服务启动超时")


def main() -> None:
    from requests_rs import requests
    Session = requests.Session

    所有记录 = json.loads((项目目录 / "fingerprints.json").read_text(encoding="utf-8"))
    源记录 = next(记录 for 记录 in 所有记录 if 记录["profile"] == "edge152")
    预期签名算法 = 归一化签名算法(源记录["tls"]["signature_algorithms"])
    预期JA4 = 源记录["tls"]["ja4"]
    预期长度 = 源记录["tls"]["client_hello_length"]

    端口 = 获取空闲端口()
    地址 = f"https://127.0.0.1:{端口}/api/fingerprint?source=edge152-grease"
    输出文件 = 项目目录 / "test_edge152_grease_records.json"
    环境 = os.environ.copy()
    环境["FINGERPRINT_OUTPUT"] = str(输出文件)
    环境["FINGERPRINT_PORT"] = str(端口)
    输出文件.unlink(missing_ok=True)
    进程 = subprocess.Popen(
        [sys.executable, "fingerprint_server.py"],
        cwd=捕获器目录,
        env=环境,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    try:
        等待端口(端口, 进程)
        with tempfile.TemporaryDirectory() as 临时目录:
            指纹文件 = Path(临时目录) / "edge152.json"
            指纹文件.write_text(json.dumps([源记录]), encoding="utf-8")
            for _ in range(请求次数):
                with Session(impersonate=指纹文件, fingerprint_rotation=False, verify=False) as 会话:
                    TLS = 会话.get(地址).json()["tls"]
                assert 归一化签名算法(TLS["signature_algorithms"]) == 预期签名算法, TLS
                assert TLS["ja4"] == 预期JA4, TLS
                assert abs(TLS["client_hello_length"] - 预期长度) % 32 == 0, TLS
        print(f"Edge 152签名GREASE、规范JA4和ClientHello长度桶验证通过: {请求次数}次握手")
    finally:
        进程.terminate()
        进程.wait(timeout=5)
        输出文件.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
