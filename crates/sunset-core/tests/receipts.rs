//! End-to-end test: Alice sends a Text, Bob "auto-acks" it via a
//! manually-driven loop (mirroring the wasm bridge), Alice picks
//! up the Receipt and can identify Bob as a confirmer.

use rand_core::OsRng;
use sunset_core::crypto::constants::test_fast_params;
use sunset_core::{Identity, MessageBody, Room, compose_receipt, compose_text, decode_message};
use sunset_store::Store as _;

#[tokio::test(flavor = "current_thread")]
async fn receipt_round_trip_between_two_identities() {
    let alice = Identity::generate(&mut OsRng);
    let bob = Identity::generate(&mut OsRng);
    let room = Room::open_with_params("general", &test_fast_params()).unwrap();

    let alice_store = sunset_store_memory::MemoryStore::with_accept_all();
    let bob_store = sunset_store_memory::MemoryStore::with_accept_all();

    // 1. Alice composes and inserts a Text.
    let text = compose_text(
        &alice,
        &room,
        0,
        1,
        sunset_core::ChannelLabel::default_general(),
        "hello bob",
        &mut OsRng,
    )
    .unwrap();
    alice_store
        .insert(text.entry.clone(), Some(text.block.clone()))
        .await
        .unwrap();

    // 2. Simulate sync: same entry shows up in Bob's store.
    bob_store
        .insert(text.entry.clone(), Some(text.block.clone()))
        .await
        .unwrap();

    // 3. Bob's bridge logic: decode Alice's Text. Since author != self,
    //    Bob composes a Receipt referencing the text's value_hash.
    let decoded = decode_message(&room, &text.entry, &text.block).unwrap();
    assert!(matches!(decoded.body, MessageBody::Text(_)));
    let receipt = compose_receipt(
        &bob,
        &room,
        0,
        2,
        sunset_core::ChannelLabel::default_general(),
        decoded.value_hash,
        &mut OsRng,
    )
    .unwrap();
    bob_store
        .insert(receipt.entry.clone(), Some(receipt.block.clone()))
        .await
        .unwrap();

    // 4. Sync the receipt back to Alice's store.
    alice_store
        .insert(receipt.entry.clone(), Some(receipt.block.clone()))
        .await
        .unwrap();

    // 5. Alice's bridge logic: decode the receipt; confirm it
    //    references her text and is signed by Bob.
    let receipt_decoded = decode_message(&room, &receipt.entry, &receipt.block).unwrap();
    assert_eq!(receipt_decoded.author_key, bob.public());
    match receipt_decoded.body {
        MessageBody::Receipt { for_value_hash } => {
            assert_eq!(for_value_hash, text.entry.value_hash);
        }
        _ => panic!("expected Receipt body"),
    }
}
