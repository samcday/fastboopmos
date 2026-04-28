#[derive(Debug, PartialEq, Eq)]
pub enum RangeError {
    Invalid,
    NotSatisfiable { size: u64 },
}

pub fn parse_single_byte_range(header: &str, size: u64) -> Result<(u64, u64), RangeError> {
    let spec = header.strip_prefix("bytes=").ok_or(RangeError::Invalid)?;
    let spec = spec.trim();
    if spec.is_empty() || spec.contains(',') {
        return Err(RangeError::Invalid);
    }

    let (start_raw, end_raw) = spec.split_once('-').ok_or(RangeError::Invalid)?;
    let start_raw = start_raw.trim();
    let end_raw = end_raw.trim();

    if start_raw.is_empty() {
        if end_raw.is_empty() {
            return Err(RangeError::Invalid);
        }
        let suffix_len: u64 = end_raw.parse().map_err(|_| RangeError::Invalid)?;
        if suffix_len == 0 || size == 0 {
            return Err(RangeError::NotSatisfiable { size });
        }
        if suffix_len >= size {
            return Ok((0, size - 1));
        }
        return Ok((size - suffix_len, size - 1));
    }

    let start: u64 = start_raw.parse().map_err(|_| RangeError::Invalid)?;
    if start >= size {
        return Err(RangeError::NotSatisfiable { size });
    }

    if end_raw.is_empty() {
        return Ok((start, size - 1));
    }

    let end: u64 = end_raw.parse().map_err(|_| RangeError::Invalid)?;
    if end < start {
        return Err(RangeError::Invalid);
    }

    Ok((start, end.min(size - 1)))
}

#[cfg(test)]
mod tests {
    use super::{RangeError, parse_single_byte_range};

    #[test]
    fn normal_range() {
        assert_eq!(parse_single_byte_range("bytes=0-499", 1000), Ok((0, 499)));
    }

    #[test]
    fn open_ended_range() {
        assert_eq!(parse_single_byte_range("bytes=500-", 1000), Ok((500, 999)));
    }

    #[test]
    fn suffix_range() {
        assert_eq!(parse_single_byte_range("bytes=-100", 1000), Ok((900, 999)));
    }

    #[test]
    fn suffix_larger_than_file() {
        assert_eq!(parse_single_byte_range("bytes=-2000", 1000), Ok((0, 999)));
    }

    #[test]
    fn unsatisfiable_start() {
        assert_eq!(
            parse_single_byte_range("bytes=1000-", 1000),
            Err(RangeError::NotSatisfiable { size: 1000 })
        );
    }

    #[test]
    fn malformed_ranges() {
        assert_eq!(
            parse_single_byte_range("bytes=500-100", 1000),
            Err(RangeError::Invalid)
        );
        assert_eq!(
            parse_single_byte_range("bytes=0-100,200-300", 1000),
            Err(RangeError::Invalid)
        );
        assert_eq!(
            parse_single_byte_range("0-100", 1000),
            Err(RangeError::Invalid)
        );
    }

    #[test]
    fn empty_file_is_not_satisfiable() {
        assert_eq!(
            parse_single_byte_range("bytes=0-0", 0),
            Err(RangeError::NotSatisfiable { size: 0 })
        );
    }
}
