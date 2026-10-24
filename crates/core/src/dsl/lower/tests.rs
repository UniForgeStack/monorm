use super::*;

#[test]
fn an_argument_without_an_extent_falls_back_to_the_position_of_its_key() {
    let key = Pos { line: 3, col: 9, len: 0 };
    let args = Args { pos: key, named: vec![("where".into(), key, Value::Bool(true))], ..Default::default() };
    assert_eq!(Named::new("index", &args).extent("where"), Some(key));
    assert_eq!(Named::new("index", &args).extent("name"), None);
}
