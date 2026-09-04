# requests-rs 0.4.0

Breaking API release. The only supported public import is:

```python
from requests_rs import requests
```

The previous `requests_rust` Python import is intentionally not provided.

Included assets:

- Windows x86-64 CPython ABI3 wheel (Python 3.10+)
- Linux x86-64 manylinux 2.38 CPython ABI3 wheel (Python 3.10+)

Both wheels were installed and tested with synchronous and asynchronous HTTPS requests. HTTP/3 template and fallback regression tests passed on Windows and Linux.
