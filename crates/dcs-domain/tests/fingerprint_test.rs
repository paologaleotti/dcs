use dcs_domain::fingerprint::ContentFingerprint;

#[test]
fn hex_round_trips() {
    let bytes: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(3));
    let fp = ContentFingerprint::from_bytes(bytes);
    let hex = fp.to_hex();
    assert_eq!(hex.len(), 64);
    assert_eq!(ContentFingerprint::from_hex(&hex), Some(fp));
}

#[test]
fn json_round_trips_as_hex_string() {
    let fp = ContentFingerprint::from_bytes([0xab; 32]);
    let json = serde_json::to_string(&fp).unwrap();
    // Serialized form is a plain hex string, not a byte array — keeps the
    // project file human-readable and diffable (§5).
    assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
    let back: ContentFingerprint = serde_json::from_str(&json).unwrap();
    assert_eq!(back, fp);
}

#[test]
fn parses_uppercase_hex() {
    let lower = "ab".repeat(32);
    let upper = lower.to_uppercase();
    assert_eq!(
        ContentFingerprint::from_hex(&upper),
        ContentFingerprint::from_hex(&lower)
    );
}

#[test]
fn rejects_wrong_length_and_non_hex() {
    assert_eq!(ContentFingerprint::from_hex(""), None);
    assert_eq!(ContentFingerprint::from_hex(&"a".repeat(63)), None);
    assert_eq!(ContentFingerprint::from_hex(&"a".repeat(65)), None);
    assert_eq!(ContentFingerprint::from_hex(&"g".repeat(64)), None);
}

#[test]
fn as_bytes_returns_the_digest() {
    let bytes = [9u8; 32];
    assert_eq!(ContentFingerprint::from_bytes(bytes).as_bytes(), &bytes);
}
