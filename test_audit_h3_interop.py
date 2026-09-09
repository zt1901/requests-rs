"""Local independent HTTP/3/QPACK interoperability (aioquic==1.3.0).

Run with the built requests_rs wheel and aioquic installed in an isolated venv.
No external endpoints. Private aioquic hooks are deliberate test instrumentation.
"""
from __future__ import annotations
import asyncio
import datetime
import importlib.metadata
import json
from pathlib import Path
import tempfile


def certificate(directory):
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "localhost")])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
            .public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now-datetime.timedelta(minutes=1))
            .not_valid_after(now+datetime.timedelta(days=1))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName("localhost")]), False)
            .sign(key, hashes.SHA256()))
    c, k = Path(directory)/"cert.pem", Path(directory)/"key.pem"
    c.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    k.write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                   serialization.NoEncryption()))
    return c, k


async def scenario(profile="chrome152"):
    from aioquic.asyncio import serve, QuicConnectionProtocol
    from aioquic.h3.connection import H3Connection, H3_ALPN
    from aioquic.h3.events import HeadersReceived
    from aioquic.quic.configuration import QuicConfiguration
    from requests_rs import requests
    metrics = {"responses": 0, "dynamic_sections": 0, "delayed_encoder_batches": 0,
               "decoder_feedback_bytes": 0}
    protocols = []

    class ObservedH3(H3Connection):
        def _encode_headers(self, stream_id, headers):
            block = super()._encode_headers(stream_id, headers)
            if block[0] != 0:  # nonzero encoded Required Insert Count
                metrics["dynamic_sections"] += 1
            return block

    class Server(QuicConnectionProtocol):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            self.http = ObservedH3(self._quic)
            protocols.append(self)
            self.pending = []
            original = self._quic.send_stream_data
            def observed_send(stream_id, data, end_stream=False):
                if stream_id == self.http._local_encoder_stream_id and data:
                    # HEADERS + DATA/FIN are delivered before their new table entries.
                    metrics["delayed_encoder_batches"] += 1
                    def deliver():
                        original(stream_id, data, end_stream=end_stream)
                        self.transmit()
                    self.pending.append(asyncio.get_running_loop().call_later(0.08, deliver))
                else:
                    original(stream_id, data, end_stream=end_stream)
            self._quic.send_stream_data = observed_send

        def quic_event_received(self, event):
            for item in self.http.handle_event(event):
                if isinstance(item, HeadersReceived):
                    body = b"independent aioquic dynamic QPACK response"
                    self.http.send_headers(item.stream_id, [
                        (b":status", b"200"), (b"content-length", str(len(body)).encode()),
                        (b"x-qpack-repeat", b"same-long-value-" * 10),
                        (b"content-type", b"text/plain"),
                    ])
                    self.http.send_data(item.stream_id, body, end_stream=True)
                    metrics["responses"] += 1
            metrics["decoder_feedback_bytes"] = sum(p.http._decoder_bytes_received for p in protocols)

    with tempfile.TemporaryDirectory(prefix="requests-rs-aioquic-") as temporary:
        cert, key = certificate(temporary)
        config = QuicConfiguration(is_client=False, alpn_protocols=H3_ALPN)
        config.load_cert_chain(cert, key)
        server = await serve("127.0.0.1", 0, configuration=config, create_protocol=Server)
        port = server._transport.get_extra_info("sockname")[1]
        def client():
            # The certificate is ephemeral and this test binds loopback only.
            with requests.Session(impersonate=profile, verify=False,
                                  http_version="http3", timeout=5) as session:
                for _ in range(8):
                    response = session.get(f"https://127.0.0.1:{port}/dynamic")
                    assert response.status_code == 200, response.status_code
                    assert response.http_version == "HTTP/3", response.http_version
                    assert response.content == b"independent aioquic dynamic QPACK response"
                    assert response.headers["x-qpack-repeat"] == "same-long-value-" * 10
        try:
            await asyncio.wait_for(asyncio.to_thread(client), timeout=50)
            await asyncio.sleep(0.1)
            assert metrics["responses"] == 8, metrics
            assert len(protocols) == 1, "requests should reuse the QUIC connection"
            assert metrics["dynamic_sections"] > 0, "no dynamic QPACK was exercised"
            assert metrics["delayed_encoder_batches"] > 0, metrics
            assert metrics["decoder_feedback_bytes"] > 0, "peer did not process decoder feedback"
        finally:
            for protocol in protocols:
                for handle in protocol.pending:
                    handle.cancel()
            server.close()
    metrics["aioquic"] = importlib.metadata.version("aioquic")
    metrics["pylsqpack"] = importlib.metadata.version("pylsqpack")
    return metrics


def test_independent_dynamic_qpack():
    for name, profile in (
        ("chrome152", "chrome152"),
        ("firefox154", json.loads((Path(__file__).parent / "tests/fixtures/firefox154_schema2.json").read_text(encoding="utf-8"))),
    ):
        print(name, json.dumps(asyncio.run(scenario(profile)), sort_keys=True))


if __name__ == "__main__":
    test_independent_dynamic_qpack()
