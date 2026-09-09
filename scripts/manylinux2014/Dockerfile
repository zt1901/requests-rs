FROM quay.io/pypa/manylinux2014_x86_64@sha256:493d2032114d757aaa761a9385ad8497f391503bf71acef9abeeb66682ca5d90
ENV PATH=/opt/python/cp311-cp311/bin:/root/.cargo/bin:$PATH
RUN python -m pip install --no-cache-dir maturin==1.9.6 libclang==18.1.1
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup.sh && sh /tmp/rustup.sh -y --profile minimal --default-toolchain 1.98.0
ENV LIBCLANG_PATH=/opt/python/cp311-cp311/lib/python3.11/site-packages/clang/native
ENV CARGO_TARGET_DIR=/build/target CARGO_BUILD_JOBS=4 PYTHONUTF8=1
WORKDIR /source
