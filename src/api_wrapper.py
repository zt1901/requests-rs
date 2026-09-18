import asyncio
import json as json_module
import math
import os
from http import HTTPStatus
from http.cookiejar import CookieJar
from collections.abc import Iterator, Mapping, MutableSequence, Sequence
from http.cookies import SimpleCookie
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlencode, urlsplit, urlunsplit
from urllib.request import Request

from ._native import NativeSession, build_response_headers


HeaderInput = Mapping[str, str] | Sequence[tuple[str, str]]
FileInput = Mapping[str, str | os.PathLike[str] | tuple[str, str | os.PathLike[str], str | None]]
ProxyInput = Mapping[str, str | None]
FingerprintDocument = Mapping[str, Any] | Sequence[Mapping[str, Any]]
FingerprintInput = str | os.PathLike[str] | FingerprintDocument
_UNSET = object()


class Headers(Mapping[str, str]):
    def __init__(self, values: Sequence[tuple[str, str]] = ()) -> None:
        self._native = build_response_headers(list(values))
        self._raw = None

    @classmethod
    def _from_native(cls, native: Any) -> "Headers":
        instance = cls.__new__(cls)
        instance._native = native
        instance._raw = None
        return instance

    @property
    def raw(self) -> list[tuple[str, str]]:
        if self._raw is None:
            self._raw = self._native.raw
        return self._raw

    def __getitem__(self, name: str) -> str:
        value = self._native.get(name, None)
        if value is None:
            raise KeyError(name)
        return value

    def __iter__(self) -> Iterator[str]:
        return iter(self._native.names())

    def __len__(self) -> int:
        return self._native.len()

    def get_list(self, name: str) -> list[str]:
        return self._native.get_list(name)

    def items(self, multi: bool = False):
        if multi:
            return iter(self.raw)
        return super().items()


class MutableHeaders(MutableSequence[tuple[str, str]]):
    """保留重复项和顺序，同时兼容 curl_cffi 的 headers.update 用法。"""

    def __init__(self, values: HeaderInput | None = None) -> None:
        self._values = _header_items(values)

    def __getitem__(self, index):
        return self._values[index]

    def __setitem__(self, index, value) -> None:
        if isinstance(index, slice):
            self._values[index] = _header_items(value)
        else:
            self._values[index] = (str(value[0]), str(value[1]))

    def __delitem__(self, index) -> None:
        del self._values[index]

    def __len__(self) -> int:
        return len(self._values)

    def insert(self, index: int, value: tuple[str, str]) -> None:
        self._values.insert(index, (str(value[0]), str(value[1])))

    def update(self, values: HeaderInput | None = None, **kwargs: str) -> None:
        incoming = _header_items(values)
        incoming.extend((str(name), str(value)) for name, value in kwargs.items())
        replaced = {name.casefold() for name, _ in incoming}
        self._values[:] = [
            (name, value) for name, value in self._values if name.casefold() not in replaced
        ]
        self._values.extend(incoming)


@dataclass(frozen=True, slots=True)
class Cookie:
    name: str
    value: str
    domain: str | None
    path: str | None
    secure: bool
    http_only: bool


class Cookies(Mapping[str, str]):
    def __init__(self, native: NativeSession) -> None:
        self._native = native

    def _all(self) -> list[Cookie]:
        return [Cookie(*item) for item in self._native.get_cookies()]

    def __getitem__(self, name: str) -> str:
        matches = [cookie.value for cookie in self._all() if cookie.name == name]
        if not matches:
            raise KeyError(name)
        if len(matches) > 1:
            raise KeyError(f"Cookie名称{name!r}对应多个域或路径")
        return matches[0]

    def __iter__(self) -> Iterator[str]:
        return iter(dict.fromkeys(cookie.name for cookie in self._all()))

    def __len__(self) -> int:
        return len(set(cookie.name for cookie in self._all()))

    def set(
        self,
        name: str,
        value: str,
        *,
        url: str | None = None,
        domain: str | None = None,
        path: str = "/",
        secure: bool = False,
        http_only: bool = False,
    ) -> None:
        name, value = _validated_cookie_pair(str(name), str(value))
        path = _validated_cookie_attribute("path", str(path))
        if domain:
            domain = _validated_cookie_attribute("domain", str(domain))
        if url is None:
            if not domain:
                raise TypeError("Cookies.set必须提供url或domain")
            host = domain.lstrip(".")
            url = f"{'https' if secure else 'http'}://{host}{path}"
        attributes = [f"{name}={value}", f"Path={path}"]
        if domain:
            attributes.append(f"Domain={domain}")
        if secure:
            attributes.append("Secure")
        if http_only:
            attributes.append("HttpOnly")
        self._native.set_cookie("; ".join(attributes), url)

    def get_all(self) -> list[Cookie]:
        return self._all()

    def get_dict(self) -> dict[str, str]:
        return {cookie.name: cookie.value for cookie in self._all()}

    def clear(self) -> None:
        self._native.clear_cookies()

    def delete(self, name: str, *, url: str) -> None:
        self._native.remove_cookie(name, url)

    def update(self, cookies: Any) -> None:
        if isinstance(cookies, ResponseCookies):
            for value in cookies.raw:
                self._native.set_cookie(value, cookies.url)
            return
        raise TypeError("Cookies.update仅支持Response.cookies；普通Cookie请使用set并明确url")


class ResponseCookies(Mapping[str, str]):
    """当前响应 Set-Cookie 的独立快照，不依赖 Session Cookie Jar。"""

    def __init__(self, values: Sequence[str], url: str) -> None:
        self.raw = list(values)
        self.url = url
        parsed: dict[str, str] = {}
        for value in self.raw:
            cookie = SimpleCookie()
            try:
                cookie.load(value)
            except Exception:
                continue
            parsed.update((name, morsel.value) for name, morsel in cookie.items())
        self._values = parsed

    def __getitem__(self, name: str) -> str:
        return self._values[name]

    def __iter__(self) -> Iterator[str]:
        return iter(self._values)

    def __len__(self) -> int:
        return len(self._values)

    def get_dict(self) -> dict[str, str]:
        return dict(self._values)


