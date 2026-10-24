use orm_contracts::{extension::Declaration, ir::{ColType, FieldIr, SchemaIr}};

const ATTRIBUTE: &str = "outside.tag";

fn tag(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute == ATTRIBUTE).cloned().collect();
    for d in declarations {
        let label = d.positional.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .ok_or_else(|| format!("@@{ATTRIBUTE} on {}: the label must be a non-empty string", d.model))?;
        let model = ir.models.iter_mut().find(|m| m.name == d.model).ok_or_else(|| format!("unknown model {}", d.model))?;
        if model.fields.iter().any(|f| f.name == "tag") { return Err(format!("{} already has a field tag", d.model)); }
        model.fields.push(FieldIr { nullable: false, default: Some(label.into()), ..FieldIr::plain("tag", ColType::Text) });
    }
    Ok(())
}

#[cfg(not(any(feature = "abi-major", feature = "abi-newer")))]
orm_contracts::extension!(("acme.outside.tag", tag));
#[cfg(feature = "abi-major")]
orm_contracts::extension!(abi = (1, 0); ("acme.outside.tag", tag));
#[cfg(all(feature = "abi-newer", not(feature = "abi-major")))]
orm_contracts::extension!(abi = (0, 2); ("acme.outside.tag", tag));
