from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
import hashlib
import ipaddress
from pathlib import Path
import socket
import socketserver
import ssl
import threading
from typing import BinaryIO
from urllib.parse import urlsplit

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID


# 【可调参数】网关默认只监听本机；CA私钥仅用于本机CONNECT证书。
项目目录 = Path(__file__).resolve().parent
默认监听主机 = "127.0.0.1"
默认监听端口 = 18083
默认指纹版本 = "chrome150"
默认请求超时秒 = 30.0
默认最大请求体字节 = 64 * 1024 * 1024
默认CA目录 = 项目目录 / ".http3_gateway_ca"
跳到跳Header = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}


@dataclass
class 代理请求:
    method: str
    target: str
    version: str
    headers: list[tuple[str, str]]
    body: bytes

    def header(self, name: str, default: str = "") -> str:
        lowered = name.lower()
        for key, value in reversed(self.headers):
            if key.lower() == lowered:
                return value
        return default


class 本地证书颁发器:
    def __init__(self, directory: Path) -> None:
        self.directory = directory
        self.directory.mkdir(parents=True, exist_ok=True)
        self.ca_cert_path = directory / "local-http3-gateway-ca.pem"
        self.ca_key_path = directory / "local-http3-gateway-ca-key.pem"
        self._lock = threading.Lock()
        self._contexts: dict[str, ssl.SSLContext] = {}
        self._ca_key, self._ca_cert = self._load_or_create_ca()

    def _load_or_create_ca(self):
        if self.ca_cert_path.exists() and self.ca_key_path.exists():
            key = serialization.load_pem_private_key(self.ca_key_path.read_bytes(), password=None)
            cert = x509.load_pem_x509_certificate(self.ca_cert_path.read_bytes())
            return key, cert
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "requests_rust Local HTTP3 Gateway CA")])
        now = datetime.now(timezone.utc)
        cert = (
            x509.CertificateBuilder()
            .subject_name(name)
            .issuer_name(name)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - timedelta(minutes=5))
            .not_valid_after(now + timedelta(days=3650))
            .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
            .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
            .add_extension(
                x509.AuthorityKeyIdentifier.from_issuer_public_key(key.public_key()),
                critical=False,
            )
            .add_extension(
                x509.KeyUsage(
                    digital_signature=True,
                    content_commitment=False,
                    key_encipherment=False,
                    data_encipherment=False,
                    key_agreement=False,
                    key_cert_sign=True,
                    crl_sign=True,
                    encipher_only=False,
                    decipher_only=False,
                ),
                critical=True,
            )
            .sign(key, hashes.SHA256())
        )
        self.ca_key_path.write_bytes(
            key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            )
        )
        with suppress_os_error():
            self.ca_key_path.chmod(0o600)
        self.ca_cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
        return key, cert

    def context_for(self, host: str) -> ssl.SSLContext:
        with self._lock:
            context = self._contexts.get(host)
            if context is not None:
                return context
            digest = hashlib.sha256(host.encode()).hexdigest()[:20]
            cert_path = self.directory / f"leaf-{digest}.pem"
            key_path = self.directory / f"leaf-{digest}-key.pem"
            if not cert_path.exists() or not key_path.exists():
                self._create_leaf(host, cert_path, key_path)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.set_alpn_protocols(["http/1.1"])
            context.load_cert_chain(cert_path, key_path)
            self._contexts[host] = context
            return context

    def _create_leaf(self, host: str, cert_path: Path, key_path: Path) -> None:
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        now = datetime.now(timezone.utc)
        try:
            san = x509.IPAddress(ipaddress.ip_address(host))
        except ValueError:
            san = x509.DNSName(host)
        subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, host)])
        cert = (
            x509.CertificateBuilder()
            .subject_name(subject)
            .issuer_name(self._ca_cert.subject)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - timedelta(minutes=5))
            .not_valid_after(now + timedelta(days=30))
            .add_extension(x509.SubjectAlternativeName([san]), critical=False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
            .add_extension(
                x509.AuthorityKeyIdentifier.from_issuer_public_key(self._ca_key.public_key()),
                critical=False,
            )
            .add_extension(
                x509.KeyUsage(
                    digital_signature=True,
                    content_commitment=False,
                    key_encipherment=True,
                    data_encipherment=False,
                    key_agreement=False,
                    key_cert_sign=False,
                    crl_sign=False,
                    encipher_only=False,
                    decipher_only=False,
                ),
                critical=True,
            )
            .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
            .sign(self._ca_key, hashes.SHA256())
        )
        key_path.write_bytes(
            key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            )
        )
        with suppress_os_error():
            key_path.chmod(0o600)
        cert_path.write_bytes(
            cert.public_bytes(serialization.Encoding.PEM)
            + self._ca_cert.public_bytes(serialization.Encoding.PEM)
        )


