import asyncio
import json
import socket
import tempfile
import threading
import time
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit


# 可右键运行；全部服务使用本机空闲端口，不访问外网。
项目目录 = Path(__file__).resolve().parent
测试版本 = "chrome146"


class 静默处理器(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass


class 目标处理器(静默处理器):
    protocol_version = "HTTP/1.1"
    def do_GET(self):
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/headers")
            self.send_header("Content-Length", "0")
            self.send_header("X-Redirect-Hop", "one")
            self.end_headers()
            return

        if self.path == "/set-path-cookie":
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.send_header("Set-Cookie", "nested=only; Path=/nested")
            self.end_headers()
            return

        if self.path == "/slow":
            self.send_response(200)
            self.send_header("Content-Length", "4")
            self.end_headers()
            self.wfile.write(b"a")
            self.wfile.flush()
            time.sleep(0.5)
            self.wfile.write(b"bcd")
            return

        if self.path == "/slow-headers":
            time.sleep(0.5)
            body = b"late"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        if self.path == "/stream":
            chunks = [b"first", b"second", b"third"]
            self.send_response(200)
            self.send_header("Content-Length", str(sum(map(len, chunks))))
            self.end_headers()
            for chunk in chunks:
                self.wfile.write(chunk)
                self.wfile.flush()
                time.sleep(0.2)
            return

        if self.path == "/broken-stream":
            self.send_response(200)
            self.send_header("Content-Length", "10")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b"bad")
            self.wfile.flush()
            self.close_connection = True
            return

        if self.path.startswith("/query"):
            body = json.dumps({"path": self.path}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        body = json.dumps(
            {
                "duplicate_request_headers": self.headers.get_all("X-Repeat") or [],
                "cookie": self.headers.get("Cookie", ""),
                "user_agent": self.headers.get("User-Agent", ""),
                "sec_ch_ua": self.headers.get("sec-ch-ua", ""),
                "accept": self.headers.get("Accept", ""),
                "sec_fetch_site": self.headers.get("sec-fetch-site", ""),
                "sec_fetch_mode": self.headers.get("sec-fetch-mode", ""),
                "sec_fetch_dest": self.headers.get("sec-fetch-dest", ""),
                "upgrade_insecure_requests": self.headers.get("Upgrade-Insecure-Requests", ""),
                "priority": self.headers.get("Priority", ""),
                "runtime_default": self.headers.get("X-Runtime-Default", ""),
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Set-Cookie", "first=1; Path=/")
        self.send_header("Set-Cookie", "second=2; Path=/")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        content_type = self.headers.get("Content-Type", "")
        if self.path == "/echo-json":
            result = json.dumps(
                {
                    "content_type": content_type,
                    "json": json.loads(body),
                    "raw_body": body.decode("utf-8"),
                },
                ensure_ascii=False,
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(result)))
            self.end_headers()
            self.wfile.write(result)
            return
        if self.path == "/echo-form":
            result = json.dumps(
                {
                    "content_type": content_type,
                    "body": body.decode("ascii"),
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(result)))
            self.end_headers()
            self.wfile.write(result)
            return
        result = json.dumps(
            {
                "content_type": content_type,
                "content_length": length,
                "has_text_field": b'name="title"\r\n\r\ndemo' in body,
                "has_filename": b'filename="custom.bin"' in body,
                "has_file_type": b"Content-Type: application/octet-stream" in body,
                "has_file_content": b"streamed-file-content" in body,
                "has_async_file_content": b"async-streamed-file-content" in body,
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(result)))
        self.end_headers()
        self.wfile.write(result)


class 代理处理器(静默处理器):
    protocol_version = "HTTP/1.1"
    代理名称 = ""
    命中记录 = []

    def do_GET(self):
        target = urlsplit(self.path)
        self.命中记录.append(self.代理名称)
        connection = HTTPConnection(target.hostname, target.port, timeout=5)
        headers = [
            (name, value)
            for name, value in self.headers.raw_items()
            if name.lower() not in {"host", "proxy-connection"}
        ]
        connection.putrequest("GET", target.path or "/", skip_host=True, skip_accept_encoding=True)
        connection.putheader("Host", target.netloc)
        for name, value in headers:
            connection.putheader(name, value)
        connection.endheaders()
        response = connection.getresponse()
        body = response.read()
        self.send_response(response.status)
        for name, value in response.getheaders():
            if name.lower() not in {"connection", "transfer-encoding"}:
                self.send_header(name, value)
        self.send_header("X-Test-Proxy", self.代理名称)
        self.end_headers()
        self.wfile.write(body)
        connection.close()


def 启动服务(handler):
    class 静默服务(ThreadingHTTPServer):
        def handle_error(self, *_):
            pass

    server = 静默服务(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def 创建代理处理器(name):
    return type(f"代理_{name}", (代理处理器,), {"代理名称": name, "命中记录": []})


def main():
    from requests_rust import AsyncSession, Session, get

    target = 启动服务(目标处理器)
    proxy_a_handler = 创建代理处理器("A")
    proxy_b_handler = 创建代理处理器("B")
    proxy_a = 启动服务(proxy_a_handler)
    proxy_b = 启动服务(proxy_b_handler)
    target_url = f"http://127.0.0.1:{target.server_port}"
    proxy_a_url = f"http://127.0.0.1:{proxy_a.server_port}"
    proxy_b_url = f"http://127.0.0.1:{proxy_b.server_port}"
    try:
        with Session(
            impersonate=测试版本,
            proxy=proxy_a_url,
            headers=[("X-Repeat", "session"), ("X-Default", "keep")],
        ) as session:
            first = session.get(
                target_url + "/headers",
                headers=[("X-Repeat", "one"), ("X-Repeat", "two")],
            )
            assert first.json()["duplicate_request_headers"] == ["one", "two"]
            assert first.headers.get_list("set-cookie") == ["first=1; Path=/", "second=2; Path=/"]
            assert first.headers.get_list("x-test-proxy") == ["A"]
            assert proxy_a_handler.命中记录 == ["A"]
            assert proxy_b_handler.命中记录 == []

            映射前A次数 = len(proxy_a_handler.命中记录)
            映射前B次数 = len(proxy_b_handler.命中记录)
            with Session(impersonate=测试版本, proxies={"http": proxy_b_url}) as 映射代理会话:
                mapped = 映射代理会话.get(target_url + "/headers")
                assert mapped.headers["x-test-proxy"] == "B"
                request_mapped = 映射代理会话.get(
                    target_url + "/headers",
                    proxies={"http://": proxy_a_url},
                )
                assert request_mapped.headers["x-test-proxy"] == "A"
                fallback_mapped = 映射代理会话.get(
                    target_url + "/headers",
                    proxies={"all": proxy_a_url},
                )
                assert fallback_mapped.headers["x-test-proxy"] == "A"
            assert len(proxy_a_handler.命中记录) == 映射前A次数 + 2
            assert len(proxy_b_handler.命中记录) == 映射前B次数 + 1
            try:
                Session(impersonate=测试版本, proxy=proxy_a_url, proxies={"http": proxy_b_url})
            except TypeError as error:
                assert "proxy和proxies" in str(error)
            else:
                raise AssertionError("proxy和proxies同时传入没有被拒绝")

            # 浏览器cURL与业务代码传入的Header原样优先，库不接管UA或Client Hints。
            覆盖请求头 = {
                "User-Agent": "RequestsRustOverride/1.0",
                "sec-ch-ua": '"RequestsRustOverride";v="1"',
            }
            默认覆盖 = session.get(target_url + "/headers", headers=覆盖请求头).json()
            assert 默认覆盖["user_agent"] == 覆盖请求头["User-Agent"], 默认覆盖
            assert 默认覆盖["sec_ch_ua"] == 覆盖请求头["sec-ch-ua"], 默认覆盖

            query = session.get(
                target_url + "/query?existing=one#fragment",
                params={"tag": ["A B", "中文"], "empty": "", "skip": None},
                proxy=None,
            ).json()["path"]
            assert query == "/query?existing=one&tag=A+B&tag=%E4%B8%AD%E6%96%87&empty=&skip=None", query
            tuple_query = session.get(
                target_url + "/query",
                params={"bool": True, "number": 7, "tuple": ("x/y", b"raw bytes")},
                proxy=None,
            ).json()["path"]
            assert tuple_query == "/query?bool=True&number=7&tuple=x%2Fy&tuple=raw+bytes", tuple_query
            form = session.post(
                target_url + "/echo-form",
                data={"tag": ["A B", "中文"], "empty": "", "none": None, "raw": b"raw bytes"},
                proxy=None,
            ).json()
            assert form == {
                "content_type": "application/x-www-form-urlencoded",
                "body": "tag=A+B&tag=%E4%B8%AD%E6%96%87&empty=&none=None&raw=raw+bytes",
            }, form
            encoded_json = session.post(
                target_url + "/echo-json",
                json={"text": "中文", "items": [True, None, 7], "nested": {"space": "A B"}},
                proxy=None,
            ).json()
            assert encoded_json == {
                "content_type": "application/json",
                "json": {"text": "中文", "items": [True, None, 7], "nested": {"space": "A B"}},
                "raw_body": '{"text":"中文","items":[true,null,7],"nested":{"space":"A B"}}',
            }, encoded_json
            fallback_json = session.post(
                target_url + "/echo-json",
                json={"large": 10**100},
                proxy=None,
            ).json()
            assert fallback_json["raw_body"] == json.dumps({"large": 10**100}, ensure_ascii=False, separators=(",", ":"))

            # 默认Header由Rust预解析缓存，但保留用户对session.headers的运行时修改习惯。
            session.headers.append(("X-Runtime-Default", "changed"))
            修改后默认头 = session.get(target_url + "/headers").json()
            assert 修改后默认头["runtime_default"] == "changed", 修改后默认头

            # 一次顶层导航的上下文头不能作为所有业务请求的默认画像发送。
            默认上下文 = session.get(target_url + "/headers").json()
            for key in (
                "accept",
                "sec_fetch_site",
                "sec_fetch_mode",
                "sec_fetch_dest",
                "upgrade_insecure_requests",
                "priority",
            ):
                assert 默认上下文[key] == "", (key, 默认上下文)

            session.set_proxy(proxy_b_url)
            切换前B次数 = len(proxy_b_handler.命中记录)
            second = session.get(target_url + "/headers")
            assert second.headers["x-test-proxy"] == "B"
            assert "first=1" in second.json()["cookie"]
            assert "second=2" in second.json()["cookie"]
            assert len(proxy_b_handler.命中记录) == 切换前B次数 + 1

            direct = session.get(target_url + "/headers", proxy=None)
            assert "x-test-proxy" not in direct.headers
            proxied_again = session.get(target_url + "/headers")
            assert proxied_again.headers["x-test-proxy"] == "B"

            # 传输统计只适用于HTTPS/TLS；专用HTTPS代理基准会校验准确字节计数。
            assert direct.transfer_stats is None

            session.cookies.set("manual", "value", url=target_url)
            assert session.cookies["manual"] == "value"
            assert any(cookie.name == "manual" for cookie in session.cookies.get_all())
            assert "manual=value" in session.get(target_url + "/headers").json()["cookie"]
            session.get(target_url + "/set-path-cookie")
            assert "nested=only" not in session.get(target_url + "/headers").json()["cookie"]
            nested_cookie = session.get(target_url + "/nested/headers").json()["cookie"]
            assert "nested=only" in nested_cookie
            request_cookie = session.get(
                target_url + "/headers",
                cookies={"first": "override", "request_only": "snapshot"},
            ).json()["cookie"]
            assert "first=override" in request_cookie
            assert "second=2" in request_cookie
            assert "request_only=snapshot" in request_cookie
            merged_cookie = session.get(
                target_url + "/headers",
                headers={"Cookie": "manual=header"},
                cookies={"request_only": "snapshot"},
            ).json()["cookie"]
            assert "manual=header" not in merged_cookie
            assert "first=1" in merged_cookie
            assert "request_only=snapshot" in merged_cookie
            list_cookie = session.get(
                target_url + "/headers",
                cookies=[("first", "list-override"), ("list_only", "value")],
            ).json()["cookie"]
            assert "first=list-override" in list_cookie
            assert "second=2" in list_cookie
            assert "list_only=value" in list_cookie
            session.cookies.delete("manual", url=target_url)
            assert "manual" not in session.cookies

            stopped = session.get(target_url + "/redirect", allow_redirects=False)
            assert stopped.status_code == 302
            assert stopped.url.endswith("/redirect")
            assert stopped.history == []

            followed = session.get(target_url + "/redirect")
            assert followed.status_code == 200
            assert followed.url.endswith("/headers")
            assert len(followed.history) == 1
            assert followed.history[0].status_code == 302
            assert followed.history[0].headers["x-redirect-hop"] == "one"

            try:
                session.get(
                    target_url + "/slow",
                    read_timeout=0.1,
                    stream=True,
                    proxy=None,
                ).content
            except RuntimeError as error:
                message = str(error).lower()
                assert "timeout" in message or "timed out" in message
            else:
                raise AssertionError("慢响应没有触发读取超时")

            started = time.perf_counter()
            with session.get(target_url + "/stream", stream=True) as streamed:
                assert time.perf_counter() - started < 0.5
                chunks = list(streamed.iter_content(5))
                assert b"".join(chunks) == b"firstsecondthird"
                assert all(0 < len(chunk) <= 5 for chunk in chunks)

            session.cookies.clear()
            assert session.cookies.get_dict() == {}

            with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as upload_file:
                upload_file.write(b"streamed-file-content")
                upload_path = upload_file.name
            try:
                uploaded = session.post(
                    target_url + "/upload",
                    data={"title": "demo"},
                    files={
                        "document": (
                            "custom.bin",
                            upload_path,
                            "application/octet-stream",
                        )
                    },
                    proxy=None,
                ).json()
                assert uploaded["content_type"].startswith("multipart/form-data; boundary=")
                assert uploaded["content_length"] > len(b"streamed-file-content")
                assert uploaded["has_text_field"]
                assert uploaded["has_filename"]
                assert uploaded["has_file_type"]
                assert uploaded["has_file_content"]
            finally:
                Path(upload_path).unlink(missing_ok=True)

        with tempfile.TemporaryDirectory() as temp_dir:
            # 从内置指纹中按版本拆成两个互不相同的实例文件，验证按实例独立加载
            全部记录 = json.loads(
                (项目目录 / "fingerprints.json").read_text(encoding="utf-8")
            )
            chrome文件 = Path(temp_dir) / "chrome_only.json"
            firefox文件 = Path(temp_dir) / "firefox_only.json"
            chrome文件.write_text(
                json.dumps(
                    [记录 for 记录 in 全部记录 if 记录["profile"] == 测试版本],
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )
            firefox文件.write_text(
                json.dumps(
                    [记录 for 记录 in 全部记录 if 记录["profile"] == "firefox151"],
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )

            # 同一进程两个实例各读各的指纹文件，互不干扰
            with Session(
                impersonate=测试版本,
                fingerprints_path=chrome文件,
            ) as 自定义chrome:
                with Session(
                    impersonate="firefox151",
                    fingerprints_path=firefox文件,
                ) as 自定义firefox:
                    assert 自定义chrome.fingerprint_count == len(
                        [
                            记录
                            for 记录 in 全部记录
                            if 记录["profile"] == 测试版本 and 41 not in 记录["tls"]["extensions"]
                        ]
                    )
                    assert 自定义firefox.fingerprint_count == len(
                        [
                            记录
                            for 记录 in 全部记录
                            if 记录["profile"] == "firefox151" and 41 not in 记录["tls"]["extensions"]
                        ]
                    )
                    with Session(
                        impersonate=测试版本,
                        fingerprints_path=chrome文件,
                        fingerprint_rotation=True,
                    ) as 轮换chrome:
                        chrome_ids = [
                            轮换chrome.get(target_url + f"/headers?variant={index}").fingerprint_id
                            for index in range(min(3, 轮换chrome.fingerprint_count))
                        ]
                    assert len(set(chrome_ids)) == len(chrome_ids)
                    assert 自定义firefox.get(target_url + "/headers").status_code == 200
                    assert (
                        自定义firefox.get(target_url + "/headers").impersonate
                        == "firefox151"
                    )

            # 文件中不存在的版本直接报错，并列出该文件里的可用版本
            try:
                Session(impersonate=测试版本, fingerprints_path=firefox文件)
            except RuntimeError as error:
                message = str(error)
                assert "指纹文件中没有" in message
                assert "firefox151" in message
            else:
                raise AssertionError("指纹文件中不存在的版本没有被拒绝")

            # 不存在的文件路径直接报读取错误
            try:
                Session(
                    impersonate=测试版本,
                    fingerprints_path=Path(temp_dir) / "missing.json",
                )
            except RuntimeError as error:
                assert "无法读取指纹文件" in str(error)
            else:
                raise AssertionError("不存在的指纹文件路径没有被拒绝")

            # 不传路径的实例仍使用内置指纹
            with Session(impersonate=测试版本) as 内置会话:
                assert 内置会话.get(target_url + "/headers").status_code == 200

            # 单次请求API同样透传实例级指纹文件，而不是将参数误传给Session.request。
            assert (
                get(
                    target_url + "/headers",
                    impersonate=测试版本,
                    fingerprints_path=chrome文件,
                    proxy=None,
                ).status_code
                == 200
            )

        shortcut = get(
            target_url + "/headers",
            impersonate=测试版本,
            proxy=proxy_a_url,
            verify=False,
        )
        assert shortcut.headers["x-test-proxy"] == "A"

        async def 测试异步API():
            from requests_rust import Response
            from requests_rust._native import NativeSession

            # 公共包装已编译进_native.pyd，源码不可再由inspect读取；异步行为以下方真实并发与流式请求验证。
            assert AsyncSession.__module__ == "requests_rust._embedded_api"
            assert Response.__module__ == "requests_rust._embedded_api"
            async with AsyncSession(impersonate=测试版本) as session:
                responses = await asyncio.gather(
                    session.get(target_url + "/headers"),
                    session.get(target_url + "/headers"),
                    session.get(target_url + "/headers"),
                )
                assert all(response.status_code == 200 for response in responses)
                mapped = await session.get(target_url + "/headers", proxies={"http": proxy_a_url})
                assert mapped.headers["x-test-proxy"] == "A"
                streamed = await session.get(target_url + "/stream", stream=True)
                body = bytearray()
                async for chunk in streamed.aiter_content(4):
                    body.extend(chunk)
                assert bytes(body) == b"firstsecondthird"

                closing = await session.get(target_url + "/slow", stream=True)
                pending_read = asyncio.ensure_future(closing._stream.read_async(4))
                await asyncio.sleep(0.05)
                started = time.perf_counter()
                closing.close()
                assert time.perf_counter() - started < 0.05
                assert await asyncio.wait_for(pending_read, 0.2) == b""

                async_closing = await session.get(target_url + "/slow", stream=True)
                pending_read = asyncio.ensure_future(async_closing._stream.read_async(4))
                await asyncio.sleep(0.05)
                await asyncio.wait_for(async_closing.aclose(), 0.2)
                assert await asyncio.wait_for(pending_read, 0.2) == b""

                early = await session.get(target_url + "/stream", stream=True)
                early_consumer = early.aiter_content(5)
                assert await anext(early_consumer) == b"first"
                await early_consumer.aclose()
                assert early._stream is None

                single_consumer = await session.get(target_url + "/stream", stream=True)
                first_consumer = single_consumer.aiter_content(5)
                second_consumer = single_consumer.aiter_content(5)
                assert await anext(first_consumer) == b"first"
                try:
                    await anext(second_consumer)
                except RuntimeError as error:
                    assert "一个活动消费者" in str(error)
                else:
                    raise AssertionError("同一响应允许了第二个活动消费者")
                await first_consumer.aclose()
                assert single_consumer._stream is None

                broken = await session.get(target_url + "/broken-stream", stream=True)
                try:
                    await broken._stream.read_async()
                except RuntimeError as first_error:
                    try:
                        await broken._stream.read_async()
                    except RuntimeError as second_error:
                        assert str(second_error) == str(first_error)
                    else:
                        raise AssertionError("Body错误后仍可继续读取")
                else:
                    raise AssertionError("截断Body没有产生终止错误")
                await broken.aclose()

                pending_stream = asyncio.create_task(
                    session.get(target_url + "/slow-headers", stream=True)
                )
                await asyncio.sleep(0.1)
                pending_stream.cancel()
                try:
                    await pending_stream
                except asyncio.CancelledError:
                    pass
                else:
                    raise AssertionError("异步流请求没有被取消")
                assert (await session.get(target_url + "/headers")).status_code == 200

                with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as upload_file:
                    upload_file.write(b"async-streamed-file-content")
                    async_upload_path = upload_file.name
                try:
                    uploaded = await session.post(
                        target_url + "/upload",
                        data={"title": "demo"},
                        files={
                            "document": (
                                "custom.bin",
                                async_upload_path,
                                "application/octet-stream",
                            )
                        },
                        proxy=None,
                    )
                    uploaded_data = uploaded.json()
                    assert uploaded_data["content_type"].startswith(
                        "multipart/form-data; boundary="
                    )
                    assert uploaded_data["has_text_field"]
                    assert uploaded_data["has_filename"]
                    assert uploaded_data["has_file_type"]
                    assert uploaded_data["has_async_file_content"]
                    assert uploaded_data["content_length"] > len(
                        b"async-streamed-file-content"
                    )
                finally:
                    Path(async_upload_path).unlink(missing_ok=True)

                cancelled = await session.get(target_url + "/slow", stream=True)
                pending_read = asyncio.ensure_future(cancelled._stream.read_async(4))
                await asyncio.sleep(0.1)
                pending_read.cancel()
                try:
                    await pending_read
                except asyncio.CancelledError:
                    pass
                else:
                    raise AssertionError("异步流读取任务没有被取消")
                await asyncio.sleep(0.05)
                assert await asyncio.wait_for(cancelled._stream.read_async(4), 1) == b"abcd"
                cancelled.close()

                concurrent = await session.get(target_url + "/slow", stream=True)
                first_read = asyncio.ensure_future(concurrent._stream.read_async(4))
                await asyncio.sleep(0.05)
                try:
                    await concurrent._stream.read_async(1)
                except RuntimeError as error:
                    assert "一个活动读取" in str(error)
                else:
                    raise AssertionError("同一原生流允许了并发读取")
                assert await first_read == b"abcd"
                await concurrent.aclose()

                active = asyncio.create_task(session.get(target_url + "/slow-headers"))
                await asyncio.sleep(0.1)
                await session.close()
                assert (await active).content == b"late"
                try:
                    await session.get(target_url + "/headers")
                except RuntimeError as error:
                    assert "Session已经关闭" in str(error)
                else:
                    raise AssertionError("关闭后的Session仍接受新请求")

            for invalid_timeout in (float("nan"), float("inf"), float("-inf"), 0, -1):
                try:
                    AsyncSession(impersonate=测试版本, timeout=invalid_timeout)
                except ValueError:
                    pass
                else:
                    raise AssertionError("Python边界接受了无效timeout")

            native = NativeSession(测试版本)
            for invalid_timeout in (float("nan"), float("inf"), 1e308):
                try:
                    native.request_async(
                        "GET",
                        target_url + "/headers",
                        [],
                        None,
                        invalid_timeout,
                        None,
                        False,
                        None,
                        None,
                        True,
                        10,
                    )
                except RuntimeError as error:
                    assert "有限正数" in str(error) or "可表示范围" in str(error)
                else:
                    raise AssertionError("Rust边界接受了无效timeout")
            native.close()

        asyncio.run(测试异步API())
        print("动态代理、Cookie、重复头、重定向、超时、原生异步流、异步multipart和API验证通过")
    finally:
        for server in (proxy_b, proxy_a, target):
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    main()
