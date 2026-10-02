//! Compact, bounded bytes used only by the managed protocol and journal.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserializer, Serializer};
use std::fmt;
pub const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_BATCH_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_CIPHERTEXT_BYTES: usize = MAX_BATCH_BYTES + 41;
fn serialize<S: Serializer>(bytes: &[u8], serializer: S, max: usize) -> Result<S::Ok, S::Error> {
    if bytes.len() > max {
        return Err(serde::ser::Error::custom("managed bytes exceed size bound"));
    }
    serializer.serialize_str(&STANDARD.encode(bytes))
}
fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
    max: usize,
) -> Result<Vec<u8>, D::Error> {
    struct BoundedBytes(usize);
    impl<'de> serde::de::Visitor<'de> for BoundedBytes {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("bounded base64 bytes")
        }
        fn visit_str<E: serde::de::Error>(self, encoded: &str) -> Result<Vec<u8>, E> {
            if encoded.len() > self.0.div_ceil(3) * 4 {
                return Err(E::custom("managed encoded bytes exceed size bound"));
            }
            let bytes = STANDARD.decode(encoded).map_err(E::custom)?;
            if bytes.len() > self.0 {
                return Err(E::custom("managed decoded bytes exceed size bound"));
            }
            Ok(bytes)
        }
    }
    deserializer.deserialize_str(BoundedBytes(max))
}
macro_rules! bounded_bytes {
    ($name:ident, $max:expr) => {
        pub(crate) mod $name {
            use super::*;
            pub fn serialize<S: Serializer>(
                bytes: &[u8],
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                super::serialize(bytes, serializer, $max)
            }
            pub fn deserialize<'de, D: Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Vec<u8>, D::Error> {
                super::deserialize(deserializer, $max)
            }
        }
    };
}
bounded_bytes!(record_bytes, MAX_RECORD_BYTES);
bounded_bytes!(ciphertext_bytes, MAX_CIPHERTEXT_BYTES);
bounded_bytes!(proof_bytes, 32);
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(serde::Deserialize)]
    struct Proof {
        #[serde(with = "proof_bytes")]
        bytes: Vec<u8>,
    }
    #[test]
    fn encoded_and_decoded_proof_bounds_reject_before_unbounded_allocation() {
        assert!(
            serde_json::from_str::<Proof>(&format!("{{\"bytes\":\"{}\"}}", "A".repeat(48)))
                .is_err()
        );
        let too_long = STANDARD.encode([255; 33]);
        assert!(serde_json::from_str::<Proof>(&format!("{{\"bytes\":\"{too_long}\"}}")).is_err());
        assert_eq!(
            serde_json::from_str::<Proof>(&format!(
                "{{\"bytes\":\"{}\"}}",
                STANDARD.encode([255; 32])
            ))
            .unwrap()
            .bytes
            .len(),
            32
        );
    }
}
