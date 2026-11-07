//! SQL text helpers that extensions and the host share.

/// `name` as a double-quoted SQL identifier.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_quote_in_a_name_is_doubled() {
        assert_eq!(super::ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(super::ident("t"), "\"t\"");
    }
}
