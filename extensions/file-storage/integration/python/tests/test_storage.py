import asyncio
import io
import json
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

from opendal import AsyncOperator
from monorm_file_storage import (
    CapabilityError,
    FileAdapter,
    FileField,
    Reference,
    Registry,
    SizeLimitError,
    StorageError,
    UploadError,
)
from monorm_file_storage.storage import CHUNK_SIZE

FIXTURES = Path(__file__).parents[2] / "../fixtures/references.json"


async def read_all(stream):
    return b"".join([chunk async for chunk in stream])


class RecordingOperator:
    """A versioned, presigning stand-in for an object service: OpenDAL's memory and fs services have neither."""

    def __init__(self, inner, version="v7", presign=True):
        self.inner, self.version, self.presign, self.calls = inner, version, presign, []

    def capability(self):
        outer = self

        class Capability:
            read_with_version = outer.version is not None
            presign_read = outer.presign

        return Capability()

    async def open(self, path, mode, **kwargs):
        self.calls.append(("open", mode, kwargs))
        return await self.inner.open(path, mode, **{k: v for k, v in kwargs.items() if k != "version"})

    async def stat(self, path, **kwargs):
        meta = await self.inner.stat(path, **kwargs)
        return type("Meta", (), {"version": self.version, "content_length": meta.content_length})()

    async def presign_read(self, path, expires, **kwargs):
        self.calls.append(("presign_read", path, expires, kwargs))
        return type("Presigned", (), {"url": f"https://files/{path}?ttl={expires}"})()


