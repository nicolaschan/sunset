//! Pulls the `x25519` field out of the relay's identity JSON. The
//! relay produces a fixed shape (see `sunset_relay::status::identity_json`):
//! `{"ed25519":"<hex>","x25519":"<hex>","address":"<url>"}` — three
//! fields, hex-only values, no nested objects. We hand-roll a tiny
//! scanner so this crate doesn't have to pull in serde_json.
//!
//! The scanner finds the first occurrence of the literal substring
//! `"x25519"` in the body. This is safe for the flat schema above: a
//! field name like `"prev_x25519"` is preceded by `_`, not `"`, so it
//! doesn't match; a value containing `x25519` (e.g. inside
//! `"address":"…#x25519=…"`) lacks the leading double-quote. If the
//! relay ever grows nested objects with their own `"x25519"` keys,
//! this scanner would extract the first one regardless of depth — at
//! that point we should switch to a real JSON parser.

use crate::error::{Error, Result};

/// Extract an optional string-valued field from the relay's flat JSON
/// identity body. Returns `None` if the field is absent (forwards-
/// compatible: old relays don't ship `webtransport_address`); returns
/// `Err` if the field exists but is malformed.
pub fn extract_string_field(body: &str, field: &str) -> Result<Option<String>> {
    let key = format!("\"{field}\"");
    let key_start = match body.find(&key) {
        Some(i) => i,
        None => return Ok(None),
    };
    let after_key = &body[key_start + key.len()..];
    let after_colon = after_key
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| Error::BadJson(format!("expected ':' after \"{field}\"")))?;
    let value = after_colon.trim_start();
    let body_quoted = value
        .strip_prefix('"')
        .ok_or_else(|| Error::BadJson(format!("\"{field}\" value not a quoted string")))?;
    let close_quote = body_quoted
        .find('"')
        .ok_or_else(|| Error::BadJson(format!("unterminated \"{field}\" string")))?;
    Ok(Some(body_quoted[..close_quote].to_string()))
}

pub fn extract_x25519_from_json(body: &str) -> Result<[u8; 32]> {
    let key = "\"x25519\"";
    let key_start = body
        .find(key)
        .ok_or_else(|| Error::BadJson("missing \"x25519\" field".into()))?;
    let after_key = &body[key_start + key.len()..];
    let after_colon = after_key
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| Error::BadJson("expected ':' after \"x25519\"".into()))?;
    let value = after_colon.trim_start();
    let body_quoted = value
        .strip_prefix('"')
        .ok_or_else(|| Error::BadJson("\"x25519\" value not a quoted string".into()))?;
    let close_quote = body_quoted
        .find('"')
        .ok_or_else(|| Error::BadJson("unterminated \"x25519\" string".into()))?;
    let hex_str = &body_quoted[..close_quote];
    if hex_str.len() != 64 {
        return Err(Error::BadX25519(format!(
            "expected 64 hex chars, got {}",
            hex_str.len()
        )));
    }
    if !hex_str.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::BadX25519(format!(
            "non-hex chars in x25519: {hex_str}"
        )));
    }
    let bytes = hex::decode(hex_str).map_err(|e| Error::BadX25519(format!("hex decode: {e}")))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadX25519(format!("expected 32 bytes, got {}", bytes.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_body(hex: &str) -> String {
        format!(
            "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"ws://x:1\"}}\n",
            "11".repeat(32),
            hex,
        )
    }

    #[test]
    fn extracts_well_formed() {
        let hex = "ab".repeat(32);
        let bytes = extract_x25519_from_json(&good_body(&hex)).unwrap();
        assert_eq!(bytes, [0xab; 32]);
    }

    #[test]
    fn handles_whitespace_around_colon() {
        let hex = "cd".repeat(32);
        let body = format!(
            "{{\n  \"ed25519\" : \"{}\",\n  \"x25519\"  :  \"{}\",\n  \"address\":\"ws://x:1\"\n}}\n",
            "11".repeat(32),
            hex,
        );
        let bytes = extract_x25519_from_json(&body).unwrap();
        assert_eq!(bytes, [0xcd; 32]);
    }

    #[test]
    fn missing_field_rejected() {
        let body = "{\"ed25519\":\"00\",\"address\":\"ws://x:1\"}";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn malformed_no_quote_rejected() {
        let body = "{\"x25519\": notquoted}";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn missing_colon_rejected() {
        let body = "{\"x25519\" \"abcd\"}";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn unterminated_string_rejected() {
        let body = "{\"x25519\":\"deadbeef";
        assert!(matches!(
            extract_x25519_from_json(body),
            Err(Error::BadJson(_))
        ));
    }

    #[test]
    fn wrong_length_rejected() {
        let body = good_body("abcd");
        assert!(matches!(
            extract_x25519_from_json(&body),
            Err(Error::BadX25519(_))
        ));
    }

    #[test]
    fn non_hex_rejected() {
        let body = good_body(&"zz".repeat(32));
        assert!(matches!(
            extract_x25519_from_json(&body),
            Err(Error::BadX25519(_))
        ));
    }

    #[test]
    fn x25519_in_other_field_name_does_not_confuse_scanner() {
        // A future relay might add a "prev_x25519" field. The scanner
        // must still find the real "x25519" key — it's preceded by an
        // underscore there, not a double-quote, so the substring
        // search skips past it.
        let real = "ab".repeat(32);
        let body = format!(
            "{{\"prev_x25519\":\"00\",\"x25519\":\"{}\",\"address\":\"ws://x:1\"}}",
            real,
        );
        let bytes = extract_x25519_from_json(&body).unwrap();
        assert_eq!(bytes, [0xab; 32]);
    }

    #[test]
    fn x25519_inside_address_value_does_not_confuse_scanner() {
        // The relay's "address" field already contains a `#x25519=…`
        // fragment in its value. The scanner must look for the JSON
        // *key* (preceded by an opening quote), not the bare token.
        let real = "cd".repeat(32);
        let body = format!(
            "{{\"address\":\"ws://x:1#x25519=00\",\"x25519\":\"{}\",\"ed25519\":\"00\"}}",
            real,
        );
        let bytes = extract_x25519_from_json(&body).unwrap();
        assert_eq!(bytes, [0xcd; 32]);
    }
}