CookieTypes = Cookies | CookieJar | dict[str, str] | list[tuple[str, str]]


def _header_items(headers: HeaderInput | None) -> list[tuple[str, str]]:
    if headers is None:
        return []
    if isinstance(headers, Mapping):
        headers = headers.items()
    try:
        return [(str(name), str(value)) for name, value in headers]
    except (TypeError, ValueError) as error:
        raise TypeError("headers必须是Mapping或(name, value)序列") from error


def _is_form_sequence(value: Any) -> bool:
    """识别curl_cffi接受的有序二元组表单，保留重复键与输入顺序。"""
    if not isinstance(value, (list, tuple)):
        return False
    return all(isinstance(item, (list, tuple)) and len(item) == 2 for item in value)


def _append_query(url: str, values: Mapping[str, Any]) -> str:
    encoded = urlencode(values, doseq=True)
    parts = urlsplit(url)
    query = f"{parts.query}&{encoded}" if parts.query else encoded
    return urlunsplit((parts.scheme, parts.netloc, parts.path, query, parts.fragment))


def _cookie_items(cookies: Any, url: str) -> list[tuple[str, str]]:
    if isinstance(cookies, Cookies):
        return list(cookies._native.get_cookie_pairs(url))
    if isinstance(cookies, Mapping):
        return [_validated_cookie_pair(str(name), str(value)) for name, value in cookies.items()]
    if isinstance(cookies, list):
        return [_validated_cookie_pair(str(name), str(value)) for name, value in cookies]
    if isinstance(cookies, CookieJar):
        request = Request(url)
        cookies.add_cookie_header(request)
        value = request.get_header("Cookie", "")
        return [tuple(item.strip().split("=", 1)) for item in value.split(";") if "=" in item]
    raise TypeError("cookies必须是Cookies、CookieJar、dict或list[tuple[str, str]]")


def _validated_cookie_attribute(name: str, value: str) -> str:
    # These fields are interpolated into a Set-Cookie header, not URL-encoded.
    if any(character == ";" or ord(character) < 32 or ord(character) == 127 for character in value):
        raise ValueError(f"Cookie {name}不能包含分号或控制字符")
    return value


def _validated_cookie_pair(name: str, value: str) -> tuple[str, str]:
    if not name or any(
        not (character.isascii() and (character.isalnum() or character in "!#$%&'*+-.^_`|~"))
        for character in name
    ):
        raise ValueError("Cookie名称必须是非空HTTP token")
    return name, _validated_cookie_attribute("value", value)


def _validate_timeout(name: str, value: float | None, *, optional: bool = False) -> None:
    if value is None and optional:
        return
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
        raise ValueError(f"{name}必须是有限正数")


def _status_reason(status_code: int) -> str:
    try:
        return HTTPStatus(status_code).phrase
    except ValueError:
        return ""


def _normalize_http_version(value: str) -> str:
    if not isinstance(value, str):
        raise TypeError("http_version必须是字符串")
    aliases = {
        "auto": "auto",
        "default": "auto",
        "http1": "http1.1",
        "http1.1": "http1.1",
        "http/1.1": "http1.1",
        "http2": "http2",
        "h2": "http2",
        "http/2": "http2",
        "http3": "http3",
        "h3": "http3",
        "http/3": "http3",
        "v3": "http3",
        "v3only": "http3",
    }
    try:
        return aliases[value.lower()]
    except KeyError as error:
        raise ValueError("http_version必须是auto、http1.1、http2或http3") from error

