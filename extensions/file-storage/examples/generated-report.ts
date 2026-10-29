/** A report producer independent of ORM models and SQL transactions. */
import type { Reference, Registry } from "monorm/extensions/file-storage";

export async function generateReport(registry: Registry): Promise<Reference> {
  return registry.upload("reports", new TextEncoder().encode("product,total\nexample,42\n"), {
    filename: "report.csv", contentType: "text/csv",
  });
}

// Application composition:
// const reference = await generateReport(configuredRegistry);
// const row = await Report.objects.insert({ file: reference });
// Keep the reference in application scope after uncertain SQL/outer commit outcomes.
// `UploadError.reference` holds the candidate reference when an upload fails.
// Replacement, deletion and rollback never delete the stored object.
