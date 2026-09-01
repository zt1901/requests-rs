from requests_rust import Session


# 【可调参数】右键运行前只需要修改这里。
目标地址 = "https://example.com/"
指纹版本 = "edge152"
请求超时秒数 = 30


def main() -> None:
    with Session(
        impersonate=指纹版本,
        timeout=请求超时秒数,
    ) as 会话:
        响应 = 会话.get(
            目标地址,
            headers={"accept": "text/html,application/xhtml+xml"},
        )
        响应.raise_for_status()
        print("状态码:", 响应.status_code)
        print("实际协议:", 响应.http_version)
        print("指纹编号:", 响应.fingerprint_id)
        print("响应字节:", len(响应.content))


if __name__ == "__main__":
    main()
