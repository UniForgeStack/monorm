//! The generated key types of a composite primary key.
use monorm_core::{codegen::{python, typescript}, dsl, schema::Schema};

const SOURCE: &str = r#"
model Item {
  shop  BigInt
  id    BigInt
  title String
  lines Line[]
  @@id([shop, id])
}

model Line {
  id      BigInt @id @default(autoincrement())
  shop    BigInt
  item_id BigInt
  item    Item   @relation(fields: [shop, item_id], references: [shop, id])
}
"#;

fn section<'a>(text: &'a str, class: &str) -> &'a str {
    text.split(class).nth(1).unwrap().split("class ").next().unwrap()
}

#[test]
fn composite_primary_key_types() {
    let ir = dsl::compile(SOURCE, None).unwrap();
    let schema = Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
    let py = python::generate(&ir, &schema, "schema.prisma").unwrap();
    assert!(section(&py.stub, "class Item(Model):").contains("    def pk(self) -> tuple[int, int]: ..."), "{}", py.stub);
    assert!(!section(&py.stub, "class Line(Model):").contains("def pk("), "{}", py.stub);
    let ts = typescript::generate(&ir, &schema, "schema.prisma", "@ufstack/monorm").unwrap();
    assert!(ts.contains("readonly pk: { readonly shop: bigint; readonly id: bigint };"), "{ts}");
}
