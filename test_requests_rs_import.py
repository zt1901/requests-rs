from __future__ import annotations

from requests_rs import requests


def main() -> None:
    assert requests.Session
    assert requests.AsyncSession
    assert requests.Response
    assert requests.get
    assert requests.post
    assert requests.available_profiles()
    assert requests.request.__kwdefaults__["impersonate"] == "chrome152"
    print("from requests_rs import requests: OK")


if __name__ == "__main__":
    main()
