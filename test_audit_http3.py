"""Lightweight regression for vendored TLS build input invalidation."""
import pathlib
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent
BUILD = ROOT / 'vendor/btls/btls-sys/build/main.rs'


class TlsBuildInputTests(unittest.TestCase):
    def test_actual_rerun_directives_cover_sources_and_external_inputs(self):
        if shutil.which('rustc') is None:
            self.skipTest('rustc is required to execute the build-script helper')
        source = BUILD.read_text(encoding='utf-8')
        start = source.index('fn emit_rerun_if_changed(')
        end = source.index('\nfn emit_link_directives(', start)
        helper = source[start:end]
        harness = r'''
use std::path::PathBuf;
struct Config { env: Env }
struct Env {
    source_path: Option<PathBuf>, path: Option<PathBuf>,
    include_path: Option<PathBuf>, cmake_toolchain_file: Option<PathBuf>,
}
fn main() {
    let config = Config { env: Env {
        source_path: Some("external-source".into()),
        path: Some("external-libs".into()),
        include_path: Some("external-headers".into()),
        cmake_toolchain_file: Some("external-toolchain.cmake".into()),
    }};
    emit_rerun_if_changed(&config);
}
'''
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary)
            (path / 'check.rs').write_text(harness + helper, encoding='utf-8')
            binary = path / 'check.exe'
            subprocess.run(['rustc', '--edition=2024', str(path / 'check.rs'), '-o', str(binary)],
                           check=True, capture_output=True, timeout=60)
            result = subprocess.run([str(binary)], check=True, capture_output=True,
                                    text=True, timeout=10)
        watched = {line.removeprefix('cargo:rerun-if-changed=')
                   for line in result.stdout.splitlines()}
        self.assertTrue({'deps/boringssl', 'patches', 'cmake', 'external-source',
                         'external-libs', 'external-headers', 'external-toolchain.cmake'} <= watched)
        self.assertNotIn('build', watched)  # no generated output rebuild loop
        self.assertRegex(source, r'emit_rerun_if_changed\(&config\);')


class ProtocolIntegerTests(unittest.TestCase):
    def test_actual_integer_helpers_reject_overflow(self):
        if shutil.which('rustc') is None:
            self.skipTest('rustc is required to execute protocol helpers')
        source = (ROOT / 'vendor/quiche/quiche/src/h3/qpack/decoder.rs').read_text()
        helper = source[source.index('fn decode_int('):source.index('\nfn decode_str(')]
        transport = (ROOT / 'vendor/quiche/quiche/src/transport_params.rs').read_text()
        reserved = re.search(r'pub fn is_reserved\(&self\) -> bool \{(.*?)\n    \}',
                             transport, re.S).group(1)
        harness = r"""
#[derive(Debug, PartialEq)] enum Error { BufferTooShort }
type Result<T> = std::result::Result<T, Error>;
mod octets {
    pub struct Octets<'a> { data: &'a [u8], offset: usize }
    impl<'a> Octets<'a> {
        pub fn with_slice(data: &'a [u8]) -> Self { Self { data, offset: 0 } }
        pub fn cap(&self) -> usize { self.data.len() - self.offset }
        pub fn get_u8(&mut self) -> super::Result<u8> {
            let byte = *self.data.get(self.offset).ok_or(super::Error::BufferTooShort)?;
            self.offset += 1; Ok(byte)
        }
    }
}
struct Parameter { id: u64 }
impl Parameter { fn is_reserved(&self) -> bool { RESERVED_BODY } }
fn main() {
    for id in 0..27 { assert!(!Parameter { id }.is_reserved()); }
    for id in [27, 58, 89] { assert!(Parameter { id }.is_reserved()); }
    assert!(!Parameter { id: 28 }.is_reserved());
    assert_eq!(decode_int(&mut octets::Octets::with_slice(&[31, 154, 10]), 5), Ok(1337));
    let mut overflow = vec![255]; overflow.extend_from_slice(&[128; 9]); overflow.push(2);
    assert_eq!(decode_int(&mut octets::Octets::with_slice(&overflow), 8), Err(Error::BufferTooShort));
    // The largest valid u64 with an 8-bit prefix is still accepted.
    let mut encoded = vec![255]; let mut value = u64::MAX - 255;
    while value >= 128 { encoded.push((value as u8 & 127) | 128); value >>= 7; }
    encoded.push(value as u8);
    assert_eq!(decode_int(&mut octets::Octets::with_slice(&encoded), 8), Ok(u64::MAX));
}
""".replace('RESERVED_BODY', reserved)
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary)
            (path / 'check.rs').write_text(harness + helper, encoding='utf-8')
            binary = path / 'check.exe'
            subprocess.run(['rustc', '--edition=2024', str(path / 'check.rs'), '-o', str(binary)],
                           check=True, capture_output=True, timeout=60)
            subprocess.run([str(binary)], check=True, capture_output=True, timeout=10)


class CryptoInitializationTests(unittest.TestCase):
    def test_opaque_ffi_storage_is_initialized_before_native_partial_writes(self):
        source = (ROOT / 'vendor/quiche/quiche/src/crypto/boringssl.rs').read_text()
        self.assertIn('MaybeUninit::<AES_KEY>::zeroed()', source)
        helper = source[source.index('fn make_aead_ctx('):source.index('pub(crate) fn hkdf_extract(')]
        self.assertIn('MaybeUninit::zeroed()', helper)
        self.assertNotIn('MaybeUninit::uninit()', helper)
        self.assertNotIn('MaybeUninit::<AES_KEY>::uninit()', source)


if __name__ == '__main__':
    unittest.main()
