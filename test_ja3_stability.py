from __future__ import annotations

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


# 可右键运行；测试故意每次新建Session，强制新TLS连接并对照JA3扩展乱序。
项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parent
请求次数 = 20


def 获取空闲端口() -> int:
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        return server.getsockname()[1]


def 等待端口(port: int, process: subprocess.Popen) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("指纹捕获器提前退出")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("指纹捕获器启动超时")


def main() -> None:
    from curl_cffi.requests import Session as CurlSession
    from requests_rust import Session

    port = 获取空闲端口()
    url = f"https://127.0.0.1:{port}/api/fingerprint?source=ja3-stability"
    output = 项目目录 / "test_ja3_stability_records.json"
    environment = os.environ.copy()
    environment["FINGERPRINT_OUTPUT"] = str(output)
    environment["FINGERPRINT_PORT"] = str(port)
    output.unlink(missing_ok=True)
    process = subprocess.Popen(
        [sys.executable, "fingerprint_server.py"],
        cwd=捕获器目录,
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    try:
        等待端口(port, process)
        records = json.loads((项目目录 / "fingerprints.json").read_text(encoding="utf-8"))
        valid = [
            record
            for record in records
            if record["profile"] == "chrome150" and 41 not in record["tls"]["extensions"]
        ]
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "one-fingerprint.json"
            path.write_text(json.dumps([valid[0]]), encoding="utf-8")
            rust_ja3 = []
            rust_ja4 = []
            for _ in range(请求次数):
                with Session(impersonate=path, verify=False) as session:
                    response = session.get(url, transfer_stats=True)
                    tls = response.json()["tls"]
                    rust_ja3.append(tls["ja3"])
                    rust_ja4.append(tls["ja4"])

        curl_ja3 = []
        curl_ja4 = []
        for _ in range(请求次数):
            with CurlSession(impersonate="chrome", verify=False) as session:
                response = session.get(url, timeout=30)
                tls = response.json()["tls"]
                curl_ja3.append(tls["ja3"])
                curl_ja4.append(tls["ja4"])

        print("Rust固定指纹JA3唯一数:", len(set(rust_ja3)))
        print("curl_cffi JA3唯一数:", len(set(curl_ja3)))
        def ja3n(value: str) -> str:
            parts = value.split(",")
            parts[2] = "-".join(sorted(parts[2].split("-"), key=int))
            return ",".join(parts)

        print("Rust固定指纹JA3N唯一数:", len({ja3n(value) for value in rust_ja3}))
        print("curl_cffi JA3N唯一数:", len({ja3n(value) for value in curl_ja3}))
        print("Rust固定指纹JA4唯一数:", len(set(rust_ja4)))
        print("curl_cffi JA4唯一数:", len(set(curl_ja4)))
        print("Rust JA4:", *sorted(set(rust_ja4)), sep="\n")
        print("curl_cffi JA4:", *sorted(set(curl_ja4)), sep="\n")
        assert len(set(rust_ja3)) == 请求次数
        assert len(set(curl_ja3)) == 请求次数
        assert len({ja3n(value) for value in rust_ja3}) == 1
        assert len({ja3n(value) for value in curl_ja3}) == 1
        assert len(set(rust_ja4)) == 1
        assert len(set(curl_ja4)) == 1
        print("Rust JA3:")
        print(*sorted(set(rust_ja3)), sep="\n")
        print("curl_cffi JA3:")
        print(*sorted(set(curl_ja3)), sep="\n")
    finally:
        process.terminate()
        process.wait(timeout=5)
        output.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
