//! End-to-end: Alice reacts 👍 on a message; Bob's tracker sees the
//! snapshot. Alice removes; Bob's tracker sees the empty snapshot.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rand_core::OsRng;
use sunset_core::crypto::constants::test_fast_params;
use sunset_core::reactions::{ReactionHandles, ReactionSnapshot, spawn_reaction_tracker};
use sunset_core::{
    Identity, ReactionAction, ReactionPayload, Room, compose_reaction, compose_text,
};
use sunset_store::Store as _;

#[tokio::test(flavor = "current_thread")]
async fn reaction_round_trip_between_two_identities() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let alice = Identity::generate(&mut OsRng);
            let bob = Identity::generate(&mut OsRng);
            let room = Room::open_with_params("general", &test_fast_params()).unwrap();

            let alice_store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());
            let bob_store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());

            // Bob runs a tracker on his store.
            let bob_handles = ReactionHandles::default();
            let observed: Rc<RefCell<Vec<ReactionSnapshot>>> = Rc::new(RefCell::new(Vec::new()));
            let observed_cb = observed.clone();
            *bob_handles.on_reactions_changed.borrow_mut() =
                Some(Box::new(move |_target, snapshot| {
                    observed_cb.borrow_mut().push(snapshot.clone());
                }));
            spawn_reaction_tracker(
                bob_store.clone(),
                room.clone(),
                room.fingerprint().to_hex(),
                bob_handles.clone(),
            );

            // 1. Alice composes a Text and inserts it on her store; sync to bob.
            let text = compose_text(&alice, &room, 0, 1, "hello bob", &mut OsRng).unwrap();
            let target = text.entry.value_hash;
            alice_store
                .insert(text.entry.clone(), Some(text.block.clone()))
                .await
                .unwrap();
            bob_store
                .insert(text.entry.clone(), Some(text.block.clone()))
                .await
                .unwrap();

            // 2. Alice reacts 👍 on her own message.
            let add = compose_reaction(
                &alice,
                &room,
                0,
                100,
                &ReactionPayload {
                    for_value_hash: target,
                    emoji: "👍",
                    action: ReactionAction::Add,
                },
                &mut OsRng,
            )
            .unwrap();
            alice_store
                .insert(add.entry.clone(), Some(add.block.clone()))
                .await
                .unwrap();

            // 3. Sync to Bob.
            bob_store
                .insert(add.entry.clone(), Some(add.block.clone()))
                .await
                .unwrap();

            // 4. Bob's tracker fires.
            for _ in 0..10 {
                tokio::task::yield_now().await;
                if !observed.borrow().is_empty() {
                    break;
                }
            }
            assert_eq!(observed.borrow().len(), 1);
            let snap = observed.borrow()[0].clone();
            let alice_set = snap.get("👍").unwrap();
            assert!(alice_set.contains(&alice.public()));

            // 5. Alice removes.
            let remove = compose_reaction(
                &alice,
                &room,
                0,
                200,
                &ReactionPayload {
                    for_value_hash: target,
                    emoji: "👍",
                    action: ReactionAction::Remove,
                },
                &mut OsRng,
            )
            .unwrap();
            alice_store
                .insert(remove.entry.clone(), Some(remove.block.clone()))
                .await
                .unwrap();
            bob_store
                .insert(remove.entry.clone(), Some(remove.block.clone()))
                .await
                .unwrap();

            for _ in 0..10 {
                tokio::task::yield_now().await;
                if observed.borrow().len() >= 2 {
                    break;
                }
            }
            assert_eq!(observed.borrow().len(), 2);
            let snap2 = observed.borrow()[1].clone();
            assert!(snap2.get("👍").map(|s| s.is_empty()).unwrap_or(true));

            let _ = bob; // bob is reserved for symmetry — used in expanded versions
        })
        .await;
}
