pub fn is_numeric_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::is_numeric_id;

    #[test]
    fn validates_numeric_ids() {
        assert!(is_numeric_id("6676328990"));
        assert!(!is_numeric_id(""));
        assert!(!is_numeric_id("abc"));
        assert!(!is_numeric_id("123/456"));
    }
}
