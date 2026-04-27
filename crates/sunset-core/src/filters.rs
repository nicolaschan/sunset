//! Sync interest-set helpers.

use bytes::Bytes;

use sunset_store::Filter;

use crate::crypto::room::Room;

/// All messages currently in (or arriving in) the given room.
///
/// Pairs with the name format chosen by `compose_message`:
///   `<hex(room_fingerprint)>/msg/<hex(value_hash)>`.
pub fn room_messages_filter(room: &Room) -> Filter {
    Filter::NamePrefix(Bytes::from(format!("{}/msg/", room.fingerprint().to_hex())))
}

#[cfg(test)]
mod tests {
    use rand_core::OsRng;

    use sunset_store::VerifyingKey;

    use crate::crypto::constants::test_fast_params;
    use crate::identity::Identity;
    use crate::message::compose_message;

    use super::*;

    fn general() -> Room {
        Room::open_with_params("general", &test_fast_params()).unwrap()
    }

    #[test]
    fn matches_a_composed_message_in_the_same_room() {
        let id = Identity::generate(&mut OsRng);
        let room = general();
        let composed = compose_message(&id, &room, 0, 1, "x", &mut OsRng).unwrap();

        let filter = room_messages_filter(&room);
        assert!(filter.matches(&composed.entry.verifying_key, &composed.entry.name));
    }

    #[test]
    fn rejects_a_message_in_a_different_room() {
        let id = Identity::generate(&mut OsRng);
        let alice_room = general();
        let other_room = Room::open_with_params("other", &test_fast_params()).unwrap();
        let composed = compose_message(&id, &alice_room, 0, 1, "x", &mut OsRng).unwrap();

        let filter = room_messages_filter(&other_room);
        assert!(!filter.matches(&composed.entry.verifying_key, &composed.entry.name));
    }

    #[test]
    fn rejects_unrelated_namespaces() {
        let room = general();
        let filter = room_messages_filter(&room);
        let vk = VerifyingKey::new(Bytes::from_static(b"anyone"));
        assert!(!filter.matches(&vk, b"presence/anything"));
    }
}
