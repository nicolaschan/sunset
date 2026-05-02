//! Per-room membership state derived from heartbeat presence entries
//! and engine peer events. Pure data + reducer functions; the
//! orchestrating task lives in `membership_tracker.rs`.

use std::collections::HashMap;

use wasm_bindgen::prelude::*;

use sunset_sync::{PeerId, TransportKind};

/// Three-state presence bucket derived from heartbeat age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Online,
    Away,
    Offline,
}

impl Presence {
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
    /// Unix-ms timestamp of the last app-level presence heartbeat we
    /// observed for this peer. `None` for self (we don't track our own
    /// presence) and for any peer we've heard nothing from. The Gleam
    /// popover computes age = now_ms - last_heartbeat_ms.
    pub(crate) last_heartbeat_ms: Option<u64>,
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
    /// Heartbeat timestamp as `f64` (JS Number), or `-1` for "no
    /// heartbeat observed" (i.e. self, or a peer we've heard nothing
    /// from). We expose `f64` rather than `Option<u64>` because
    /// wasm-bindgen serializes `Option<u64>` as `bigint | undefined`
    /// in JS — the BigInt half then doesn't mix with regular Number
    /// arithmetic on the Gleam side. `f64` round-trips unix-ms
    /// exactly out to ~285616 AD, which is plenty for our purposes.
    #[wasm_bindgen(getter)]
    pub fn last_heartbeat_ms(&self) -> f64 {
        match self.last_heartbeat_ms {
            Some(ms) => ms as f64,
            None => -1.0,
        }
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
        last_heartbeat_ms: None,
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
    // V1 single-relay topology assumption: if we hold any Primary
    // transport (i.e. an open relay connection), then a peer for whom
    // we have no direct/relay transport entry yet — most commonly
    // because the relay forwarded their presence entry without us
    // building a transport to them — is reachable "via_relay".
    // Revisit when multi-relay/federated routing lands: the right
    // model then is to track which specific relay forwarded each
    // heartbeat and key the via_relay decision on whether that relay
    // is currently in peer_kinds.
    let any_relay = peer_kinds.values().any(|k| *k == TransportKind::Primary);
    for (pk, last_ms) in others {
        let age = now_ms.saturating_sub(*last_ms);
        let presence = presence_bucket(age, interval_ms, ttl_ms);
        if presence == Presence::Offline {
            continue;
        }
        let connection_mode = match peer_kinds.get(pk) {
            Some(TransportKind::Secondary) => "direct",
            Some(TransportKind::Primary) => "via_relay",
            _ if any_relay => "via_relay",
            _ => "unknown",
        }
        .to_owned();
        out.push(MemberJs {
            pubkey: pk.verifying_key().as_bytes().to_vec(),
            presence: presence.as_str().to_owned(),
            connection_mode,
            is_self: false,
            last_heartbeat_ms: Some(*last_ms),
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
        // dave: no kind, but a Primary (bob) exists → dave's presence was
        // forwarded by the relay → "via_relay".
        let out = derive_members(200, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 4);
        let modes: Vec<&str> = out.iter().map(|m| m.connection_mode.as_str()).collect();
        assert_eq!(modes, vec!["self", "via_relay", "direct", "via_relay"]);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn derive_members_unknown_when_no_relay_no_kind() {
        let me = pk(1);
        let dave = pk(4);
        let mut presence = HashMap::new();
        presence.insert(dave.clone(), 100);
        // No Primary in peer_kinds → dave's presence has no traceable
        // route → "unknown".
        let kinds = HashMap::new();
        let out = derive_members(200, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].connection_mode, "unknown");
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

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn derive_members_includes_last_heartbeat_for_others() {
        use std::collections::HashMap;
        let me = pk(0);
        let bob = pk(1);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 12_345_u64);
        let kinds = HashMap::new();
        let out = derive_members(20_000, 30_000, 60_000, &me, &presence, &kinds);
        // Self is index 0 (always present); Bob is index 1.
        assert_eq!(out[0].is_self, true);
        assert_eq!(out[0].last_heartbeat_ms, None);
        assert_eq!(out[1].is_self, false);
        assert_eq!(out[1].last_heartbeat_ms, Some(12_345_u64));
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn signature_ignores_heartbeat_timestamp() {
        // Two member lists differing ONLY in last_heartbeat_ms must
        // produce equal signatures, otherwise the membership tracker
        // would re-emit on every age tick.
        let m1 = MemberJs {
            pubkey: vec![1; 32],
            presence: "online".to_owned(),
            connection_mode: "via_relay".to_owned(),
            is_self: false,
            last_heartbeat_ms: Some(100),
        };
        let m2 = MemberJs {
            pubkey: vec![1; 32],
            presence: "online".to_owned(),
            connection_mode: "via_relay".to_owned(),
            is_self: false,
            last_heartbeat_ms: Some(200),
        };
        assert_eq!(members_signature(&[m1]), members_signature(&[m2]));
    }
}
