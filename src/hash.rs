//! FNV-1a over bytes, the one hash the supervisor uses: to reduce a replica's
//! response to the 64-bit value the `quorum` crate votes on, and to name a
//! configuration in the checkpoint.

pub const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0100_0000_01b3;

/// FNV-1a 64 continued from `seed` over `bytes`.
pub fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut h = seed;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// FNV-1a 64 over `bytes` from the standard offset.
pub fn of(bytes: &[u8]) -> u64 {
    fnv1a(OFFSET, bytes)
}

/// The value a reply votes with: the hash of the output and the next state,
/// each behind its length, so a byte cannot move from one to the other
/// without changing the value.
pub fn reply_value(output: &[u8], state: &[u8]) -> u64 {
    let len = |b: &[u8]| (b.len() as u32).to_le_bytes();
    let h = fnv1a(OFFSET, &len(output));
    let h = fnv1a(h, output);
    let h = fnv1a(h, &len(state));
    fnv1a(h, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // FNV-1a 64 test vectors
        assert_eq!(of(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(of(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(of(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn empty_reply_is_the_hash_of_two_zero_lengths() {
        assert_eq!(reply_value(b"", b""), of(&[0, 0, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn a_byte_moved_between_output_and_state_changes_the_value() {
        assert_ne!(reply_value(b"ab", b"c"), reply_value(b"a", b"bc"));
        assert_ne!(reply_value(b"abc", b""), reply_value(b"", b"abc"));
    }
}
