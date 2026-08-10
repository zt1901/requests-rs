from pathlib import Path
import os
import socket
import subprocess
import sys
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
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    try:
        time.sleep(1)
        if server.poll() is not None:
            raise RuntimeError("本次Python指纹捕获器没有成功启动")
        from requests_rust import Session, available_profiles

        print("内置版本:", available_profiles())
        with Session(
            impersonate=测试版本,
            fingerprint_rotation=True,
            verify=False,
        ) as session:
            ids = []
            for _ in range(请求次数):
                response = session.get(测试地址)
                response.raise_for_status()
                ids.append(response.fingerprint_id)
                print(response.fingerprint_id, response.json()["tls"]["ja3"])
            assert len(set(ids)) == min(请求次数, session.fingerprint_count)

        with Session(impersonate=测试版本, verify=False) as session:
            first = session.get(测试地址)
            second = session.get(测试地址)
            assert first.fingerprint_id == second.fingerprint_id
            print("固定指纹:", first.fingerprint_id)
    finally:
        server.terminate()
        server.wait(timeout=5)
        测试记录文件.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
