from pathlib import Path
import json
import os
import socket
import subprocess
import sys
import tempfile
import time


# 可右键运行；测试会自行启动和关闭上级目录的本地指纹捕获器。
项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parent
测试地址 = ""
测试版本 = "chrome142"
请求次数 = 4
测试记录文件 = 项目目录 / "test_python_package_records.json"


def 获取空闲端口():
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        return server.getsockname()[1]


def 等待捕获器启动(port: int, server: subprocess.Popen) -> None:
    """轮询监听端口，避免首次证书初始化超过固定 sleep 时间造成误判。"""
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if server.poll() is not None:
            output = server.stdout.read().decode("utf-8", errors="replace") if server.stdout else ""
            raise RuntimeError(f"本次Python指纹捕获器异常退出: {output}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("本次Python指纹捕获器启动超时")


def 断言捕获匹配初始记录(capture: dict, record: dict) -> None:
    """对照会随连接变化的字段之外的 TLS/HTTP2 指纹字段。"""
    expected_tls = record["tls"]
    actual_tls = capture["tls"]
    for field in (
        "cipher_suites",
        "extensions",
        "supported_groups",
        "signature_algorithms",
        "key_share_groups",
        "alpn",
        "ja3",
        "ja4",
    ):
        assert actual_tls.get(field) == expected_tls.get(field), (field, record["id"])
    expected_http = record["http"]
    actual_http = capture["http"]
    for field in ("protocol", "akamai", "settings", "settings_order", "pseudo_header_order"):
        assert actual_http.get(field) == expected_http.get(field), (field, record["id"])


def main():
    global 测试地址
    测试端口 = 获取空闲端口()
    测试地址 = f"https://127.0.0.1:{测试端口}/api/fingerprint?source=python-package"
    environment = os.environ.copy()
    environment["FINGERPRINT_OUTPUT"] = str(测试记录文件)
    environment["FINGERPRINT_PORT"] = str(测试端口)
    测试记录文件.unlink(missing_ok=True)
    server = subprocess.Popen(
        [sys.executable, "fingerprint_server.py"],
        cwd=捕获器目录,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    try:
        等待捕获器启动(测试端口, server)
        from requests_rust import Session, available_profiles

        print("内置版本:", available_profiles())
        with Session(
            impersonate=测试版本,
            verify=False,
        ) as session:
            ids = []
            for _ in range(请求次数):
                response = session.get(测试地址)
                response.raise_for_status()
                ids.append(response.fingerprint_id)
                capture = response.json()
                print(response.fingerprint_id, capture["tls"]["ja3"])
            assert len(set(ids)) == min(请求次数, session.fingerprint_count)

        with Session(impersonate=测试版本, verify=False) as session:
            first = session.get(测试地址)
            second = session.get(测试地址)
            assert first.fingerprint_id != second.fingerprint_id
            print("默认轮换:", first.fingerprint_id, second.fingerprint_id)

        records = json.loads((项目目录 / "fingerprints.json").read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory() as temp_dir:
            chrome_records = [record for record in records if record["profile"] == "chrome142"]
            firefox_records = [record for record in records if record["profile"] == "firefox151"]
            chrome_path = Path(temp_dir) / "chrome142.json"
            firefox_path = Path(temp_dir) / "firefox151.json"
            chrome_path.write_text(json.dumps(chrome_records), encoding="utf-8")
            firefox_path.write_text(json.dumps(firefox_records), encoding="utf-8")
            with Session(
                impersonate="chrome142",
                fingerprints_path=chrome_path,
                verify=False,
            ) as chrome_session, Session(
                impersonate="firefox151",
                fingerprints_path=firefox_path,
                verify=False,
            ) as firefox_session:
                chrome_response = chrome_session.get(测试地址)
                firefox_response = firefox_session.get(测试地址)
                chrome_capture = chrome_response.json()
                firefox_capture = firefox_response.json()
                chrome_record = next(record for record in chrome_records if record["id"] == chrome_response.fingerprint_id)
                firefox_record = next(record for record in firefox_records if record["id"] == firefox_response.fingerprint_id)
                断言捕获匹配初始记录(chrome_capture, chrome_record)
                断言捕获匹配初始记录(firefox_capture, firefox_record)
                assert chrome_capture["tls"]["ja3"] != firefox_capture["tls"]["ja3"]
                print("外部指纹文件隔离对撞验证通过")
    finally:
        server.terminate()
        server.wait(timeout=5)
        测试记录文件.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
