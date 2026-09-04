from __future__ import annotations

import base64
import copy
import json
import socket
import tempfile
import threading
from pathlib import Path
from typing import Any


项目目录 = Path(__file__).resolve().parent
捕获器目录 = 项目目录.parents[2] / "browser-fingerprint-collector"
Chrome152样本 = 捕获器目录 / "chrome152_collector_profile.json"


def _读取完整连接(连接: socket.socket, length: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        chunk = 连接.recv(length - len(chunks))
        if not chunk:
            raise RuntimeError("TLS ClientHello提前结束")
        chunks.extend(chunk)
    return bytes(chunks)


def _读取向量(data: bytes, offset: int, length_size: int) -> tuple[bytes, int]:
    length = int.from_bytes(data[offset : offset + length_size], "big")
    start = offset + length_size
    end = start + length
    if end > len(data):
        raise RuntimeError("TLS向量被截断")
    return data[start:end], end


def _查找ClientHello扩展(record: bytes, wanted: int) -> bytes | None:
    if record[0] != 22 or record[5] != 1:
        raise RuntimeError("没有捕获到TLS ClientHello")
    body = record[9:]
    offset = 34
    _, offset = _读取向量(body, offset, 1)
    _, offset = _读取向量(body, offset, 2)
    _, offset = _读取向量(body, offset, 1)
    extensions, _ = _读取向量(body, offset, 2)
    offset = 0
    while offset < len(extensions):
        extension_id = int.from_bytes(extensions[offset : offset + 2], "big")
        payload, offset = _读取向量(extensions, offset + 2, 2)
        if extension_id == wanted:
            return payload
    return None


def _捕获一次ClientHello(profile: Any) -> bytes:
    from requests_rs import requests
    Session = requests.Session

    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    port = listener.getsockname()[1]
    result: list[bytes | BaseException] = []

    def server() -> None:
        try:
            connection, _ = listener.accept()
            with connection:
                header = _读取完整连接(connection, 5)
                record = header + _读取完整连接(connection, int.from_bytes(header[3:5], "big"))
                result.append(record)
        except BaseException as error:
            result.append(error)
        finally:
            listener.close()

    thread = threading.Thread(target=server, daemon=True)
    thread.start()
    try:
        with Session(
            impersonate=profile,
            fingerprint_rotation=False,
            verify=False,
            connect_timeout=2,
            http_version="http2",
        ) as session:
            try:
                session.get(f"https://127.0.0.1:{port}/", timeout=2)
            except Exception:
                pass
    finally:
        thread.join(timeout=5)
    if not result:
        raise RuntimeError("没有收到TLS ClientHello")
    if isinstance(result[0], BaseException):
        raise result[0]
    return result[0]


def _写临时指纹(directory: Path, name: str, document: object) -> Path:
    path = directory / name
    path.write_text(json.dumps(document, ensure_ascii=False), encoding="utf-8")
    return path


def main() -> None:
    from requests_rs import requests
    Session = requests.Session
    available_profiles = requests.available_profiles

    source = json.loads(Chrome152样本.read_text(encoding="utf-8"))
    record = source[0]
    record["schema_version"] = 1
    expected_wire = next(
        extension for extension in record["tls"]["extension_wire"] if extension["id"] == 0xCA34
    )
    expected_payload = base64.b64decode(expected_wire["payload_base64"])

    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        profile = _写临时指纹(directory, "chrome152.json", [record])
        envelope = _写临时指纹(
            directory,
            "chrome152-envelope.json",
            {"schema_version": 1, "records": [record]},
        )
        with Session(impersonate=profile, fingerprint_rotation=False):
            pass
        with Session(impersonate=envelope, fingerprint_rotation=False):
            pass
        mcp_result = {
            "schema_version": 1,
            "path": str(Chrome152样本),
            "count": 1,
            "offset": 0,
            "limit": 20,
            "next_offset": None,
            "records": [record],
        }
        with Session(impersonate=[record], fingerprint_rotation=False) as session:
            assert session.impersonate == "chrome152"
        with Session(impersonate=mcp_result, fingerprint_rotation=False) as session:
            assert session.impersonate == "chrome152"

        client_hello = _捕获一次ClientHello(profile)
        actual_payload = _查找ClientHello扩展(client_hello, 0xCA34)
        assert actual_payload == expected_payload, (
            len(actual_payload or b""),
            len(expected_payload),
        )
        inline_client_hello = _捕获一次ClientHello(mcp_result)
        inline_payload = _查找ClientHello扩展(inline_client_hello, 0xCA34)
        assert inline_payload == expected_payload, (
            len(inline_payload or b""),
            len(expected_payload),
        )

        assert "chrome152" in available_profiles()
        builtin_client_hello = _捕获一次ClientHello("chrome152")
        builtin_payload = _查找ClientHello扩展(builtin_client_hello, 0xCA34)
        assert builtin_payload == expected_payload, (
            len(builtin_payload or b""),
            len(expected_payload),
        )

        invalid = copy.deepcopy(record)
        invalid_wire = next(
            extension
            for extension in invalid["tls"]["extension_wire"]
            if extension["id"] == 0xCA34
        )
        invalid_wire["payload_base64"] = "not-valid-base64"
        invalid_profile = _写临时指纹(directory, "invalid-payload.json", [invalid])
        try:
            Session(impersonate=invalid_profile, fingerprint_rotation=False)
        except RuntimeError as error:
            assert "payload_base64" in str(error), error
        else:
            raise AssertionError("非法0xca34 payload必须在Session创建时失败")

        future = copy.deepcopy(record)
        future["schema_version"] = 3
        future_profile = _写临时指纹(directory, "future-schema.json", [future])
        try:
            Session(impersonate=future_profile, fingerprint_rotation=False)
        except RuntimeError as error:
            assert "schema_version=3" in str(error), error
        else:
            raise AssertionError("未知schema_version必须fail-closed")

        mismatched_envelope = {
            "schema_version": 2,
            "records": [record],
        }
        try:
            Session(impersonate=mismatched_envelope, fingerprint_rotation=False)
        except RuntimeError as error:
            assert "与内部指纹记录不一致" in str(error), error
        else:
            raise AssertionError("envelope与内部record的schema不一致时必须fail-closed")

    print("Collector schema、MCP envelope、Chrome 152 Trust Anchor线级回放与fail-closed验证通过")


if __name__ == "__main__":
    main()
