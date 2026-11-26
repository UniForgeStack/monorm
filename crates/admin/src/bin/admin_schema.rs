//! Writes a JSON Schema to stdout. `scripts/gen-admin-types.py` renders it to the Python and TypeScript types.
//! No argument: the site config (what the host writes). `ui`: the metadata that the server sends to the UI.

use monorm_admin::site_config::{Config, SectionConfig};
use schemars::generate::SchemaSettings;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("ui") {
        println!("{}", serde_json::to_string_pretty(&monorm_admin::wire::schema()).expect("a schema is JSON"));
        return;
    }
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    // The form sections reach the config as plain values, so the root type does not name them.
    generator.subschema_for::<SectionConfig>();
    let schema = generator.into_root_schema_for::<Config>();
    println!("{}", serde_json::to_string_pretty(&schema).expect("a schema is JSON"));
}
