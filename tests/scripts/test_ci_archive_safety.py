#!/usr/bin/env python3
"""Hostile compressed archives are refused without extracting any file."""
import importlib.util
import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('safety_bundle', ROOT / 'scripts/ci-nextest-bundle.py')
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)
REQUIRED = ('target/nextest/binaries-metadata.json', 'target/nextest/cargo-metadata.json')


class ArchiveSafety(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='archive-safety.')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def archive(self, members=(), required=True, raw=None):
        if raw is None:
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode='w', format=tarfile.GNU_FORMAT) as writer:
                entries = [(name, tarfile.REGTYPE, b'{}', '') for name in REQUIRED] if required else []
                for name, kind, data, link in [*entries, *members]:
                    header = tarfile.TarInfo(name)
                    header.type = kind
                    header.linkname = link
                    header.size = len(data) if kind == tarfile.REGTYPE else 0
                    writer.addfile(header, io.BytesIO(data) if header.size else None)
            raw = stream.getvalue()
        packed = subprocess.run(['zstd', '-q', '-c', '--check'], input=raw, capture_output=True, check=True).stdout
        path = self.root / 'suite.tar.zst'
        path.write_bytes(packed)
        return path

    def test_real_shape_and_gnu_long_names_pass_without_extraction(self):
        path = self.archive([('target/debug/deps/' + 'x' * 180, tarfile.REGTYPE, b'binary', ''),
                             ('target/debug/build/example/out/', tarfile.DIRTYPE, b'', '')])
        bundle.check_archive(path)
        self.assertFalse((self.root / 'target').exists())

    def test_traversal_absolute_foreign_prefix_and_links_refuse(self):
        for name in ('../evil', 'target/../evil', 'target//evil', '/target/evil', 'targetx/evil', 'target'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                bundle.check_archive(self.archive([(name, tarfile.REGTYPE, b'evil', '')]))
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE, tarfile.CONTTYPE):
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                bundle.check_archive(self.archive([('target/link', kind, b'', '/etc/passwd')]))
        self.assertFalse((self.root / 'target').exists())

    def test_effective_long_name_duplicate_and_missing_metadata_refuse(self):
        for members, required in (
            ([('target/' + 'x' * 150 + '/../evil', tarfile.REGTYPE, b'evil', '')], True),
            ([(REQUIRED[0], tarfile.REGTYPE, b'{}', '')], True),
            ([], False),
        ):
            with self.subTest(members=members), self.assertRaises(ValueError):
                bundle.check_archive(self.archive(members, required))

    def test_corrupt_zstd_or_tar_checksum_refuses(self):
        path = self.archive()
        path.write_bytes(path.read_bytes()[:-5])
        with self.assertRaises(ValueError):
            bundle.check_archive(path)
        raw = bytearray(tarfile.TarInfo(REQUIRED[0]).tobuf(tarfile.GNU_FORMAT) + b'\0' * 1024)
        raw[0] ^= 1
        with self.assertRaises(ValueError):
            bundle.check_archive(self.archive(raw=bytes(raw)))

    def test_metadata_payload_cap_refuses_before_large_allocation(self):
        header = tarfile.TarInfo('././@LongLink')
        header.type = tarfile.GNUTYPE_LONGNAME
        header.size = 65537
        with self.assertRaisesRegex(ValueError, 'metadata'):
            bundle.check_archive(self.archive(raw=header.tobuf(tarfile.GNU_FORMAT)))

    def test_gnu_sparse_logical_size_is_bounded_even_with_zero_stored_bytes(self):
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode='w', format=tarfile.GNU_FORMAT) as writer:
            for name in REQUIRED:
                header = tarfile.TarInfo(name)
                header.size = 2
                writer.addfile(header, io.BytesIO(b'{}'))
            header = tarfile.TarInfo('target/debug/sparse')
            header.type = tarfile.GNUTYPE_SPARSE
            writer.addfile(header)
        original = stream.getvalue()
        for logical, accepted in ((1024**2, True), (32 * 1024**3 + 1, False)):
            raw = bytearray(original)
            offset = 2048
            raw[offset + 483:offset + 495] = f'{logical:011o}'.encode() + bytes([0])
            raw[offset + 148:offset + 156] = b' ' * 8
            checksum = sum(raw[offset:offset + 512])
            raw[offset + 148:offset + 156] = f'{checksum:06o}'.encode() + bytes([0, 32])
            path = self.archive(raw=bytes(raw))
            if accepted:
                self.assertEqual(bundle.check_archive(path)['expanded_bytes'], 1024**2 + 4)
            else:
                with self.assertRaises(ValueError):
                    bundle.check_archive(path)

    def test_member_and_expanded_size_limits_are_enforced(self):
        path = self.archive([('target/debug/a', tarfile.REGTYPE, b'abc', '')])
        with self.assertRaises(ValueError):
            bundle.check_archive(path, max_members=2)
        with self.assertRaises(ValueError):
            bundle.check_archive(path, max_expanded_bytes=6)


if __name__ == '__main__':
    unittest.main()
