const ALLOWED_ORIGINS_EXACT: &[&str] =
    &["https://www.fastboop.win", "https://bleeding.fastboop.win"];
const ALLOWED_LOCALHOST_HOSTS: &[&str] = &["localhost", "127.0.0.1"];

pub fn is_allowed_release_origin(origin: &str) -> bool {
    if ALLOWED_ORIGINS_EXACT.contains(&origin) {
        return true;
    }

    let Some(host) = origin_host(origin) else {
        return false;
    };

    ALLOWED_LOCALHOST_HOSTS.contains(&host)
}

fn origin_host(origin: &str) -> Option<&str> {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority.split('@').next_back().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() { None } else { Some(host) }
}

#[cfg(test)]
mod tests {
    use super::is_allowed_release_origin;

    #[test]
    fn allows_fastboop_origins() {
        assert!(is_allowed_release_origin("https://www.fastboop.win"));
        assert!(is_allowed_release_origin("https://bleeding.fastboop.win"));
    }

    #[test]
    fn allows_localhost_origins() {
        assert!(is_allowed_release_origin("http://localhost:5173"));
        assert!(is_allowed_release_origin("http://127.0.0.1:8080"));
    }

    #[test]
    fn rejects_other_origins() {
        assert!(!is_allowed_release_origin("https://example.com"));
        assert!(!is_allowed_release_origin(""));
    }
}
