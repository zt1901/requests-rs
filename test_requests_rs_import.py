from __future__ import annotations

from requests_rs import exceptions, requests


def main() -> None:
    assert requests.Session
    assert requests.AsyncSession
    assert requests.Response
    assert requests.get
    assert requests.post
    assert requests.available_profiles()
    assert requests.request.__kwdefaults__["impersonate"] == "chrome152"
    for name in exceptions.__all__:
        assert getattr(exceptions, name).__name__ == name
    print("from requests_rs import requests: OK")


if __name__ == "__main__":
    main()
