//! Allocation-free validation of mergeable-container map markers.
//!
//! `loro_common::parse_mergeable_marker` assembles a temporary byte vector and
//! evaluates IEEE CRC-32 one bit at a time. Map reads invoke marker translation
//! for every mergeable child, making that validation a projection hot path.
//! Keep the wire-compatible validation here, but stream the exact encoded
//! segments into `crc32fast` without allocating.

use crc32fast::Hasher;
use loro_common::{
    ContainerID, ContainerType, LoroValue, MERGEABLE_MARKER_MAGIC,
};

const MARKER_DIGEST_LEN: usize = 3;
const MARKER_LEN: usize = MERGEABLE_MARKER_MAGIC.len() + 1 + MARKER_DIGEST_LEN;
const CRC_DOMAIN: &[u8] = b"loro.mergeable.marker.v1";

pub(crate) fn parse_mergeable_marker(
    parent: &ContainerID,
    key: &str,
    value: &LoroValue,
) -> Option<ContainerType> {
    let LoroValue::Binary(bytes) = value else {
        return None;
    };
    if bytes.len() != MARKER_LEN || !bytes.starts_with(&MERGEABLE_MARKER_MAGIC) {
        return None;
    }

    let kind = ContainerType::try_from_u8(bytes[MERGEABLE_MARKER_MAGIC.len()]).ok()?;
    if matches!(kind, ContainerType::Unknown(_)) {
        return None;
    }

    let digest_start = MERGEABLE_MARKER_MAGIC.len() + 1;
    let expected = mergeable_marker_crc24(parent, key, kind);
    (&bytes[digest_start..] == expected.as_slice()).then_some(kind)
}

pub(crate) fn translate_mergeable_marker_value(
    parent: &ContainerID,
    key: &str,
    value: LoroValue,
) -> LoroValue {
    match parse_mergeable_marker(parent, key, &value) {
        Some(kind) => LoroValue::Container(ContainerID::new_mergeable(parent, key, kind)),
        None => value,
    }
}

fn mergeable_marker_crc24(
    parent: &ContainerID,
    key: &str,
    kind: ContainerType,
) -> [u8; MARKER_DIGEST_LEN] {
    let mut hasher = Hasher::new();
    hasher.update(CRC_DOMAIN);
    update_len_prefixed_parent(&mut hasher, parent);
    update_unsigned(&mut hasher, key.len());
    hasher.update(key.as_bytes());
    hasher.update(&[kind.to_u8()]);

    let crc = hasher.finalize() & 0x00ff_ffff;
    [
        ((crc >> 16) & 0xff) as u8,
        ((crc >> 8) & 0xff) as u8,
        (crc & 0xff) as u8,
    ]
}

/// Feed the bytes produced by `ContainerID::encode`, preceded by their unsigned
/// LEB128 length, without materializing `ContainerID::to_bytes()`.
fn update_len_prefixed_parent(hasher: &mut Hasher, parent: &ContainerID) {
    let encoded_len = match parent {
        ContainerID::Root { name, .. } => {
            1 + unsigned_len(name.len()) + name.len()
        }
        ContainerID::Normal { .. } => 13,
    };
    update_unsigned(hasher, encoded_len);

    match parent {
        ContainerID::Root {
            name,
            container_type,
        } => {
            hasher.update(&[container_type.to_u8() | 0b1000_0000]);
            update_unsigned(hasher, name.len());
            hasher.update(name.as_bytes());
        }
        ContainerID::Normal {
            peer,
            counter,
            container_type,
        } => {
            hasher.update(&[container_type.to_u8()]);
            hasher.update(&peer.to_le_bytes());
            hasher.update(&counter.to_le_bytes());
        }
    }
}

fn unsigned_len(mut value: usize) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

fn update_unsigned(hasher: &mut Hasher, mut value: usize) {
    let mut bytes = [0_u8; 10];
    let mut len = 0;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes[len] = byte;
        len += 1;
        if value == 0 {
            break;
        }
    }
    hasher.update(&bytes[..len]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_marker_parser_matches_loro_common() {
        let root = ContainerID::Root {
            name: "root".into(),
            container_type: ContainerType::Map,
        };
        let normal = ContainerID::Normal {
            peer: u64::MAX - 7,
            counter: -42,
            container_type: ContainerType::Map,
        };
        let nested = ContainerID::new_mergeable(
            &root,
            "nested/key>with\\escapes",
            ContainerType::Map,
        );

        for parent in [&root, &normal, &nested] {
            for key in ["plain", "slash/key", "angle>key", r"back\slash", "émoji-🤝"] {
                for kind in ContainerType::ALL_TYPES {
                    let marker = loro_common::mergeable_marker(parent, key, kind);
                    assert_eq!(
                        parse_mergeable_marker(parent, key, &marker),
                        loro_common::parse_mergeable_marker(parent, key, &marker),
                    );
                    assert_eq!(
                        translate_mergeable_marker_value(parent, key, marker.clone()),
                        loro_common::translate_mergeable_marker_value(parent, key, marker),
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_values_remain_scalars() {
        let parent = ContainerID::Root {
            name: "root".into(),
            container_type: ContainerType::Map,
        };
        let key = "child";
        let mut corrupt = loro_common::mergeable_marker(
            &parent,
            key,
            ContainerType::Map,
        );
        let LoroValue::Binary(bytes) = &mut corrupt else {
            unreachable!();
        };
        let mut bytes = bytes.to_vec();
        bytes[MARKER_LEN - 1] ^= 0xff;
        corrupt = LoroValue::Binary(bytes.into());

        assert_eq!(parse_mergeable_marker(&parent, key, &corrupt), None);
        assert_eq!(
            translate_mergeable_marker_value(&parent, key, corrupt.clone()),
            corrupt,
        );
        assert_eq!(
            translate_mergeable_marker_value(
                &parent,
                key,
                LoroValue::String("not a marker".into()),
            ),
            LoroValue::String("not a marker".into()),
        );
    }
}
