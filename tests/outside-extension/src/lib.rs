use orm_contracts::{extension::Declaration, ir::{ColType, FieldIr, SchemaIr}};

const ATTRIBUTE: &str = "outside.tag";

fn tag(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<Declaration> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute == ATTRIBUTE).cloned().collect();
    for d in declarations {
        let label = d.positional.first().and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .ok_or_else(|| format!("@@{ATTRIBUTE} on {}: the label must be a non-empty string", d.model))?;
        // Static passes may already have captured storage before this library runs.
        let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
        let mut found = false;
        for model in models.filter(|m| m.name == d.model) {
            found = true;
            if model.fields.iter().any(|f| f.name == "tag") { return Err(format!("{} already has a field tag", d.model)); }
            model.fields.push(FieldIr { nullable: false, default: Some(label.into()), ..FieldIr::plain("tag", ColType::Text) });
        }
        if !found { return Err(format!("unknown model {}", d.model)); }
    }
    Ok(())
}

#[cfg(not(any(feature = "abi-major", feature = "abi-newer")))]
orm_contracts::extension!(("acme.outside.tag", tag));
#[cfg(feature = "abi-major")]
orm_contracts::extension!(abi = (1, 0); ("acme.outside.tag", tag));
#[cfg(all(feature = "abi-newer", not(feature = "abi-major")))]
orm_contracts::extension!(abi = (0, 2); ("acme.outside.tag", tag));