class suppress_os_error:
    def __enter__(self):
        return self

    def __exit__(self, exception_type, _exception, _traceback):
        return exception_type is OSError


def 读取定长(stream: BinaryIO, size: int) -> bytes:
    data = bytearray()
    while len(data) < size:
        chunk = stream.read(size - len(data))
        if not chunk:
            raise ConnectionError("请求体提前结束")
        data.extend(chunk)
    return bytes(data)


def 读取分块请求体(stream: BinaryIO, maximum: int) -> bytes:
    body = bytearray()
    while True:
        line = stream.readline(8192)
        if not line:
            raise ConnectionError("chunk长度提前结束")
        size = int(line.split(b";", 1)[0].strip(), 16)
        if size == 0:
            while line := stream.readline(8192):
                if line in {b"\r\n", b"\n"}:
                    break
            return bytes(body)
        if len(body) + size > maximum:
            raise ValueError("请求体超过网关限制")
        body.extend(读取定长(stream, size))
        if 读取定长(stream, 2) != b"\r\n":
            raise ValueError("chunk结尾无效")


def 读取请求(stream: BinaryIO, maximum: int) -> 代理请求 | None:
    line = stream.readline(65537)
    if not line:
        return None
    if len(line) > 65536:
        raise ValueError("请求行过长")
    try:
        method, target, version = line.decode("latin1").strip().split(" ", 2)
    except ValueError as error:
        raise ValueError("请求行格式无效") from error
    headers = []
    while True:
        line = stream.readline(65537)
        if not line:
            raise ConnectionError("Header提前结束")
        if line in {b"\r\n", b"\n"}:
            break
        if len(line) > 65536 or b":" not in line:
            raise ValueError("Header格式无效")
        name, value = line.decode("latin1").split(":", 1)
        headers.append((name.strip(), value.strip()))
    lookup = {name.lower(): value for name, value in headers}
    if "chunked" in lookup.get("transfer-encoding", "").lower():
        body = 读取分块请求体(stream, maximum)
    else:
        length = int(lookup.get("content-length", "0"))
        if length < 0 or length > maximum:
            raise ValueError("请求体超过网关限制")
        body = 读取定长(stream, length) if length else b""
    return 代理请求(method.upper(), target, version.upper(), headers, body)


def 解析CONNECT目标(target: str) -> tuple[str, int]:
    parsed = urlsplit("//" + target)
    if parsed.hostname is None:
        raise ValueError("CONNECT目标缺少主机")
    return parsed.hostname, parsed.port or 443


def 过滤请求头(headers: list[tuple[str, str]]) -> list[tuple[str, str]]:
    return [
        (name, value)
        for name, value in headers
        if name.lower() not in 跳到跳Header | {"host", "content-length"}
    ]


def 过滤响应头(headers) -> list[tuple[str, str]]:
    return [
        (name, value)
        for name, value in headers.raw
        if name.lower() not in 跳到跳Header | {"content-length"}
    ]


def 发送简单响应(stream: BinaryIO, status: int, reason: str, body: bytes) -> None:
    stream.write(
        f"HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\n"
        f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode("latin1")
        + body
    )
    stream.flush()


class 网关处理器(socketserver.StreamRequestHandler):
    def handle(self) -> None:
        from requests_rust import Session

        self.connection.settimeout(self.server.request_timeout)
        self.outbound = Session(
            impersonate=self.server.impersonate,
            http_version="http3",
            verify=self.server.verify_upstream,
            cookie_store=False,
            max_connections=self.server.max_connections,
        )
        try:
            initial = 读取请求(self.rfile, self.server.max_request_body)
            if initial is None:
                return
            if initial.method == "CONNECT":
                host, port = 解析CONNECT目标(initial.target)
                self.wfile.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                self.wfile.flush()
                context = self.server.certificates.context_for(host)
                tls_socket = context.wrap_socket(self.connection, server_side=True)
                with tls_socket:
                    self._serve_http(tls_socket, host, port)
                return
            self._serve_plain(initial)
        except Exception as error:
            with suppress_os_error():
                发送简单响应(self.wfile, 502, "Bad Gateway", str(error).encode("utf-8", errors="replace"))
            self.server.record_error(error)
        finally:
            self.outbound.close()

    def _serve_http(self, connection: ssl.SSLSocket, host: str, port: int) -> None:
        stream = connection.makefile("rwb", buffering=0)
        with stream:
            while request := 读取请求(stream, self.server.max_request_body):
                try:
                    target = request.target
                    if target.startswith(("http://", "https://")):
                        parsed = urlsplit(target)
                        if parsed.hostname != host or (parsed.port or 443) != port:
                            raise ValueError("CONNECT隧道内禁止切换目标主机")
                        url = target
                    else:
                        authority = f"[{host}]:{port}" if ":" in host else f"{host}:{port}"
                        url = f"https://{authority}{target}"
                    keep_alive = self._forward(stream, request, url)
                    if not keep_alive:
                        return
                except Exception as error:
                    发送简单响应(stream, 502, "Bad Gateway", str(error).encode("utf-8", errors="replace"))
                    self.server.record_error(error)
                    return

    def _serve_plain(self, initial: 代理请求) -> None:
        if not initial.target.startswith("https://"):
            发送简单响应(self.wfile, 400, "Bad Request", b"absolute https:// URL required")
            return
        self._forward(self.wfile, initial, initial.target)

    def _forward(self, stream: BinaryIO, request: 代理请求, url: str) -> bool:
        connection_header = request.header("connection").lower()
        keep_alive = request.version == "HTTP/1.1" and connection_header != "close"
        response = self.outbound.request(
            request.method,
            url,
            headers=过滤请求头(request.headers),
            data=request.body or None,
            allow_redirects=False,
            timeout=self.server.request_timeout,
        )
        reason = "OK"
        try:
            from http import HTTPStatus

            reason = HTTPStatus(response.status_code).phrase
        except ValueError:
            pass
        headers = 过滤响应头(response.headers)
        headers.extend(
            [
                ("Content-Length", str(len(response.content))),
                ("X-Local-HTTP3-Gateway", "1"),
                ("X-Upstream-HTTP-Version", response.http_version),
                ("Connection", "keep-alive" if keep_alive else "close"),
            ]
        )
        stream.write(f"HTTP/1.1 {response.status_code} {reason}\r\n".encode("latin1"))
        for name, value in headers:
            stream.write(f"{name}: {value}\r\n".encode("latin1", errors="replace"))
        stream.write(b"\r\n")
        if request.method != "HEAD":
            stream.write(response.content)
        stream.flush()
        self.server.record_success(len(response.content))
        return keep_alive