class Response:
    def __init__(
        self,
        *,
        status_code: int,
        headers: Sequence[tuple[str, str]],
        url: str,
        fingerprint_id: str,
        impersonate: str | os.PathLike[str],
        http_version: str = "UNKNOWN",
        content: bytes | None = None,
        stream: Any = None,
        history: Sequence["Response"] = (),
    ) -> None:
        self.status_code = status_code
        self.headers = Headers(headers)
        self.url = url
        self.fingerprint_id = fingerprint_id
        impersonate = os.fspath(impersonate)
        self.impersonate = impersonate
        self.http_version = http_version
        self.reason = _status_reason(status_code)
        self.cookies = ResponseCookies(self.headers.get_list("set-cookie"), url)
        self._content = content
        self._stream = stream
        self.transfer_stats = None
        self._consumer_active = False
        self._async_stream = False
        self._close_task = None
        self.history = list(history)

    @classmethod
    def _from_native(cls, native: Any) -> "Response":
        response = cls.__new__(cls)
        response.status_code = native.status_code
        response._native_response = native
        response._headers = None
        response.url = native.url
        response.fingerprint_id = native.fingerprint_id
        response.impersonate = native.impersonate
        response.http_version = native.http_version
        response.reason = _status_reason(response.status_code)
        response.transfer_stats = native.transfer_stats
        response._content = native.content
        response._stream = None
        response._consumer_active = False
        response._async_stream = False
        response._close_task = None
        response._history = None
        response.cookies = ResponseCookies(response.headers.get_list("set-cookie"), response.url)
        return response

    @property
    def headers(self) -> Headers:
        if self._headers is None:
            self._headers = Headers._from_native(self._native_response.headers)
        return self._headers

    @headers.setter
    def headers(self, value: Headers) -> None:
        self._headers = value

    @property
    def history(self) -> list["Response"]:
        if self._history is None:
            self._history = _build_history(
                self._native_response.history(),
                self.fingerprint_id,
                self.impersonate,
            )
        return self._history

    @history.setter
    def history(self, value: Sequence["Response"]) -> None:
        self._history = list(value)

    @property
    def content(self) -> bytes:
        if self._content is None:
            if self._stream is None:
                raise RuntimeError("响应流已经关闭")
            if self._async_stream:
                raise RuntimeError("异步流响应不能同步读取content；请使用await response.aread()")
            if self._consumer_active:
                raise RuntimeError("同一响应同时只允许一个活动消费者")
            self._consumer_active = True
            try:
                self._content = self._stream.read()
            finally:
                self._consumer_active = False
                self.close()
        return self._content

    @property
    def text(self) -> str:
        content_type = self.headers.get("content-type", "")
        charset = self._charset(content_type)
        return self.content.decode(charset, errors="replace")

    @staticmethod
    def _charset(content_type: str) -> str:
        charset = "utf-8"
        for item in content_type.split(";")[1:]:
            name, separator, value = item.partition("=")
            if separator and name.strip().casefold() == "charset":
                charset = value.strip().strip('"\'') or "utf-8"
                break
        try:
            "".encode(charset)
        except LookupError:
            return "utf-8"
        return charset

    async def aread(self) -> bytes:
        if self._content is not None:
            return self._content
        if self._stream is None:
            raise RuntimeError("响应流已经关闭")
        if self._consumer_active:
            raise RuntimeError("同一响应同时只允许一个活动消费者")
        self._consumer_active = True
        stream = self._stream
        chunks = []
        try:
            while chunk := await stream.read_async(64 * 1024):
                chunks.append(chunk)
            self._content = b"".join(chunks)
            return self._content
        finally:
            self._consumer_active = False
            await self.aclose()

    async def atext(self) -> str:
        content = await self.aread()
        return content.decode(self._charset(self.headers.get("content-type", "")), errors="replace")

    async def ajson(self) -> Any:
        return json_module.loads(await self.aread())

    def json(self) -> Any:
        return json_module.loads(self.content)

    @property
    def ok(self) -> bool:
        return 200 <= self.status_code < 400

    def raise_for_status(self) -> None:
        if not self.ok:
            raise RuntimeError(f"HTTP {self.status_code}: {self.url}")

    def iter_content(self, chunk_size: int = 64 * 1024) -> Iterator[bytes]:
        if chunk_size <= 0:
            raise ValueError("chunk_size必须大于0")
        if self._content is not None:
            for offset in range(0, len(self._content), chunk_size):
                yield self._content[offset:offset + chunk_size]
            return
        if self._stream is None:
            raise RuntimeError("响应流已经关闭")
        if self._async_stream:
            raise RuntimeError("异步流响应请使用response.aiter_content()")
        if self._consumer_active:
            raise RuntimeError("同一响应同时只允许一个活动消费者")
        self._consumer_active = True
        stream = self._stream
        try:
            while chunk := stream.read(chunk_size):
                yield chunk
        finally:
            self._consumer_active = False
            self.close()

    async def aiter_content(self, chunk_size: int = 64 * 1024):
        if chunk_size <= 0:
            raise ValueError("chunk_size必须大于0")
        if self._content is not None:
            for chunk in self.iter_content(chunk_size):
                yield chunk
            return
        if self._stream is None:
            raise RuntimeError("响应流已经关闭")
        if self._consumer_active:
            raise RuntimeError("同一响应同时只允许一个活动消费者")
        self._consumer_active = True
        stream = self._stream
        try:
            while chunk := await stream.read_async(chunk_size):
                yield chunk
        finally:
            self._consumer_active = False
            await self.aclose()

    def close(self) -> None:
        if self._stream is not None:
            if self._async_stream:
                raise RuntimeError("异步流响应请使用await response.aclose()")
            stream = self._stream
            self._stream = None
            stream.close()

    async def aclose(self) -> None:
        if self._close_task is None and self._stream is not None:
            # Keep the native close alive if the caller is cancelled; later callers
            # must await that same operation rather than return before it releases
            # the connection permit.
            self._close_task = asyncio.ensure_future(self._stream.close_async())
            self._close_task.add_done_callback(self._consume_close_result)
            self._stream = None
        if self._close_task is not None:
            await asyncio.shield(self._close_task)

    @staticmethod
    def _consume_close_result(task: asyncio.Future) -> None:
        if not task.cancelled():
            task.exception()

    async def __aenter__(self) -> "Response":
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.aclose()

    def __enter__(self) -> "Response":
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()


@dataclass(frozen=True, slots=True)
class WebSocketMessage:
    type: str
    data: str | bytes | None = None
    code: int | None = None
    reason: str | None = None

    @classmethod
    def _from_native(cls, value: Any) -> "WebSocketMessage | None":
        if value is None:
            return None
        message_type, text, binary, code, reason = value
        if message_type == "text":
            data = text
        elif message_type in {"binary", "ping", "pong"}:
            data = bytes(binary)
        else:
            data = None
        return cls(message_type, data, code, reason)


class WebSocket:
    def __init__(self, native: Any) -> None:
        self._native = native
        self.url = native.url
        self.protocol = native.protocol
        self.fingerprint_id = native.fingerprint_id
        self.impersonate = native.impersonate

    @property
    def closed(self) -> bool:
        return self._native.closed

    def send(self, data: str | bytes | bytearray | memoryview) -> None:
        if isinstance(data, str):
            self._native.send_text(data)
        elif isinstance(data, (bytes, bytearray, memoryview)):
            self._native.send_bytes(bytes(data))
        else:
            raise TypeError("WebSocket消息必须是str或bytes-like")

    def send_text(self, data: str) -> None:
        self._native.send_text(data)

    def send_bytes(self, data: bytes | bytearray | memoryview) -> None:
        self._native.send_bytes(bytes(data))

    def ping(self, data: bytes | bytearray | memoryview = b"") -> None:
        self._native.ping(bytes(data))

    def pong(self, data: bytes | bytearray | memoryview = b"") -> None:
        self._native.pong(bytes(data))

    def recv(self) -> WebSocketMessage | None:
        return WebSocketMessage._from_native(self._native.recv())

    def close(self, code: int = 1000, reason: str = "") -> None:
        self._native.close(code, reason)

    def __enter__(self) -> "WebSocket":
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()

    def __iter__(self) -> "WebSocket":
        return self

    def __next__(self) -> WebSocketMessage:
        message = self.recv()
        if message is None:
            raise StopIteration
        return message


