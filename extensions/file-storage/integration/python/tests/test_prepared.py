import tempfile
import unittest
from pathlib import Path
from monorm.adapters import Pending
from opendal import AsyncOperator
from monorm_file_storage import (
    FileAdapter,
    FileField,
    FileFieldError,
    FileNotLoaded,
    MissingFile,
    Reference,
    Registry,
    Upload,
)


class Tests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.registry = Registry({"reports": AsyncOperator("fs", root=self.directory.name)})
        self.fields = {"file": FileField("file", "reports", True)}
        self.adapter = FileAdapter(self.fields["file"], lambda: self.registry)

    def stored(self):
        return list(Path(self.directory.name).iterdir())

    async def test_validate_defers_an_upload_and_prepare_stores_it(self):
        pending = self.adapter.validate(Upload(b"report", filename="report.xlsx"))
        self.assertIsInstance(pending, Pending)
        self.assertEqual(self.stored(), [])
        data = await self.adapter.prepare(pending.value)
        reference = Reference.from_dict(data)
        self.assertEqual(len(self.stored()), 1)
        self.assertTrue((Path(self.directory.name) / reference.key).exists())
        self.assertEqual(self.adapter.decode(data), reference)

    async def test_references_and_null_validate_to_stored_values(self):
        ref = await self.registry.upload("reports", b"report")
        self.assertEqual(self.adapter.validate(ref), ref.to_dict())
        self.assertEqual(self.adapter.validate(ref.to_dict()), ref.to_dict())
        self.assertIsNone(self.adapter.validate(None))
        with self.assertRaises(FileFieldError):
            FileAdapter(FileField("file", "other"), lambda: self.registry).validate(ref)
        with self.assertRaises(MissingFile):
            FileAdapter(FileField("file", "reports"), lambda: self.registry).validate(None)
        self.assertEqual(len(self.stored()), 1)

    async def test_a_missing_registry_or_provider_fails_before_io(self):
        with self.assertRaises(FileFieldError):
            FileAdapter(self.fields["file"], lambda: None).validate(Upload(b"x"))
        with self.assertRaises(Exception):
            FileAdapter(FileField("file", "unknown"), lambda: self.registry).validate(Upload(b"x"))
        self.assertEqual(self.stored(), [])

    async def test_null_and_unloaded_fail_before_provider_access(self):
        field = self.fields["file"]
        with self.assertRaises(FileNotLoaded):
            await field.signed_url({}, self.registry)
        with self.assertRaises(MissingFile):
            field.open({"file": None}, self.registry)
        self.assertIsNone(field.decode(None))
        self.assertEqual(self.stored(), [])
