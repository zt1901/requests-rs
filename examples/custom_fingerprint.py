from pathlib import Path

from requests_rs import requests


# 【可调参数】指向指纹捕获器导出的单profile JSON。
指纹文件 = Path(r"C:\fingerprints\edge152.json")
目标地址 = "https://example.com/"

# Header使用二元组序列，便于保留浏览器请求中的名称顺序和重复字段。
浏览器请求头 = [
    ("upgrade-insecure-requests", "1"),
    ("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    ("sec-fetch-site", "none"),
    ("sec-fetch-mode", "navigate"),
    ("sec-fetch-user", "?1"),
    ("sec-fetch-dest", "document"),
]


def main() -> None:
    if not 指纹文件.is_file():
        raise FileNotFoundError(f"请先把指纹文件路径改成真实JSON: {指纹文件}")

    with requests.Session(
        impersonate=指纹文件,
        fingerprint_rotation=False,
        headers=浏览器请求头,
        timeout=30,
    ) as 会话:
        print("文件内指纹数量:", 会话.fingerprint_count)
        响应 = 会话.get(目标地址)
        响应.raise_for_status()
        print("状态码:", 响应.status_code)
        print("实际协议:", 响应.http_version)
        print("本次指纹编号:", 响应.fingerprint_id)


if __name__ == "__main__":
    main()
