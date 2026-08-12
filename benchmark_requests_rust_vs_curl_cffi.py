import gc
import statistics
import time

from curl_cffi import requests as curl_requests
from requests_rust import Session


# 可右键运行；先用Debian 12容器在本机18080端口启动HTTP服务。
测试地址 = "http://127.0.0.1:18080/"
测试版本 = "chrome142"
创建次数 = 200
预热次数 = 10
请求次数 = 200
重复轮数 = 3


def 测量创建耗时(factory):
    started = time.perf_counter()
    sessions = [factory() for _ in range(创建次数)]
    elapsed = time.perf_counter() - started
    for session in sessions:
        session.close()
    return elapsed


def 测量请求耗时(factory):
    session = factory()
    try:
        for _ in range(预热次数):
            response = session.get(测试地址)
            response.raise_for_status()
        started = time.perf_counter()
        for _ in range(请求次数):
            response = session.get(测试地址)
            response.raise_for_status()
        return time.perf_counter() - started
    finally:
        session.close()


def main():
    factories = {
        "requests_rust": lambda: Session(impersonate=测试版本),
        "requests_rust默认轮换": lambda: Session(impersonate=测试版本),
        "curl_cffi": lambda: curl_requests.Session(impersonate="chrome"),
    }
    for name, factory in factories.items():
        gc.collect()
        creation = [测量创建耗时(factory) for _ in range(重复轮数)]
        requests = [测量请求耗时(factory) for _ in range(重复轮数)]
        creation_median = statistics.median(creation)
        request_median = statistics.median(requests)
        print(
            f"{name}: 创建{创建次数}个Session={creation_median:.4f}秒，"
            f"{请求次数}次请求={request_median:.4f}秒，"
            f"吞吐={请求次数 / request_median:.2f}请求/秒"
        )


if __name__ == "__main__":
    main()
