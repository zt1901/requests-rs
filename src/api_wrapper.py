import json as json_module
import math
import os
from http.cookiejar import CookieJar
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlencode

from ._native import NativeSession, available_profiles, build_response_headers


HeaderInput = Mapping[str, str] | Sequence[tuple[str, str]]
FileInput = Mapping[str, str | os.PathLike[str] | tuple[str, str | os.PathLike[str], str | None]]
ProxyInput = Mapping[str, str | None]
_UNSET = object()


class Headers(Mapping[str, str]):
    def __init__(self, values: Sequence[tuple[str, str]] = ()) -> None:
        self._native = build_response_headers(list(values))
        self.raw = self._native.raw

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
        url: str,
        domain: str | None = None,
        path: str = "/",
        secure: bool = False,
        http_only: bool = False,
    ) -> None:
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


CookieTypes = Cookies | CookieJar | dict[str, str] | list[tuple[str, str]]


def _header_items(headers: HeaderInput | None) -> list[tuple[str, str]]:
    if headers is None:
        return []
    if isinstance(headers, Mapping):
        return list(headers.items())
    return list(headers)


def _urlencoded_items(values: Mapping[str, Any] | None) -> list[tuple[str, list[str]]]:
    if not values:
        return []
    result = []
    for name, value in values.items():
        values = value if isinstance(value, (list, tuple)) else (value,)
        normalized = []
        for item in values:
            if isinstance(item, bytes):
                normalized.append(item.decode("latin1"))
            else:
                normalized.append(str(item))
        result.append((str(name), normalized))
    return result


def _cookie_items(cookies: Any) -> list[tuple[str, str]]:
    if isinstance(cookies, Mapping):
        return [(str(name), str(value)) for name, value in cookies.items()]
    if isinstance(cookies, list):
        return [(str(name), str(value)) for name, value in cookies]
    if isinstance(cookies, Cookies):
        return [(cookie.name, cookie.value) for cookie in cookies.get_all()]
    if isinstance(cookies, CookieJar):
        return [(cookie.name, cookie.value) for cookie in cookies]
    raise TypeError("cookies必须是Cookies、CookieJar、dict或list[tuple[str, str]]")


def _validate_timeout(name: str, value: float | None, *, optional: bool = False) -> None:
    if value is None and optional:
        return
    if not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
        raise ValueError(f"{name}必须是有限正数")


class Response:
    def __init__(
        self,
        *,
        status_code: int,
        headers: Sequence[tuple[str, str]],
        url: str,
        fingerprint_id: str,
        impersonate: str,
        content: bytes | None = None,
        stream: Any = None,
        history: Sequence["Response"] = (),
    ) -> None:
        self.status_code = status_code
        self.headers = Headers(headers)
        self.url = url
        self.fingerprint_id = fingerprint_id
        self.impersonate = impersonate
        self._content = content
        self._stream = stream
        self._consumer_active = False
        self.history = list(history)

    @property
    def content(self) -> bytes:
        if self._content is None:
            if self._stream is None:
                raise RuntimeError("响应流已经关闭")
            try:
                self._content = self._stream.read()
            finally:
                self.close()
        return self._content

    @property
    def text(self) -> str:
        content_type = self.headers.get("content-type", "")
        charset = "utf-8"
        if "charset=" in content_type:
            charset = content_type.split("charset=", 1)[1].split(";", 1)[0].strip()
        return self.content.decode(charset, errors="replace")

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
        if self._consumer_active:
            raise RuntimeError("同一响应同时只允许一个活动消费者")
        self._consumer_active = True
        try:
            while chunk := self._stream.read(chunk_size):
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
        try:
            while chunk := await self._stream.read_async(chunk_size):
                yield chunk
        finally:
            self._consumer_active = False
            await self.aclose()

    def close(self) -> None:
        if self._stream is not None:
            stream = self._stream
            self._stream = None
            stream.close()

    async def aclose(self) -> None:
        if self._stream is not None:
            stream = self._stream
            self._stream = None
            await stream.close_async()

    def __enter__(self) -> "Response":
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()