class MemoryTestCase(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.operator = AsyncOperator("memory")
        self.registry = Registry({"reports": self.operator})

    async def keys(self):
        return sorted([entry.path async for entry in await self.operator.list("/") if entry.path != "/"])


class ReferenceTests(unittest.TestCase):
    def test_shared_vectors(self):
        for data in json.loads(FIXTURES.read_text()):
            self.assertEqual(Reference.from_dict(data).to_dict(), data)
            self.assertEqual(Reference.from_json(Reference.from_dict(data).to_json()).to_dict(), data)

    def test_invalid(self):
        for extra in [
            {"v": 2},
            {"size": -1},
            {"size": True},
            {"size": 2**53},
            {"version": None},
            {"storage": ""},
            {"secret": "no"},
            {"key": "\ud800"},
        ]:
            with self.assertRaises(ValueError):
                Reference.from_dict({"v": 1, "storage": "local", "key": "key", **extra})


class UploadTests(MemoryTestCase):
    async def test_a_stream_keeps_its_position_and_the_reference_describes_the_object(self):
        source = io.BytesIO(b"skipreport")
        source.seek(4)
        reference = await self.registry.upload("reports", source, filename="report.xlsx")
        self.assertFalse(source.closed)
        self.assertEqual(source.tell(), 10)
        self.assertEqual((reference.storage, reference.size, reference.filename), ("reports", 6, "report.xlsx"))
        self.assertEqual(await read_all(self.registry.open(reference)), b"report")
        self.assertEqual(await self.keys(), [reference.key])

    async def test_keys_are_random_uuid4_hex_and_filenames_are_metadata_only(self):
        first = await self.registry.upload("reports", b"a", filename="../../etc/passwd")
        second = await self.registry.upload("reports", b"a", filename="../../etc/passwd")
        for reference in (first, second):
            self.assertRegex(reference.key, r"^[0-9a-f]{32}$")
            self.assertEqual(uuid_version(reference.key), 4)
        self.assertNotEqual(first.key, second.key)

    async def test_content_type_defaults_to_octet_stream_and_is_not_guessed(self):
        plain = await self.registry.upload("reports", b"x", filename="report.xlsx")
        self.assertEqual(plain.content_type, "application/octet-stream")
        typed = await self.registry.upload("reports", b"x", content_type="text/csv")
        self.assertEqual(typed.content_type, "text/csv")

    async def test_an_upload_streams_in_bounded_chunks(self):
        async def large():
            for _ in range(200):
                yield b"x" * CHUNK_SIZE

        reference = await self.registry.upload("reports", large())
        self.assertEqual(reference.size, 200 * CHUNK_SIZE)
        self.assertTrue(all([len(c) <= CHUNK_SIZE async for c in self.registry.open(reference)]))

    async def test_max_size_stops_the_stream_and_the_error_holds_the_candidate(self):
        pulled = 0

        async def large():
            nonlocal pulled
            for _ in range(200):
                pulled += 1
                yield b"x" * CHUNK_SIZE

        with self.assertRaises(UploadError) as caught:
            await self.registry.upload("reports", large(), filename="big.bin", max_size=CHUNK_SIZE)
        self.assertIsInstance(caught.exception.__cause__, SizeLimitError)
        self.assertEqual(pulled, 2)
        candidate = caught.exception.reference
        self.assertEqual((candidate.storage, candidate.filename), ("reports", "big.bin"))
        self.assertIsNone(candidate.size)
        # The failed upload commits no object.
        self.assertEqual(await self.keys(), [])

    async def test_a_failing_source_raises_an_upload_error_and_a_bad_chunk_a_type_error_cause(self):
        async def invalid():
            yield "not bytes"

        with self.assertRaises(UploadError) as caught:
            await self.registry.upload("reports", invalid())
        self.assertIsInstance(caught.exception.__cause__, TypeError)

    async def test_a_cancelled_upload_is_not_wrapped_and_commits_nothing(self):
        started = asyncio.Event()

        async def blocked():
            yield b"partial"
            started.set()
            await asyncio.Event().wait()

        task = asyncio.create_task(self.registry.upload("reports", blocked()))
        await started.wait()
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(await self.keys(), [])

    async def test_an_unconfigured_storage_is_rejected(self):
        with self.assertRaises(StorageError):
            await self.registry.upload("other", b"x")
        with self.assertRaises(StorageError):
            await read_all(self.registry.open(Reference("other", "k")))
        with self.assertRaises(ValueError):
            Registry({"": self.operator})

    async def test_the_adapter_rejects_a_reference_of_another_storage_name(self):
        reference = await self.registry.upload("reports", b"x")
        with self.assertRaises(Exception):
            FileAdapter(FileField("file", "other"), lambda: self.registry).validate(reference)


class VersionAndSigningTests(MemoryTestCase):
    async def test_the_stored_version_is_in_the_reference_and_passed_to_open_and_sign(self):
        recording = RecordingOperator(self.operator, version="v7")
        registry = Registry({"reports": recording})
        reference = await registry.upload("reports", b"data")
        self.assertEqual(reference.version, "v7")
        recording.calls.clear()
        # Reading goes through the real operator, which refuses a version it does not support.
        await read_all(registry.open(reference))
        self.assertEqual(recording.calls, [("open", "rb", {"version": "v7"})])
        url = await registry.signed_url(reference, expires_in=60)
        self.assertEqual(url, f"https://files/{reference.key}?ttl=60")
        self.assertEqual(recording.calls[-1], ("presign_read", reference.key, 60, {"version": "v7"}))

    async def test_a_service_without_versions_stores_none_and_passes_none(self):
        recording = RecordingOperator(self.operator, version=None)
        registry = Registry({"reports": recording})
        reference = await registry.upload("reports", b"data")
        self.assertIsNone(reference.version)
        self.assertNotIn("version", reference.to_dict())
        recording.calls.clear()
        await read_all(registry.open(reference))
        await registry.signed_url(reference)
        self.assertEqual([call[-1] for call in recording.calls], [{}, {}])

    async def test_the_content_type_reaches_the_service(self):
        recording = RecordingOperator(self.operator, version=None)
        await Registry({"reports": recording}).upload("reports", b"x", content_type="text/csv")
        self.assertEqual(recording.calls[0], ("open", "wb", {"content_type": "text/csv"}))

    async def test_a_service_without_presign_raises_a_capability_error(self):
        reference = await self.registry.upload("reports", b"x")
        with self.assertRaises(CapabilityError):
            await self.registry.signed_url(reference)
        # The capability flag alone decides: OpenDAL's own error is mapped as well.

        class Refusing(RecordingOperator):
            async def presign_read(self, *args, **kwargs):
                return await self.inner.presign_read(*args, **kwargs)

        refusing = Registry({"reports": Refusing(self.operator, presign=True)})
        with self.assertRaises(CapabilityError):
            await refusing.signed_url(reference)

    async def test_the_expiry_is_checked(self):
        reference = await self.registry.upload("reports", b"x")
        for expires_in in (0, 604801, 1.5):
            with self.assertRaises(ValueError):
                await self.registry.signed_url(reference, expires_in=expires_in)


class FsTests(unittest.IsolatedAsyncioTestCase):
    async def test_the_fs_service_stores_and_reads_back(self):
        with tempfile.TemporaryDirectory() as directory:
            registry = Registry({"local": AsyncOperator("fs", root=directory)})
            reference = await registry.upload("local", b"report")
            self.assertEqual((Path(directory) / reference.key).read_bytes(), b"report")
            self.assertEqual(await read_all(registry.open(reference)), b"report")
            with self.assertRaises(CapabilityError):
                await registry.signed_url(reference)


class MemoryUsageTest(unittest.TestCase):
    def test_streaming_64_mib_keeps_the_peak_rss_low(self):
        # Python allocation tracing cannot see OpenDAL's Rust allocations, so measure the process peak RSS.
        script = textwrap.dedent(
            """
            import asyncio, json, resource, sys, tempfile
            sys.path[:] = json.loads(sys.argv[1])
            from opendal import AsyncOperator
            from monorm_file_storage import Registry

            async def source():
                for _ in range(1024):
                    yield b"x" * 65536

            async def main():
                with tempfile.TemporaryDirectory() as directory:
                    registry = Registry({"local": AsyncOperator("fs", root=directory)})
                    before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
                    reference = await registry.upload("local", source())
                    after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
                assert reference.size == 64 * 1024 * 1024
                unit = 1 if sys.platform == "darwin" else 1024
                print((after - before) * unit)

            asyncio.run(main())
            """
        )
        result = subprocess.run(
            [sys.executable, "-I", "-c", script, json.dumps(sys.path)], capture_output=True, text=True, check=True
        )
        self.assertLess(int(result.stdout), 32 * 1024 * 1024)


def uuid_version(hex_key):
    import uuid

    return uuid.UUID(hex=hex_key).version


if __name__ == "__main__":
    unittest.main()