class AsyncWebSocket:
    def __init__(self, native: Any) -> None:
        self._native = native
        self._close_task = None
        self.url = native.url
        self.protocol = native.protocol
        self.fingerprint_id = native.fingerprint_id
        self.impersonate = native.impersonate

    @property
    def closed(self) -> bool:
        return self._native.closed

    async def send(self, data: str | bytes | bytearray | memoryview) -> None:
        if isinstance(data, str):
            await self._native.send_text_async(data)
        elif isinstance(data, (bytes, bytearray, memoryview)):
            await self._native.send_bytes_async(bytes(data))
        else:
            raise TypeError("WebSocket消息必须是str或bytes-like")

    async def send_text(self, data: str) -> None:
        await self._native.send_text_async(data)

    async def send_bytes(self, data: bytes | bytearray | memoryview) -> None:
        await self._native.send_bytes_async(bytes(data))

    async def ping(self, data: bytes | bytearray | memoryview = b"") -> None:
        await self._native.ping_async(bytes(data))

    async def pong(self, data: bytes | bytearray | memoryview = b"") -> None:
        await self._native.pong_async(bytes(data))

    async def recv(self) -> WebSocketMessage | None:
        return WebSocketMessage._from_native(await self._native.recv_async())

    async def close(self, code: int = 1000, reason: str = "") -> None:
        if self._close_task is None:
            self._close_task = asyncio.ensure_future(self._native.close_async(code, reason))
            self._close_task.add_done_callback(self._consume_close_result)
        # 取消调用方等待不会取消已经开始的底层关闭。
        await asyncio.shield(self._close_task)

    @staticmethod
    def _consume_close_result(task: asyncio.Future) -> None:
        if not task.cancelled():
            task.exception()

    async def __aenter__(self) -> "AsyncWebSocket":
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()

    def __aiter__(self) -> "AsyncWebSocket":
        return self

    async def __anext__(self) -> WebSocketMessage:
        message = await self.recv()
        if message is None:
            raise StopAsyncIteration
        return message

