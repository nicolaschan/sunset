//! End-to-end bridge test: compose a real signed+encrypted message through
//! the WASM bridge, then decode it back, asserting the author + body
//! survive the round trip.
//!
//! Runs under `wasm-pack test --node`. The native unit tests in
//! `sunset-core` already cover the underlying logic (134 tests); this
//! test confirms the wasm-bindgen marshaling for each export round-trips
//! correctly.

use sunset_core_wasm::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_node_experimental);

#[wasm_bindgen_test]
fn alice_composes_bob_decodes() {
    // Deterministic seeds for reproducibility.
    let alice_seed = [1u8; 32];
    let nonce_seed = [3u8; 32];

    let alice = identity_generate(&alice_seed).expect("identity_generate");

    // Re-derive alice's public key independently and compare.
    let alice_pub_again =
        identity_public_from_secret(&alice.secret).expect("identity_public_from_secret");
    assert_eq!(alice.public, alice_pub_again);

    // Two opens of the same room name yield the same fingerprint + secrets.
    let alice_room = room_open("plan-a-test-room").expect("alice room_open");
    let bob_room = room_open("plan-a-test-room").expect("bob room_open");
    assert_eq!(alice_room.fingerprint, bob_room.fingerprint);
    assert_eq!(alice_room.k_room, bob_room.k_room);
    assert_eq!(alice_room.epoch_0_root, bob_room.epoch_0_root);

    // Filter prefix is `<hex_fingerprint>/msg/`.
    let prefix = room_messages_filter_prefix(&alice_room.fingerprint).expect("filter prefix");
    let prefix_str = std::str::from_utf8(&prefix).expect("prefix is utf-8");
    assert!(prefix_str.ends_with("/msg/"));
    assert_eq!(prefix_str.len(), 64 + "/msg/".len());

    // alice composes a real encrypted+signed message.
    let body = "hello bob via wasm bridge";
    let sent_at = 1_700_000_000_000u64;
    let composed = compose_message(
        &alice.secret,
        "plan-a-test-room",
        0,
        sent_at,
        body,
        &nonce_seed,
    )
    .expect("compose_message");

    // outer Ed25519 sig passes (independent of decode).
    verify_entry_signature(&composed.entry).expect("verify_entry_signature");

    // bob decodes (using bob's separately-opened room).
    let decoded = decode_message("plan-a-test-room", &composed.entry, &composed.block)
        .expect("decode_message");

    assert_eq!(decoded.author_pubkey, alice.public);
    assert_eq!(decoded.epoch_id, 0u64);
    assert_eq!(decoded.sent_at_ms, sent_at);
    assert_eq!(decoded.body, body);
}

#[wasm_bindgen_test]
fn identity_generate_rejects_short_seed() {
    let bad_seed = [0u8; 7];
    let err = identity_generate(&bad_seed).expect_err("short seed must fail");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("32 bytes"),
        "error should mention 32-byte requirement: {msg}"
    );
}

#[wasm_bindgen_test]
fn decode_rejects_wrong_room() {
    let alice_seed = [1u8; 32];
    let nonce_seed = [3u8; 32];
    let alice = identity_generate(&alice_seed).expect("identity_generate");

    let composed = compose_message(&alice.secret, "alice-room", 0, 1u64, "x", &nonce_seed)
        .expect("compose_message");

    // Bob opens a different room and attempts to decode.
    let err = decode_message("eve-room", &composed.entry, &composed.block)
        .expect_err("decode with wrong room must fail");
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("sunset-core"),
        "error should be a sunset-core error: {msg}"
    );
}
