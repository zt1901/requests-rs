"""Offline build entry-point regressions; never compile or install a wheel."""
import os
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import build_and_install as build


class FingerprintSyncTests(unittest.TestCase):
    def setUp(self):
        path = Path(__file__).parent / 'scripts/同步指纹.py'
        spec = importlib.util.spec_from_file_location('audit_fingerprint_sync', path)
        self.sync = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.sync)

    def test_atomic_write_preserves_utf8_json(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'fingerprints.json'
            target.write_text('old', encoding='utf-8')
            records = [{'id': '指纹'}]
            self.sync.写入指纹(target, records)
            self.assertEqual(json.loads(target.read_text('utf-8')), records)
            self.assertEqual(list(Path(directory).iterdir()), [target])

    def test_failed_replace_preserves_original_and_removes_temp(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'fingerprints.json'
            target.write_bytes(b'original')
            with patch.object(self.sync.os, 'replace', side_effect=PermissionError('locked')):
                with self.assertRaises(PermissionError):
                    self.sync.写入指纹(target, [{'id': 'new'}])
            self.assertEqual(target.read_bytes(), b'original')
            self.assertEqual(list(Path(directory).iterdir()), [target])


class BuildTests(unittest.TestCase):
    def test_preserves_explicit_build_environment(self):
        supplied = {
            'CARGO_TARGET_DIR': 'custom-target', 'CARGO_HOME': 'custom-cargo',
            'RUSTUP_HOME': 'custom-rustup', 'LIBCLANG_PATH': 'custom-clang',
            'PATH': 'original-path',
        }
        with patch.dict(os.environ, supplied, clear=True), \
                patch.object(Path, 'is_dir', return_value=True), \
                patch.object(build.subprocess, 'run') as run:
            build.run('fake-command')
        actual = run.call_args.kwargs['env']
        for key, value in supplied.items():
            if key != 'PATH':
                self.assertEqual(actual[key], value)
        self.assertTrue(actual['PATH'].endswith(os.pathsep + supplied['PATH']))
        self.assertTrue(run.call_args.kwargs['check'])

    def _build(self, arguments):
        commands = []
        def run(*command):
            commands.append(command)
            if command[0] == 'maturin':
                output = Path(command[command.index('--out') + 1])
                (output / 'requests_rs-test.whl').write_bytes(b'test wheel')

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(build, 'os', SimpleNamespace(name='nt')), \
                patch.object(build.shutil, 'which', side_effect=lambda name: name), \
                patch.object(build, 'run', side_effect=run), \
                patch.object(build, '同步可编辑原生模块'), \
                patch.object(build, '输出目录', Path(directory) / 'dist'):
            build.main(arguments)
        return commands

    def test_default_build_is_locked_and_does_not_rewrite_fingerprints(self):
        commands = self._build([])
        self.assertEqual(len(commands), 2)
        self.assertEqual(commands[0][:4], ('maturin', 'build', '--release', '--locked'))
        self.assertEqual(commands[1][:3], ('uv', 'pip', 'install'))

    def test_fingerprint_sync_requires_explicit_option(self):
        commands = self._build(['--sync-fingerprints'])
        self.assertEqual(len(commands), 3)
        self.assertTrue(commands[0][1].endswith('同步指纹.py'))

    def test_missing_installer_fails_before_sync_or_build(self):
        with patch.object(build, 'os', SimpleNamespace(name='nt')), \
                patch.object(build.shutil, 'which', side_effect=['maturin', None]), \
                patch.object(build, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'uv'):
                build.main(['--sync-fingerprints'])
        run.assert_not_called()

    def test_ci_uses_locked_dependency_resolution(self):
        workflow = (Path(__file__).parent / '.github/workflows/build-wheels.yml').read_text('utf-8')
        self.assertIn('maturin build --release --locked --target', workflow)


if __name__ == '__main__':
    unittest.main()
