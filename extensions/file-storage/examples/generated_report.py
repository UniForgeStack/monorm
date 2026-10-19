"""A report producer independent of ORM models and SQL transactions."""

from orm_file_storage import Reference, Registry


async def generate_report(registry: Registry) -> Reference:
    # A producer may supply a large async stream instead of these CSV bytes.
    return await registry.upload("reports", b"product,total\nexample,42\n", filename="report.csv", content_type="text/csv")


# Application composition:
# reference = await generate_report(configured_registry)
# row = await Report.objects.insert(file=reference)
# Keep the reference in application scope if the SQL/outer commit acknowledgement fails.
# `UploadError.reference` holds the candidate reference when an upload fails.
# No row replacement, deletion or rollback deletes the stored object.
