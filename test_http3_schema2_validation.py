from __future__ import annotations

import base64
import copy
import hashlib
import json
from pathlib import Path

from requests_rust import Session


FIXTURE = Path(__file__).parent / "tests" / "fixtures" / "chrome152_schema2.json"


def rejected(record: dict, expected: str) -> None:
    try:
        Session(impersonate=[record], http_version="http3")
    except (RuntimeError, ValueError) as error:
        assert expected.lower() in str(error).lower(), error
    else:
        raise AssertionError(f"invalid schema 2 template was accepted: {expected}")


def main() -> None:
    record = json.loads(FIXTURE.read_text(encoding="utf-8"))[0]
    with Session(impersonate=[record], http_version="http3"):
        pass

    semantic = copy.deepcopy(record)
    semantic["http3"]["quic"]["transport_parameters"]["initial_max_data"] += 1
    rejected(semantic, "semantic value")

    structured = copy.deepcopy(record)
    structured["http3"]["quic"]["transport_parameters"]["wire"]["parameters"][0][
        "value_hex"
    ] = "80f00001"
    rejected(structured, "disagrees with wire")

    raw = copy.deepcopy(record)
    wire = raw["http3"]["quic"]["transport_parameters"]["wire"]
    payload = bytearray(base64.b64decode(wire["base64"]))
    payload[-1] ^= 1
    wire["base64"] = base64.b64encode(payload).decode("ascii")
    wire["sha256"] = hashlib.sha256(payload).hexdigest()
    rejected(raw, "disagrees with wire")

    extension = copy.deepcopy(record)
    extension["http3"]["tls"]["extension_wire"][0]["payload_sha256"] = "0" * 64
    rejected(extension, "tls extension")

    settings = copy.deepcopy(record)
    values = settings["http3"]["http"]["settings"]
    values[0], values[1] = values[1], values[0]
    rejected(settings, "settings order")

    datagram = copy.deepcopy(record)
    for setting in datagram["http3"]["http"]["settings"]:
        if setting[0] == 51:
            setting[1] = 0
    rejected(datagram, "datagram")

    print("schema 2 HTTP/3 strict validation: passed")


if __name__ == "__main__":
    main()