class Session:
    def __init__(
        self,
        *,
        impersonate: FingerprintInput = "chrome152",
        fingerprint_rotation: bool = True,
        headers: HeaderInput | None = None,
        proxy: str | None = None,
        proxies: ProxyInput | None = None,
        verify: bool = True,
        timeout: float = 30,
        connect_timeout: float | None = None,
        read_timeout: float | None = None,
        fingerprints_path: str | os.PathLike[str] | None = None,
        max_connections: int = 50,
        happy_eyeballs_timeout: float | None = 0.3,
        resolve: Mapping[str, str | Sequence[str]] | None = None,
        dns_servers: Sequence[str] | None = None,
        dns_timeout: float | None = 5.0,
        fingerprint_pool: bool = True,
        fingerprint_pool_size: int = 100,
        max_cached_origins: int = 4,
        max_response_bytes: int = 64 * 1024 * 1024,
        max_websocket_message_bytes: int = 16 * 1024 * 1024,
        cookie_store: bool = True,
        http_version: str = "http2",
        max_clients: int | None = None,
    ) -> None:
        if max_clients is not None:
            if max_connections != 50 and max_connections != max_clients:
                raise TypeError("max_connections和max_clients不能设置为不同值")
            max_connections = max_clients
        _validate_timeout("timeout", timeout)
        _validate_timeout("connect_timeout", connect_timeout, optional=True)
        _validate_timeout("read_timeout", read_timeout, optional=True)
        _validate_timeout("happy_eyeballs_timeout", happy_eyeballs_timeout, optional=True)
        _validate_timeout("dns_timeout", dns_timeout, optional=True)
        if isinstance(max_connections, bool) or not isinstance(max_connections, int) or max_connections <= 0:
            raise ValueError("max_connections必须是正整数")
        if max_connections > (1 << 61) - 1:
            raise ValueError("max_connections超过Tokio Semaphore上限")
        if not isinstance(fingerprint_pool, bool):
            raise TypeError("fingerprint_pool必须是bool")
        if (
            isinstance(fingerprint_pool_size, bool)
            or not isinstance(fingerprint_pool_size, int)
            or fingerprint_pool_size <= 0
        ):
            raise ValueError("fingerprint_pool_size必须是正整数")
        if (
            isinstance(max_cached_origins, bool)
            or not isinstance(max_cached_origins, int)
            or max_cached_origins < 0
        ):
            raise ValueError("max_cached_origins必须是非负整数")
        if (
            isinstance(max_response_bytes, bool)
            or not isinstance(max_response_bytes, int)
            or max_response_bytes <= 0
        ):
            raise ValueError("max_response_bytes必须是正整数")
        if (
            isinstance(max_websocket_message_bytes, bool)
            or not isinstance(max_websocket_message_bytes, int)
            or max_websocket_message_bytes <= 0
        ):
            raise ValueError("max_websocket_message_bytes必须是正整数")
        if not isinstance(cookie_store, bool):
            raise TypeError("cookie_store必须是bool")
        http_version = _normalize_http_version(http_version)
        if proxy is not None and proxies is not None:
            raise TypeError("proxy和proxies不能同时传入")
        if proxies is not None and not isinstance(proxies, Mapping):
            raise TypeError("proxies必须是Mapping或None")
        if resolve is not None and not isinstance(resolve, Mapping):
            raise TypeError("resolve必须是Mapping或None")
        native_resolve = []
        for domain, addresses in (resolve or {}).items():
            if isinstance(addresses, str):
                addresses = [addresses]
            elif not isinstance(addresses, Sequence):
                raise TypeError("resolve中的IP必须是字符串或字符串序列")
            native_resolve.append((str(domain), [str(address) for address in addresses]))
        if isinstance(dns_servers, str):
            raise TypeError("dns_servers必须是字符串序列，不能是单个字符串")
        native_dns_servers = [str(server) for server in (dns_servers or [])]
        fingerprints_json = None
        if isinstance(impersonate, Mapping) or (
            isinstance(impersonate, Sequence)
            and not isinstance(impersonate, (str, bytes, bytearray))
        ):
            if fingerprints_path is not None:
                raise TypeError("impersonate使用捕获结果对象时不能同时传fingerprints_path")
            try:
                fingerprints_json = json_module.dumps(
                    impersonate,
                    ensure_ascii=False,
                    separators=(",", ":"),
                )
            except (TypeError, ValueError) as error:
                raise TypeError(f"impersonate捕获结果不能序列化为JSON: {error}") from error
            native_impersonate = ""
        else:
            native_impersonate = os.fspath(impersonate)
        self.impersonate = native_impersonate
        self.fingerprint_rotation = fingerprint_rotation
        self.headers = MutableHeaders(headers)
        self._native_headers = tuple(self.headers)
        self._default_has_content_type = any(name.lower() == "content-type" for name, _ in self.headers)
        self.timeout = timeout
        self.connect_timeout = connect_timeout
        self.read_timeout = read_timeout
        self.verify = verify
        self.proxy = proxy
        self.proxies = dict(proxies) if proxies is not None else None
        self.fingerprints_path = fingerprints_path
        self.max_connections = max_connections
        self.happy_eyeballs_timeout = happy_eyeballs_timeout
        self.resolve = dict(resolve or {})
        self.dns_servers = tuple(native_dns_servers)
        self.dns_timeout = dns_timeout
        self.fingerprint_pool = fingerprint_pool
        self.fingerprint_pool_size = fingerprint_pool_size
        self.max_cached_origins = max_cached_origins
        self.max_response_bytes = max_response_bytes
        self.max_websocket_message_bytes = max_websocket_message_bytes
        self.cookie_store = cookie_store
        self.http_version = http_version
        self._native = NativeSession(
            native_impersonate,
            fingerprint_rotation,
            proxy,
            verify,
            connect_timeout,
            None if fingerprints_path is None else os.fspath(fingerprints_path),
            self.headers,
            max_connections,
            happy_eyeballs_timeout,
            native_resolve,
            native_dns_servers,
            dns_timeout,
            fingerprint_pool,
            fingerprint_pool_size,
            max_cached_origins,
            max_response_bytes,
            max_websocket_message_bytes,
            cookie_store,
            fingerprints_json,
        )
        if fingerprints_json is not None:
            self.impersonate = self._native.impersonate
        self.cookies = Cookies(self._native)

    @property
    def fingerprint_count(self) -> int:
        return self._native.fingerprint_count

    @property
    def fingerprint_pool_count(self) -> int:
        return self._native.fingerprint_pool_count

    @property
    def cached_origin_count(self) -> int:
        return self._native.cached_origin_count

    @property
    def request_dns_client_count(self) -> int:
        return self._native.request_dns_client_count

    def set_proxy(self, proxy: str | None) -> None:
        self._native.set_proxy(proxy)
        self.proxy = proxy
        self.proxies = None

    def _prepare_request(
        self,
        method: str,
        url: str,
        *,
        params: Mapping[str, Any] | None,
        headers: HeaderInput | None,
        cookies: CookieTypes | None,
        data: bytes | str | Mapping[str, Any] | None,
        json: Any,
        timeout: float | None,
        read_timeout: float | None,
        proxy: str | None | object,
        proxies: ProxyInput | None,
        max_redirects: int,
        http_version: str | None,
    ):
        request_timeout = self.timeout if timeout is None else timeout
        _validate_timeout("timeout", request_timeout)
        request_read_timeout = self.read_timeout if read_timeout is None else read_timeout
        _validate_timeout("read_timeout", request_read_timeout, optional=True)
        if isinstance(max_redirects, bool) or not isinstance(max_redirects, int) or max_redirects < 0:
            raise ValueError("max_redirects必须是非负整数")
        if params:
            url = _append_query(url, params)

        # 保持session.headers可变的兼容语义；仅在实际变更后同步到原生默认Header快照。
        current_headers = tuple(self.headers)
        if current_headers != self._native_headers:
            self._native.set_default_headers(self.headers)
            self._native_headers = current_headers
            self._default_has_content_type = any(name.lower() == "content-type" for name, _ in self.headers)
        merged_headers = _header_items(headers)
        request_cookies = None if cookies is None else _cookie_items(cookies, url)
        if proxy is not _UNSET and proxies is not None:
            raise TypeError("proxy和proxies不能同时传入")
        if proxies is not None and not isinstance(proxies, Mapping):
            raise TypeError("proxies必须是Mapping或None")
        if proxy is not _UNSET:
            proxy_override = True
            request_proxy = proxy
        elif proxies is not None:
            proxy_override = True
            request_proxy = self._native.select_proxy(url, list(proxies.items()))
        elif self.proxies is not None:
            proxy_override = True
            request_proxy = self._native.select_proxy(url, list(self.proxies.items()))
        else:
            proxy_override = False
            request_proxy = None
        request_http_version = (
            self.http_version if http_version is None else _normalize_http_version(http_version)
        )
        if data is not None and json is not None:
            raise ValueError("data和json不能同时传入")
        if json is not None:
            body = self._native.encode_json(json)
            if body is None:
                body = json_module.dumps(
                    json,
                    ensure_ascii=False,
                    separators=(",", ":"),
                    allow_nan=False,
                ).encode()
            if not self._default_has_content_type and not any(name.lower() == "content-type" for name, _ in merged_headers):
                merged_headers.append(("content-type", "application/json"))
        elif isinstance(data, Mapping) or _is_form_sequence(data):
            body = urlencode(data, doseq=True).encode("ascii")
            if not self._default_has_content_type and not any(name.lower() == "content-type" for name, _ in merged_headers):
                merged_headers.append(("content-type", "application/x-www-form-urlencoded"))
        elif isinstance(data, str):
            body = data.encode()
        else:
            body = data
        return (
            method.upper(),
            url,
            merged_headers,
            body,
            request_timeout,
            request_read_timeout,
            proxy_override,
            request_proxy,
            request_cookies,
            request_http_version,
        )

    def _prepare_websocket(
        self,
        url: str,
        *,
        headers: HeaderInput | None,
        cookies: CookieTypes | None,
        protocols: Sequence[str] | None,
        version: str,
        timeout: float | None,
        proxy: str | None | object,
        proxies: ProxyInput | None,
    ):
        request_timeout = self.timeout if timeout is None else timeout
        _validate_timeout("timeout", request_timeout)
        normalized_version = version.lower()
        if normalized_version not in {"http1", "http1.1", "http/1.1", "http2", "h2", "http/2"}:
            raise ValueError("version必须是http1或http2")
        if not url.startswith(("ws://", "wss://")):
            raise ValueError("WebSocket URL必须使用ws://或wss://")
        if protocols is not None and (
            isinstance(protocols, (str, bytes))
            or not isinstance(protocols, Sequence)
            or any(not isinstance(protocol, str) for protocol in protocols)
        ):
            raise TypeError("protocols必须是字符串序列，不能是单个字符串")

        current_headers = tuple(self.headers)
        if current_headers != self._native_headers:
            self._native.set_default_headers(self.headers)
            self._native_headers = current_headers
            self._default_has_content_type = any(name.lower() == "content-type" for name, _ in self.headers)
        merged_headers = _header_items(headers)
        cookie_url = "https://" + url[6:] if url.startswith("wss://") else "http://" + url[5:]
        request_cookies = None if cookies is None else _cookie_items(cookies, cookie_url)
        if proxy is not _UNSET and proxies is not None:
            raise TypeError("proxy和proxies不能同时传入")
        if proxies is not None and not isinstance(proxies, Mapping):
            raise TypeError("proxies必须是Mapping或None")
        if proxy is not _UNSET:
            proxy_override = True
            request_proxy = proxy
        elif proxies is not None:
            proxy_override = True
            request_proxy = self._native.select_proxy(cookie_url, list(proxies.items()))
        elif self.proxies is not None:
            proxy_override = True
            request_proxy = self._native.select_proxy(cookie_url, list(self.proxies.items()))
        else:
            proxy_override = False
            request_proxy = None
        return (
            url,
            merged_headers,
            list(protocols or ()),
            normalized_version,
            request_timeout,
            proxy_override,
            request_proxy,
            request_cookies,
        )

    def websocket(
        self,
        url: str,
        *,
        headers: HeaderInput | None = None,
        cookies: CookieTypes | None = None,
        protocols: Sequence[str] | None = None,
        version: str = "http1",
        timeout: float | None = None,
        proxy: str | None | object = _UNSET,
        proxies: ProxyInput | None = None,
    ) -> WebSocket:
        prepared = self._prepare_websocket(
            url,
            headers=headers,
            cookies=cookies,
            protocols=protocols,
            version=version,
            timeout=timeout,
            proxy=proxy,
            proxies=proxies,
        )
        return WebSocket(self._native.websocket(*prepared))

    def request(
        self,
        method: str,
        url: str,
        *,
        params: Mapping[str, Any] | None = None,
        headers: HeaderInput | None = None,
        cookies: CookieTypes | None = None,
        data: bytes | str | Mapping[str, Any] | None = None,
        json: Any = None,
        files: FileInput | None = None,
        timeout: float | None = None,
        read_timeout: float | None = None,
        stream: bool = False,
        proxy: str | None | object = _UNSET,
        proxies: ProxyInput | None = None,
        allow_redirects: bool = True,
        max_redirects: int = 10,
        transfer_stats: bool = False,
        dns_servers: Sequence[str] | None = None,
        dns_timeout: float | None = None,
        http_version: str | None = None,
        discard_cookies: bool = False,
        impersonate: FingerprintInput | None = None,
        verify: bool | None = None,
    ) -> Response:
        if not isinstance(discard_cookies, bool):
            raise TypeError("discard_cookies必须是bool")
        if impersonate is not None and os.fspath(impersonate) != self.impersonate:
            raise ValueError("请求级impersonate必须与Session指纹一致")
        if verify is not None and verify != self.verify:
            raise ValueError("请求级verify必须与Session配置一致")
        if files is not None and json is not None:
            raise ValueError("files不能和json同时使用")
        if transfer_stats and stream:
            raise ValueError("transfer_stats暂不支持stream=True")
        if transfer_stats and files is not None:
            raise ValueError("transfer_stats暂不支持multipart请求")
        if isinstance(dns_servers, str):
            raise TypeError("dns_servers必须是字符串序列，不能是单个字符串")
        if dns_timeout is not None and dns_servers is None:
            raise ValueError("单请求dns_timeout必须和dns_servers同时传入")
        if dns_timeout is not None:
            _validate_timeout("dns_timeout", dns_timeout)
        dns_override = dns_servers is not None
        native_dns_servers = [str(server) for server in (dns_servers or [])]
        request_dns_timeout = self.dns_timeout if dns_timeout is None else dns_timeout
        prepared = self._prepare_request(
            method,
            url,
            params=params,
            headers=headers,
            cookies=cookies,
            data=None if files is not None else data,
            json=None if files is not None else json,
            timeout=timeout,
            read_timeout=read_timeout,
            proxy=proxy,
            proxies=proxies,
            max_redirects=max_redirects,
            http_version=http_version,
        )
        (
            method,
            url,
            merged_headers,
            body,
            request_timeout,
            request_read_timeout,
            proxy_override,
            request_proxy,
            request_cookies,
            request_http_version,
        ) = prepared
        if files is not None:
            if stream:
                raise ValueError("multipart响应暂不支持stream=True")
            fields = []
            if isinstance(data, Mapping):
                fields = [(str(name), str(value)) for name, value in data.items()]
            elif data is not None:
                raise TypeError("multipart的data必须是Mapping或None")
            native_files = []
            for name, file_value in files.items():
                if isinstance(file_value, tuple):
                    filename, path, content_type = file_value
                    native_files.append((name, os.fspath(path), filename, content_type))
                else:
                    native_files.append((name, os.fspath(file_value), None, None))
            result = self._native.request_multipart(
                method.upper(),
                url,
                merged_headers,
                fields,
                native_files,
                request_timeout,
                request_read_timeout,
                proxy_override,
                request_proxy,
                request_cookies,
                request_http_version,
                allow_redirects,
                max_redirects,
                dns_override,
                native_dns_servers,
                request_dns_timeout,
                discard_cookies,
            )
            return Response._from_native(result)
        if stream:
            native = self._native.request_stream(
                method,
                url,
                merged_headers,
                body,
                request_timeout,
                request_read_timeout,
                proxy_override,
                request_proxy,
                request_cookies,
                request_http_version,
                allow_redirects,
                max_redirects,
                dns_override,
                native_dns_servers,
                request_dns_timeout,
                discard_cookies,
            )
            history = _build_history(
                native.history,
                native.fingerprint_id,
                native.impersonate,
            )
            return Response(
                status_code=native.status_code,
                headers=native.headers,
                url=native.url,
                fingerprint_id=native.fingerprint_id,
                impersonate=native.impersonate,
                stream=native,
                http_version=native.http_version,
                history=history,
            )

        result = self._native.request(
            method,
            url,
            merged_headers,
            body,
            request_timeout,
            request_read_timeout,
            proxy_override,
            request_proxy,
            request_cookies,
            request_http_version,
            allow_redirects,
            max_redirects,
            transfer_stats,
            dns_override,
            native_dns_servers,
            request_dns_timeout,
            discard_cookies,
        )
        return Response._from_native(result)

    def get(self, url: str, **kwargs: Any) -> Response:
        return self.request("GET", url, **kwargs)

    def post(self, url: str, **kwargs: Any) -> Response:
        return self.request("POST", url, **kwargs)

    def put(self, url: str, **kwargs: Any) -> Response:
        return self.request("PUT", url, **kwargs)

    def patch(self, url: str, **kwargs: Any) -> Response:
        return self.request("PATCH", url, **kwargs)

    def delete(self, url: str, **kwargs: Any) -> Response:
        return self.request("DELETE", url, **kwargs)

    def head(self, url: str, **kwargs: Any) -> Response:
        return self.request("HEAD", url, **kwargs)

    def options(self, url: str, **kwargs: Any) -> Response:
        return self.request("OPTIONS", url, **kwargs)

    def close(self) -> None:
        self._native.close()

    def __enter__(self) -> "Session":
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()