class Session:
    def __init__(
        self,
        *,
        impersonate: str,
        headers: HeaderInput | None = None,
        proxy: str | None = None,
        proxies: ProxyInput | None = None,
        verify: bool = True,
        timeout: float = 30,
        connect_timeout: float | None = None,
        read_timeout: float | None = None,
        fingerprints_path: str | os.PathLike[str] | None = None,
    ) -> None:
        _validate_timeout("timeout", timeout)
        _validate_timeout("connect_timeout", connect_timeout, optional=True)
        _validate_timeout("read_timeout", read_timeout, optional=True)
        if proxy is not None and proxies is not None:
            raise TypeError("proxy和proxies不能同时传入")
        if proxies is not None and not isinstance(proxies, Mapping):
            raise TypeError("proxies必须是Mapping或None")
        self.impersonate = impersonate
        self.headers = _header_items(headers)
        self._native_headers = tuple(self.headers)
        self.timeout = timeout
        self.connect_timeout = connect_timeout
        self.read_timeout = read_timeout
        self.proxy = proxy
        self.proxies = dict(proxies) if proxies is not None else None
        self.fingerprints_path = fingerprints_path
        self._native = NativeSession(
            impersonate,
            proxy,
            verify,
            connect_timeout,
            None if fingerprints_path is None else os.fspath(fingerprints_path),
            self.headers,
        )
        self.cookies = Cookies(self._native)

    @property
    def fingerprint_count(self) -> int:
        return self._native.fingerprint_count

    def set_proxy(self, proxy: str | None) -> None:
        self._native.set_proxy(proxy)
        self.proxy = proxy

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
    ):
        request_timeout = self.timeout if timeout is None else timeout
        _validate_timeout("timeout", request_timeout)
        request_read_timeout = self.read_timeout if read_timeout is None else read_timeout
        _validate_timeout("read_timeout", request_read_timeout, optional=True)
        if max_redirects < 0:
            raise ValueError("max_redirects不能小于0")
        if params:
            url = self._native.append_query(url, _urlencoded_items(params))

        # 保持session.headers可变的兼容语义；仅在实际变更后同步到原生默认Header快照。
        current_headers = tuple(self.headers)
        if current_headers != self._native_headers:
            self._native.set_default_headers(self.headers)
            self._native_headers = current_headers
        merged_headers = _header_items(headers)
        header_names = {name.lower() for name, _ in self.headers}
        header_names.update(name.lower() for name, _ in merged_headers)
        request_cookies = None if cookies is None else _cookie_items(cookies)
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
        if json is not None:
            body = self._native.encode_json(json)
            if body is None:
                body = json_module.dumps(json, ensure_ascii=False, separators=(",", ":")).encode()
            if "content-type" not in header_names:
                merged_headers.append(("content-type", "application/json"))
        elif isinstance(data, Mapping):
            body = self._native.encode_form(_urlencoded_items(data))
            if "content-type" not in header_names:
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
        )

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
    ) -> Response:
        if files is not None and json is not None:
            raise ValueError("files不能和json同时使用")
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
                allow_redirects,
                max_redirects,
            )
            status_code, response_headers, content, fingerprint_id, profile, final_url, native_history = result
            return Response(
                status_code=status_code,
                headers=response_headers,
                content=content,
                url=final_url,
                fingerprint_id=fingerprint_id,
                impersonate=profile,
                history=_build_history(native_history, fingerprint_id, profile),
            )
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
                allow_redirects,
                max_redirects,
            )
            history = _build_history(native.history, native.fingerprint_id, native.impersonate)
            return Response(
                status_code=native.status_code,
                headers=native.headers,
                url=native.url,
                fingerprint_id=native.fingerprint_id,
                impersonate=native.impersonate,
                stream=native,
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
            allow_redirects,
            max_redirects,
        )
        status_code, response_headers, content, fingerprint_id, profile, final_url, native_history = result
        return Response(
            status_code=status_code,
            headers=response_headers,
            content=content,
            url=final_url,
            fingerprint_id=fingerprint_id,
            impersonate=profile,
            history=_build_history(native_history, fingerprint_id, profile),
        )

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
    def __init__(self, **kwargs: Any) -> None:
        self._session = Session(**kwargs)
        self.cookies = self._session.cookies

    @property
    def fingerprint_count(self) -> int:
        return self._session.fingerprint_count

    async def request(self, method: str, url: str, **kwargs: Any) -> Response:
        allow_redirects = kwargs.pop("allow_redirects", True)
        max_redirects = kwargs.pop("max_redirects", 10)
        files = kwargs.pop("files", None)
        stream = kwargs.pop("stream", False)
        data = kwargs.pop("data", None)
        json = kwargs.pop("json", None)
        if files is not None and json is not None:
            raise ValueError("files不能和json同时使用")
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
        )
        if kwargs:
            unexpected = next(iter(kwargs))
            raise TypeError(f"request() got an unexpected keyword argument {unexpected!r}")
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
            method, url, headers, _body, timeout, read_timeout, proxy_override, proxy, request_cookies = prepared
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
                allow_redirects,
                max_redirects,
            )
            return _response_from_native(result)
        if stream:
            native = await self._session._native.request_stream_async(
                *prepared,
                allow_redirects,
                max_redirects,
            )
            return _response_from_stream(native)
        result = await self._session._native.request_async(
            *prepared,
            allow_redirects,
            max_redirects,
        )
        return _response_from_native(result)

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
    impersonate: str,
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


def _response_from_native(result: Sequence[Any]) -> Response:
    status_code, response_headers, content, fingerprint_id, profile, final_url, native_history = result
    return Response(
        status_code=status_code,
        headers=response_headers,
        content=content,
        url=final_url,
        fingerprint_id=fingerprint_id,
        impersonate=profile,
        history=_build_history(native_history, fingerprint_id, profile),
    )


def _response_from_stream(native: Any) -> Response:
    return Response(
        status_code=native.status_code,
        headers=native.headers,
        url=native.url,
        fingerprint_id=native.fingerprint_id,
        impersonate=native.impersonate,
        stream=native,
        history=_build_history(native.history, native.fingerprint_id, native.impersonate),
    )


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
