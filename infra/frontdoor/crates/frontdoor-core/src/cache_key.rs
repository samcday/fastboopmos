use alloc::string::String;
use sha2::{Digest, Sha256};

pub fn gha_cache_key(owner: &str, repo: &str, run_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(owner.as_bytes());
    hasher.update(b"/");
    hasher.update(repo.as_bytes());
    hasher.update(b":");
    hasher.update(run_id.as_bytes());

    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{gha_cache_key, hex_encode};

    #[test]
    fn cache_key_is_stable() {
        let key = gha_cache_key("samcday", "fastboopmos", "12345");
        assert_eq!(
            hex_encode(&key),
            "9c87e47358852e02c8d2fbafa0327d0ddb796326873da07ad15b040f9231f831"
        );
    }

    #[test]
    fn hex_encodes_bytes() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }
}
