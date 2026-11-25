//! Writes the JSON Schema of the admin site config to stdout. `scripts/gen-admin-types.py` renders it to the Python and TypeScript types.

use monorm_admin::site_config::{Config, SectionConfig};
use schemars::generate::SchemaSettings;

fn main() {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    // The form sections reach the config as plain values, so the root type does not name them.
    generator.subschema_for::<SectionConfig>();
    let schema = generator.into_root_schema_for::<Config>();
    println!("{}", serde_json::to_string_pretty(&schema).expect("a schema is JSON"));
}