class AsyncSession:
    def __init__(
        self,
        *,
        max_connections: int = 50,
        max_clients: int | None = None,
        **kwargs: Any,
    ) -> None:
        if max_clients is not None:
            if max_connections != 50 and max_connections != max_clients:
                raise TypeError("max_connections和max_clients不能设置为不同值")
            max_connections = max_clients
        self._session = Session(max_connections=max_connections, **kwargs)
        self.max_connections = max_connections
        self.max_clients = max_connections
        self.headers = self._session.headers
        self.cookies = self._session.cookies

    @property
    def fingerprint_count(self) -> int:
        return self._session.fingerprint_count

    @property
    def fingerprint_rotation(self) -> bool:
        return self._session.fingerprint_rotation

    @property
    def fingerprint_pool_count(self) -> int:
        return self._session.fingerprint_pool_count

    @property
    def cached_origin_count(self) -> int:
        return self._session.cached_origin_count

    @property
    def request_dns_client_count(self) -> int:
        return self._session.request_dns_client_count

    async def request(self, method: str, url: str, **kwargs: Any) -> Response:
        allow_redirects = kwargs.pop("allow_redirects", True)
        max_redirects = kwargs.pop("max_redirects", 10)
        files = kwargs.pop("files", None)
        stream = kwargs.pop("stream", False)
        transfer_stats = kwargs.pop("transfer_stats", False)
        dns_servers = kwargs.pop("dns_servers", None)
        dns_timeout = kwargs.pop("dns_timeout", None)
        http_version = kwargs.pop("http_version", None)
        discard_cookies = kwargs.pop("discard_cookies", False)
        impersonate = kwargs.pop("impersonate", None)
        verify = kwargs.pop("verify", None)
        if not isinstance(discard_cookies, bool):
            raise TypeError("discard_cookies必须是bool")
        if impersonate is not None and os.fspath(impersonate) != self._session.impersonate:
            raise ValueError("请求级impersonate必须与Session指纹一致")
        if verify is not None and verify != self._session.verify:
            raise ValueError("请求级verify必须与Session配置一致")
        data = kwargs.pop("data", None)
        json = kwargs.pop("json", None)
        if files is not None and json is not None:
            raise ValueError("files不能和json同时使用")
        if transfer_stats and stream:
            raise ValueError("transfer_stats暂不支持stream=True")
        if transfer_stats and files is not None:
            raise ValueError("transfer_stats暂不支持multipart请求")
        if isinstance(dns_servers, str):
            raise TypeError("dns_servers必须是字符串序列，不能是单个字符串")
        if dns_timeout is not None and dns_servers is None:
            raise ValueError("单请求dns_timeout必须和dns_servers同时传入")
        if dns_timeout is not None:
            _validate_timeout("dns_timeout", dns_timeout)
        dns_override = dns_servers is not None
        native_dns_servers = [str(server) for server in (dns_servers or [])]
        request_dns_timeout = self._session.dns_timeout if dns_timeout is None else dns_timeout
        prepared = self._session._prepare_request(
            method,
            url,
            params=kwargs.pop("params", None),
            headers=kwargs.pop("headers", None),
            cookies=kwargs.pop("cookies", None),
            data=None if files is not None else data,
            json=None if files is not None else json,
            timeout=kwargs.pop("timeout", None),
            read_timeout=kwargs.pop("read_timeout", None),
            proxy=kwargs.pop("proxy", _UNSET),
            proxies=kwargs.pop("proxies", None),
            max_redirects=max_redirects,
            http_version=http_version,
        )
        if kwargs:
            unexpected = next(iter(kwargs))
            raise TypeError(f"request() got an unexpected keyword argument {unexpected!r}")
        request_http_version = prepared[-1]
        if files is not None:
            if stream:
                raise ValueError("multipart响应暂不支持stream=True")
            fields = []
            if isinstance(data, Mapping):
                fields = [(str(name), str(value)) for name, value in data.items()]
            elif data is not None:
                raise TypeError("multipart的data必须是Mapping或None")
            native_files = []
            for name, file_value in files.items():
                if isinstance(file_value, tuple):
                    filename, path, content_type = file_value
                    native_files.append((name, os.fspath(path), filename, content_type))
                else:
                    native_files.append((name, os.fspath(file_value), None, None))
            (
                method,
                url,
                headers,
                _body,
                timeout,
                read_timeout,
                proxy_override,
                proxy,
                request_cookies,
                request_http_version,
            ) = prepared
            result = await self._session._native.request_multipart_async(
                method,
                url,
                headers,
                fields,
                native_files,
                timeout,
                read_timeout,
                proxy_override,
                proxy,
                request_cookies,
                request_http_version,
                allow_redirects,
                max_redirects,
                dns_override,
                native_dns_servers,
                request_dns_timeout,
                discard_cookies,
            )
            return _response_from_native(result)
        if stream:
            native = await self._session._native.request_stream_async(
                *prepared,
                allow_redirects,
                max_redirects,
                dns_override,
                native_dns_servers,
                request_dns_timeout,
                discard_cookies,
            )
            return _response_from_stream(native, async_stream=True)
        result = await self._session._native.request_async(
            *prepared,
            allow_redirects,
            max_redirects,
            transfer_stats,
            dns_override,
            native_dns_servers,
            request_dns_timeout,
            discard_cookies,
        )
        return _response_from_native(result)

    async def websocket(
        self,
        url: str,
        *,
        headers: HeaderInput | None = None,
        cookies: CookieTypes | None = None,
        protocols: Sequence[str] | None = None,
        version: str = "http1",
        timeout: float | None = None,
        proxy: str | None | object = _UNSET,
        proxies: ProxyInput | None = None,
    ) -> AsyncWebSocket:
        prepared = self._session._prepare_websocket(
            url,
            headers=headers,
            cookies=cookies,
            protocols=protocols,
            version=version,
            timeout=timeout,
            proxy=proxy,
            proxies=proxies,
        )
        native = await self._session._native.websocket_async(*prepared)
        return AsyncWebSocket(native)

    async def get(self, url: str, **kwargs: Any) -> Response:
        return await self.request("GET", url, **kwargs)

    async def post(self, url: str, **kwargs: Any) -> Response:
        return await self.request("POST", url, **kwargs)

    async def put(self, url: str, **kwargs: Any) -> Response:
        return await self.request("PUT", url, **kwargs)

    async def patch(self, url: str, **kwargs: Any) -> Response:
        return await self.request("PATCH", url, **kwargs)

    async def delete(self, url: str, **kwargs: Any) -> Response:
        return await self.request("DELETE", url, **kwargs)

    async def head(self, url: str, **kwargs: Any) -> Response:
        return await self.request("HEAD", url, **kwargs)

    async def options(self, url: str, **kwargs: Any) -> Response:
        return await self.request("OPTIONS", url, **kwargs)

    def set_proxy(self, proxy: str | None) -> None:
        self._session.set_proxy(proxy)

    async def close(self) -> None:
        self._session.close()

    async def __aenter__(self) -> "AsyncSession":
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()


