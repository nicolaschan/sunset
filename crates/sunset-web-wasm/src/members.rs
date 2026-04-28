//! Per-room membership state derived from heartbeat presence entries
//! and engine peer events. Pure data + reducer functions; the
//! orchestrating task lives in `membership_tracker.rs`.
//!
//! Items in this module are consumed by `membership_tracker` (Task 7);
//! `#[allow(dead_code)]` is applied to keep clippy clean while the
//! consumer is being landed.

use std::collections::HashMap;

use wasm_bindgen::prelude::*;

use sunset_sync::{PeerId, TransportKind};

/// Three-state presence bucket derived from heartbeat age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Presence {
    Online,
    Away,
    Offline,
}

impl Presence {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Online => "online",
            Presence::Away => "away",
            Presence::Offline => "offline",
        }
    }
}

/// Bucket a heartbeat age into Online / Away / Offline.
///
/// - `age_ms < interval_ms`         → Online
/// - `interval_ms ≤ age_ms < ttl_ms` → Away
/// - `age_ms ≥ ttl_ms`              → Offline (caller drops member from list)
pub fn presence_bucket(age_ms: u64, interval_ms: u64, ttl_ms: u64) -> Presence {
    if age_ms < interval_ms {
        Presence::Online
    } else if age_ms < ttl_ms {
        Presence::Away
    } else {
        Presence::Offline
    }
}

/// JS-exported per-member view consumed by the Gleam UI.
#[wasm_bindgen]
pub struct MemberJs {
    pub(crate) pubkey: Vec<u8>,
    pub(crate) presence: String,
    pub(crate) connection_mode: String,
    pub(crate) is_self: bool,
}

#[wasm_bindgen]
impl MemberJs {
    #[wasm_bindgen(getter)]
    pub fn pubkey(&self) -> Vec<u8> {
        self.pubkey.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn presence(&self) -> String {
        self.presence.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn connection_mode(&self) -> String {
        self.connection_mode.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn is_self(&self) -> bool {
        self.is_self
    }
}

/// Pure derivation: given the current state, return the rendered
/// member list. Self is always present and always Online.
pub fn derive_members(
    now_ms: u64,
    interval_ms: u64,
    ttl_ms: u64,
    self_peer: &PeerId,
    presence_map: &HashMap<PeerId, u64>,
    peer_kinds: &HashMap<PeerId, TransportKind>,
) -> Vec<MemberJs> {
    let mut out = Vec::new();
    // Self always first.
    out.push(MemberJs {
        pubkey: self_peer.verifying_key().as_bytes().to_vec(),
        presence: Presence::Online.as_str().to_owned(),
        connection_mode: "self".to_owned(),
        is_self: true,
    });
    // Others, sorted by pubkey for stable ordering.
    let mut others: Vec<(&PeerId, &u64)> = presence_map
        .iter()
        .filter(|(pk, _)| *pk != self_peer)
        .collect();
    others.sort_by(|(a, _), (b, _)| {
        a.verifying_key()
            .as_bytes()
            .cmp(b.verifying_key().as_bytes())
    });
    for (pk, last_ms) in others {
        let age = now_ms.saturating_sub(*last_ms);
        let presence = presence_bucket(age, interval_ms, ttl_ms);
        if presence == Presence::Offline {
            continue;
        }
        let connection_mode = match peer_kinds.get(pk) {
            Some(TransportKind::Secondary) => "direct",
            Some(TransportKind::Primary) => "via_relay",
            _ => "unknown",
        }
        .to_owned();
        out.push(MemberJs {
            pubkey: pk.verifying_key().as_bytes().to_vec(),
            presence: presence.as_str().to_owned(),
            connection_mode,
            is_self: false,
        });
    }
    out
}

/// Stable shape signature used to debounce callbacks. The tracker
/// compares the current signature with the previously-emitted one
/// and only fires the callback if it changed.
pub fn members_signature(members: &[MemberJs]) -> Vec<(Vec<u8>, String, String)> {
    members
        .iter()
        .map(|m| {
            (
                m.pubkey.clone(),
                m.presence.clone(),
                m.connection_mode.clone(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn pk(b: u8) -> PeerId {
        PeerId(VerifyingKey::new(Bytes::copy_from_slice(&[b; 32])))
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn presence_bucket_thresholds() {
        assert_eq!(presence_bucket(0, 1000, 3000), Presence::Online);
        assert_eq!(presence_bucket(999, 1000, 3000), Presence::Online);
        assert_eq!(presence_bucket(1000, 1000, 3000), Presence::Away);
        assert_eq!(presence_bucket(2999, 1000, 3000), Presence::Away);
        assert_eq!(presence_bucket(3000, 1000, 3000), Presence::Offline);
        assert_eq!(presence_bucket(10_000, 1000, 3000), Presence::Offline);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn derive_members_self_only_when_no_peers() {
        let me = pk(1);
        let presence = HashMap::new();
        let kinds = HashMap::new();
        let out = derive_members(0, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_self);
        assert_eq!(out[0].presence, "online");
        assert_eq!(out[0].connection_mode, "self");
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn derive_members_skips_offline_peers() {
        let me = pk(1);
        let bob = pk(2);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 0u64);
        let kinds = HashMap::new();
        // bob's heartbeat is 5s old but ttl is 3s → Offline → dropped.
        let out = derive_members(5000, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_self);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn derive_members_maps_kinds_to_modes() {
        let me = pk(1);
        let bob = pk(2);
        let carol = pk(3);
        let dave = pk(4);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 100);
        presence.insert(carol.clone(), 100);
        presence.insert(dave.clone(), 100);
        let mut kinds = HashMap::new();
        kinds.insert(bob.clone(), TransportKind::Primary);
        kinds.insert(carol.clone(), TransportKind::Secondary);
        // dave: no kind → "unknown"
        let out = derive_members(200, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 4);
        let modes: Vec<&str> = out.iter().map(|m| m.connection_mode.as_str()).collect();
        assert_eq!(modes, vec!["self", "via_relay", "direct", "unknown"]);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn members_signature_changes_on_presence_change() {
        let me = pk(1);
        let bob = pk(2);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 0);
        let kinds = HashMap::new();

        let s1 = members_signature(&derive_members(500, 1000, 3000, &me, &presence, &kinds));
        let s2 = members_signature(&derive_members(1500, 1000, 3000, &me, &presence, &kinds));
        assert_ne!(s1, s2, "Online → Away should change signature");
    }
}
