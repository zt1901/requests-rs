set -euo pipefail
export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(gcc -print-file-name=include)"
if [ -e /build/source ]; then echo "Use a fresh build volume: /build/source already exists" >&2; exit 1; fi
mkdir -p /build/source /io/wheels
cd /input
tar --exclude=.git --exclude=target --exclude=dist --exclude='dist-*' --exclude=__pycache__ --exclude='.venv' --exclude='benchmark*' -cf - . | tar -xf - -C /build/source
cd /build/source
{ rustc --version; gcc --version | head -1; getconf GNU_LIBC_VERSION; python --version; } > /io/environment.log
cargo test --locked --all-targets > /io/all-targets.log 2>&1
maturin build --release --locked --compatibility manylinux2014 --interpreter /opt/python/cp310-cp310/bin/python --out /io/wheels > /io/maturin.log 2>&1
python -m pip install --no-deps --force-reinstall /io/wheels/*.whl > /io/install.log 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -keyout /build/fingerprint_key.pem -out /build/fingerprint_cert.pem -days 2 -subj /CN=localhost > /io/test-certificate.log 2>&1
python /input/scripts/manylinux2014/run-regression.py > /io/regression-run.log 2>&1
python -m unittest test_audit_quic_cancel -v > /io/cancel.log 2>&1
auditwheel show /io/wheels/*.whl > /io/auditwheel.log 2>&1
