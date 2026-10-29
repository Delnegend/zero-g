use std::fmt::Write;

pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_urlencode() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a/b?c=d&e=f"), "a%2Fb%3Fc%3Dd%26e%3Df");
        assert_eq!(urlencode("safe-text_123.~"), "safe-text_123.~");
    }
}