def request(
    method: str,
    url: str,
    *,
    impersonate: FingerprintInput = "chrome152",
    fingerprint_rotation: bool = True,
    proxy: str | None = None,
    proxies: ProxyInput | None = None,
    verify: bool = True,
    connect_timeout: float | None = None,
    read_timeout: float | None = None,
    fingerprints_path: str | os.PathLike[str] | None = None,
    **kwargs: Any,
) -> Response:
    with Session(
        impersonate=impersonate,
        fingerprint_rotation=fingerprint_rotation,
        proxy=proxy,
        proxies=proxies,
        verify=verify,
        connect_timeout=connect_timeout,
        read_timeout=read_timeout,
        fingerprints_path=fingerprints_path,
    ) as session:
        return session.request(method, url, **kwargs)


def _build_history(
    entries: Sequence[tuple[int, str, str, Sequence[tuple[str, str]]]],
    fingerprint_id: str,
    impersonate: str,
) -> list[Response]:
    return [
        Response(
            status_code=status,
            headers=headers,
            content=b"",
            url=previous,
            fingerprint_id=fingerprint_id,
            impersonate=impersonate,
        )
        for status, previous, _target, headers in entries
    ]


def _response_from_native(result: Any) -> Response:
    return Response._from_native(result)


def _response_from_stream(native: Any, *, async_stream: bool = False) -> Response:
    response = Response(
        status_code=native.status_code,
        headers=native.headers,
        url=native.url,
        fingerprint_id=native.fingerprint_id,
        impersonate=native.impersonate,
        stream=native,
        http_version=native.http_version,
        history=_build_history(
            native.history,
            native.fingerprint_id,
            native.impersonate,
        ),
    )
    response._async_stream = async_stream
    return response


def get(url: str, **kwargs: Any) -> Response:
    return request("GET", url, **kwargs)


def post(url: str, **kwargs: Any) -> Response:
    return request("POST", url, **kwargs)


def put(url: str, **kwargs: Any) -> Response:
    return request("PUT", url, **kwargs)


def patch(url: str, **kwargs: Any) -> Response:
    return request("PATCH", url, **kwargs)


def delete(url: str, **kwargs: Any) -> Response:
    return request("DELETE", url, **kwargs)


def head(url: str, **kwargs: Any) -> Response:
    return request("HEAD", url, **kwargs)


def options(url: str, **kwargs: Any) -> Response:
    return request("OPTIONS", url, **kwargs)