class 线程网关服务(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(
        self,
        address: tuple[str, int],
        *,
        ca_dir: Path,
        impersonate: str,
        verify_upstream: bool,
        request_timeout: float,
        max_request_body: int,
        max_connections: int,
    ) -> None:
        super().__init__(address, 网关处理器)
        self.certificates = 本地证书颁发器(ca_dir)
        self.impersonate = impersonate
        self.verify_upstream = verify_upstream
        self.request_timeout = request_timeout
        self.max_request_body = max_request_body
        self.max_connections = max_connections
        self._stats_lock = threading.Lock()
        self.successes = 0
        self.errors = 0
        self.response_bytes = 0

    def record_success(self, size: int) -> None:
        with self._stats_lock:
            self.successes += 1
            self.response_bytes += size

    def record_error(self, _error: Exception) -> None:
        with self._stats_lock:
            self.errors += 1


class 本地HTTP3网关:
    def __init__(
        self,
        host: str = 默认监听主机,
        port: int = 默认监听端口,
        *,
        ca_dir: Path = 默认CA目录,
        impersonate: str = 默认指纹版本,
        verify_upstream: bool = True,
        request_timeout: float = 默认请求超时秒,
        max_request_body: int = 默认最大请求体字节,
        max_connections: int = 50,
        allow_remote: bool = False,
    ) -> None:
        if not allow_remote and host not in {"127.0.0.1", "::1", "localhost"}:
            raise ValueError("默认只允许监听本机；远程监听必须显式allow_remote=True")
        self.server = 线程网关服务(
            (host, port),
            ca_dir=ca_dir,
            impersonate=impersonate,
            verify_upstream=verify_upstream,
            request_timeout=request_timeout,
            max_request_body=max_request_body,
            max_connections=max_connections,
        )
        self.thread = threading.Thread(target=self.server.serve_forever, name="local-http3-gateway", daemon=True)

    @property
    def address(self) -> tuple[str, int]:
        return self.server.server_address

    @property
    def proxy_url(self) -> str:
        host, port = self.address
        return f"http://{host}:{port}"

    @property
    def ca_certificate(self) -> Path:
        return self.server.certificates.ca_cert_path

    def start(self) -> None:
        self.thread.start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default=默认监听主机)
    parser.add_argument("--port", type=int, default=默认监听端口)
    parser.add_argument("--impersonate", default=默认指纹版本)
    parser.add_argument("--no-verify-upstream", action="store_true")
    parser.add_argument("--allow-remote", action="store_true")
    args = parser.parse_args()
    gateway = 本地HTTP3网关(
        args.host,
        args.port,
        impersonate=args.impersonate,
        verify_upstream=not args.no_verify_upstream,
        allow_remote=args.allow_remote,
    )
    gateway.start()
    print(f"本地HTTP/3网关: {gateway.proxy_url}")
    print(f"本地CA证书: {gateway.ca_certificate}")
    print("HTTPS客户端必须信任该CA，或仅在受控测试中关闭本地代理证书验证。")
    try:
        gateway.thread.join()
    except KeyboardInterrupt:
        pass
    finally:
        gateway.close()


if __name__ == "__main__":
    main()
