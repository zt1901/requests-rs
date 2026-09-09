import json, os, subprocess, time
from pathlib import Path
base = Path('/build')
out = Path('/io')
root = base / 'source'
python = Path('/opt/python/cp311-cp311/bin/python')
env = os.environ.copy()
env.update(PYTHONUTF8='1', PYTHONIOENCODING='utf-8', CARGO_TARGET_DIR=str(base / 'target'), CARGO_BUILD_JOBS='4', TMPDIR='/tmp')
cases = [
 ('audit-all', ['-m', 'unittest', 'test_audit_python_api', 'test_audit_build', 'test_audit_http3', 'test_audit_rust_api', 'test_audit_error_details', '-v'], 120),
 ('audit-packaged-wrapper', ['-c', "import unittest, test_audit_python_api as t; from requests_rs import requests; from types import SimpleNamespace; t.api = SimpleNamespace(**requests.Session.__init__.__globals__); result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromModule(t)); raise SystemExit(not result.wasSuccessful())"], 60),
 *[(Path(name).stem, [name], 120) for name in ('test_requests_rs_import.py', 'test_python_api.py', 'test_proxy_preauth.py', 'test_socks5_auth.py', 'test_http3_schema2_validation.py', 'test_websocket_api.py')],
 ('test_http3', ['test_http3.py'], 600),
]
results = []
for name, args, timeout in cases:
 start = time.monotonic()
 log = out / (name + '.log')
 print('START', name, flush=True)
 try:
  with log.open('w', encoding='utf-8') as output:
   p = subprocess.run([str(python), *args], cwd=root, env=env, stdout=output, stderr=subprocess.STDOUT, timeout=timeout)
  code = p.returncode
 except subprocess.TimeoutExpired:
  code = 'TIMEOUT'
 result = dict(name=name, code=code, elapsed=round(time.monotonic()-start, 2), log=str(log))
 results.append(result)
 (out / 'regression-results.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
 print(result, flush=True)
 print(log.read_text(encoding='utf-8', errors='replace')[-5000:], flush=True)
raise SystemExit(any(r['code'] != 0 for r in results))
