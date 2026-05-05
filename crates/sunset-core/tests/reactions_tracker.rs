//! End-to-end: spawn a reaction tracker over a MemoryStore, write
//! Reaction entries, observe whole-snapshot callbacks per logical
//! change.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rand_core::OsRng;
use sunset_core::crypto::constants::test_fast_params;
use sunset_core::reactions::{ReactionHandles, ReactionSnapshot, spawn_reaction_tracker};
use sunset_core::{Identity, ReactionAction, ReactionPayload, Room, compose_reaction};
use sunset_store::Store as _;

#[tokio::test(flavor = "current_thread")]
async fn tracker_fires_on_alice_reaction_then_remove() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let alice = Identity::generate(&mut OsRng);
            let bob = Identity::generate(&mut OsRng);
            let room = Room::open_with_params("general", &test_fast_params()).unwrap();
            let store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());

            // Bob's tracker watches the room.
            let handles = ReactionHandles::default();
            let observed: Rc<RefCell<Vec<(sunset_store::Hash, ReactionSnapshot)>>> =
                Rc::new(RefCell::new(Vec::new()));
            let observed_cb = observed.clone();
            *handles.on_reactions_changed.borrow_mut() = Some(Box::new(move |target, snapshot| {
                observed_cb.borrow_mut().push((*target, snapshot.clone()));
            }));
            spawn_reaction_tracker(
                store.clone(),
                room.clone(),
                room.fingerprint().to_hex(),
                handles.clone(),
            );

            // Alice composes a reaction targeting an arbitrary message hash.
            let target: sunset_store::Hash = blake3::hash(b"target message").into();

            let composed_add = compose_reaction(
                &alice,
                &room,
                0,
                100,
                sunset_core::ChannelLabel::default_general(),
                &ReactionPayload {
                    for_value_hash: target,
                    emoji: "👍",
                    action: ReactionAction::Add,
                },
                &mut OsRng,
            )
            .unwrap();
            store
                .insert(composed_add.entry, Some(composed_add.block))
                .await
                .unwrap();

            // Yield to let the spawned task drain the subscription.
            for _ in 0..10 {
                tokio::task::yield_now().await;
                if !observed.borrow().is_empty() {
                    break;
                }
            }
            assert_eq!(
                observed.borrow().len(),
                1,
                "tracker should fire once for Add"
            );
            let (fired_target, fired_snapshot) = observed.borrow()[0].clone();
            assert_eq!(fired_target, target);
            let alice_set = fired_snapshot.get("👍").unwrap();
            assert_eq!(
                alice_set.get(&alice.public()),
                Some(&100),
                "snapshot should expose the Add's sent_at_ms so the info panel can stamp the reaction"
            );

            // Alice removes the reaction.
            let composed_remove = compose_reaction(
                &alice,
                &room,
                0,
                200,
                sunset_core::ChannelLabel::default_general(),
                &ReactionPayload {
                    for_value_hash: target,
                    emoji: "👍",
                    action: ReactionAction::Remove,
                },
                &mut OsRng,
            )
            .unwrap();
            store
                .insert(composed_remove.entry, Some(composed_remove.block))
                .await
                .unwrap();

            for _ in 0..10 {
                tokio::task::yield_now().await;
                if observed.borrow().len() >= 2 {
                    break;
                }
            }
            assert_eq!(
                observed.borrow().len(),
                2,
                "tracker should fire again for Remove"
            );
            let (_, fired_snapshot_2) = observed.borrow()[1].clone();
            assert!(
                fired_snapshot_2
                    .get("👍")
                    .map(|s| s.is_empty())
                    .unwrap_or(true),
                "Remove should yield an empty snapshot for 👍"
            );

            // Suppress unused-variable warnings for bob (kept for symmetry with
            // future tests where bob also reacts).
            let _ = bob;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tracker_debounces_duplicate_state() {
    // Two consecutive Adds with different timestamps but same outcome
    // should fire twice (signature changes only on outcome change), but
    // re-applying the same event twice (e.g., from Replay::All) must
    // NOT fire a redundant callback.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let alice = Identity::generate(&mut OsRng);
            let room = Room::open_with_params("general", &test_fast_params()).unwrap();
            let store = Arc::new(sunset_store_memory::MemoryStore::with_accept_all());

            let handles = ReactionHandles::default();
            let observed: Rc<RefCell<Vec<sunset_store::Hash>>> = Rc::new(RefCell::new(Vec::new()));
            let observed_cb = observed.clone();
            *handles.on_reactions_changed.borrow_mut() =
                Some(Box::new(move |target, _snapshot| {
                    observed_cb.borrow_mut().push(*target);
                }));
            spawn_reaction_tracker(
                store.clone(),
                room.clone(),
                room.fingerprint().to_hex(),
                handles.clone(),
            );

            let target: sunset_store::Hash = blake3::hash(b"target").into();
            let composed = compose_reaction(
                &alice,
                &room,
                0,
                100,
                sunset_core::ChannelLabel::default_general(),
                &ReactionPayload {
                    for_value_hash: target,
                    emoji: "👍",
                    action: ReactionAction::Add,
                },
                &mut OsRng,
            )
            .unwrap();
            // Insert the same entry twice (the second insert is a no-op at the
            // store level — same value_hash); the tracker should also not
            // double-fire.
            store
                .insert(composed.entry.clone(), Some(composed.block.clone()))
                .await
                .unwrap();
            let _ = store
                .insert(composed.entry.clone(), Some(composed.block.clone()))
                .await;

            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                observed.borrow().len(),
                1,
                "duplicate insert must not double-fire"
            );
        })
        .await;
}
