//! Linked relations: a relation that names the relations to follow (`path: [a, b]`) and has no keys of its own.
use crate::{ir::RelKind, schema::Model};

/// The schema checks of every linked relation: each hop exists on the model it is read from, the last hop
/// reaches the target, no linked relation reaches itself, and a to-one linked relation has only to-one hops.
pub fn validate(models: &[Model]) -> Result<(), String> {
    for (mi, m) in models.iter().enumerate() {
        for r in m.ir.relations.iter().filter(|r| !r.path.is_empty()) {
            let what = |e: String| format!("relation {}.{}: path: {e}", m.ir.name, r.name);
            let mut seen = vec![(mi, r.name.clone())];
            let (end, to_one) = walk(models, mi, &r.path, &mut seen).map_err(what)?;
            if models[end].ir.name != r.target {
                return Err(what(format!("the last hop ends at {}, not at {}", models[end].ir.name, r.target)));
            }
            if r.kind == RelKind::One && !to_one {
                return Err(what(format!("a hop is to-many, so the relation is {}[], not {}?", r.target, r.target)));
            }
        }
    }
    Ok(())
}

/// Walks `hops` from model `from`; returns the model reached and whether every hop is to-one.
/// `seen` holds the linked relations on the way, to find a cycle.
fn walk(models: &[Model], from: usize, hops: &[String], seen: &mut Vec<(usize, String)>) -> Result<(usize, bool), String> {
    let (mut cur, mut to_one) = (from, true);
    for hop in hops {
        let Some(&(ri, ti)) = models[cur].relation_index.get(hop) else {
            let elsewhere = models.iter().find(|o| o.ir.name != models[cur].ir.name && o.relation_index.contains_key(hop));
            return Err(match elsewhere {
                Some(o) => format!("{hop} is a relation of {}, not of {}", o.ir.name, models[cur].ir.name),
                None => format!("{} has no relation {hop}", models[cur].ir.name),
            });
        };
        let rel = &models[cur].ir.relations[ri];
        if rel.path.is_empty() {
            to_one &= rel.kind == RelKind::One;
        } else {
            let key = (cur, rel.name.clone());
            if seen.contains(&key) {
                let names: Vec<String> = seen.iter().chain([&key]).map(|(m, n)| format!("{}.{n}", models[*m].ir.name)).collect();
                return Err(format!("a cycle: {}", names.join(" -> ")));
            }
            seen.push(key);
            let (end, inner) = walk(models, cur, &rel.path, seen)?;
            seen.pop();
            if models[end].ir.name != rel.target {
                return Err(format!("{}.{} ends at {}, not at {}", models[cur].ir.name, rel.name, models[end].ir.name, rel.target));
            }
            to_one &= inner;
        }
        cur = ti;
    }
    Ok((cur, to_one))
}

/// The real relations behind a linked relation, in order: every linked hop replaced by its own path.
/// A relation that is not linked gives itself.
pub fn flatten(models: &[Model], from: usize, name: &str) -> Vec<String> {
    let Some(&(ri, _)) = models[from].relation_index.get(name) else { return vec![name.to_owned()] };
    let rel = &models[from].ir.relations[ri];
    if rel.path.is_empty() {
        return vec![name.to_owned()];
    }
    let mut out = Vec::new();
    let mut cur = from;
    for hop in &rel.path {
        out.extend(flatten(models, cur, hop));
        cur = models[cur].relation_index[hop].1;
    }
    out
}
