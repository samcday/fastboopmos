pub fn content_type_for_ext(ext: &str) -> &'static str {
    match ext {
        "channel" => "application/octet-stream",
        "sha256" | "sha256sum" | "txt" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::content_type_for_ext;

    #[test]
    fn maps_known_extensions() {
        assert_eq!(content_type_for_ext("channel"), "application/octet-stream");
        assert_eq!(content_type_for_ext("sha256"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for_ext("json"), "application/json");
        assert_eq!(content_type_for_ext("zip"), "application/zip");
    }

    #[test]
    fn falls_back_for_unknown_extensions() {
        assert_eq!(content_type_for_ext("unknown"), "application/octet-stream");
    }
}
