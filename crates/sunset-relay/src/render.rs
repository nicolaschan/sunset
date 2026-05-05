//! Pure-function renderers used by the axum handlers. Inputs are the
//! `Send` POD snapshots from `bridge.rs`; outputs are HTML/JSON strings.
//! No engine handles or `Rc`s here.

use bytes::Bytes;

use sunset_store::{Filter, VerifyingKey};
use sunset_sync::PeerId;

use crate::bridge::{DashboardSnapshot, IdentitySnapshot};

/// Plaintext dashboard body. Same shape and content as the original
/// `status.rs::render`, just driven from a snapshot instead of an
/// `Rc<Engine>`.
pub fn render_dashboard(snap: &DashboardSnapshot) -> String {
    let mut out = String::new();
    out.push_str("sunset-relay\n");
    out.push_str("============\n\n");

    out.push_str("identity\n--------\n");
    out.push_str(&format!("ed25519:  {}\n", hex::encode(snap.ed25519_public)));
    out.push_str(&format!("x25519:   {}\n", hex::encode(snap.x25519_public)));
    out.push_str(&format!("listen:   ws://{}\n", snap.listen_addr));
    out.push_str(&format!("dial:     {}\n\n", snap.dial_url));

    out.push_str("peers\n-----\n");
    if snap.configured_peers.is_empty() {
        out.push_str("configured federated peers: (none)\n");
    } else {
        out.push_str(&format!(
            "configured federated peers ({}):\n",
            snap.configured_peers.len()
        ));
        for p in &snap.configured_peers {
            out.push_str(&format!("  - {}\n", p));
        }
    }
    if snap.connected_peers.is_empty() {
        out.push_str("connected peers:            (none)\n");
    } else {
        out.push_str(&format!(
            "connected peers ({}):\n",
            snap.connected_peers.len()
        ));
        for p in &snap.connected_peers {
            out.push_str(&format!("  - ed25519:{}\n", peer_short(p)));
        }
    }
    out.push('\n');

    out.push_str("subscriptions (advertised by connected peers)\n");
    out.push_str("---------------------------------------------\n");
    if snap.subscriptions.is_empty() {
        out.push_str("(none)\n\n");
    } else {
        for (peer, filter) in &snap.subscriptions {
            out.push_str(&format!(
                "  ed25519:{} -> {}\n",
                peer_short(peer),
                format_filter(filter)
            ));
        }
        out.push('\n');
    }

    out.push_str("store\n-----\n");
    let stats = &snap.store_stats;
    out.push_str(&format!(
        "data dir:           {}\n",
        snap.data_dir.display()
    ));
    out.push_str(&format!(
        "on-disk size:       {}\n",
        human_bytes(snap.on_disk_size)
    ));
    out.push_str(&format!("entries:            {}\n", stats.entry_count));
    out.push_str(&format!("  with ttl:         {}\n", stats.entries_with_ttl));
    out.push_str(&format!(
        "  without ttl:      {}\n",
        stats.entries_without_ttl
    ));
    out.push_str(&format!(
        "  subscriptions:    {} (under `_sunset-sync/subscribe`)\n",
        stats.subscription_entries
    ));
    out.push_str(&format!(
        "current cursor:     {}\n",
        stats
            .cursor
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into())
    ));

    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(s) = &stats.soonest_expiry {
        out.push_str(&format!(
            "soonest expiry:     in {} ({})\n",
            human_secs_until(s.expires_at, now),
            describe_entry(&s.vk, &s.name),
        ));
    } else {
        out.push_str("soonest expiry:     (no entries with ttl)\n");
    }
    if let Some(l) = &stats.latest_expiry {
        out.push_str(&format!(
            "latest expiry:      in {} ({})\n",
            human_secs_until(l.expires_at, now),
            describe_entry(&l.vk, &l.name),
        ));
    }

    out
}

/// JSON identity. Hex-only field values, no escaping needed. The
/// optional `webtransport_address` field appears only when the relay
/// successfully bound a WT listener; old clients (which don't know
/// about the field) keep working off `address` (the legacy WS URL).
pub fn render_identity(snap: &IdentitySnapshot) -> String {
    let mut out = String::from("{");
    out.push_str(&format!(
        "\"ed25519\":\"{}\",",
        hex::encode(snap.ed25519_public)
    ));
    out.push_str(&format!(
        "\"x25519\":\"{}\",",
        hex::encode(snap.x25519_public)
    ));
    out.push_str(&format!("\"address\":\"{}\"", snap.dial_url));
    if let Some(wt) = &snap.webtransport_address {
        out.push_str(&format!(",\"webtransport_address\":\"{wt}\""));
    }
    out.push_str("}\n");
    out
}

// --- helpers ---

fn peer_short(p: &PeerId) -> String {
    let h = hex::encode(p.0.as_bytes());
    if h.len() <= 16 {
        h
    } else {
        format!("{}…", &h[..16])
    }
}

fn vk_short(vk: &VerifyingKey) -> String {
    let h = hex::encode(vk.as_bytes());
    if h.len() <= 16 {
        h
    } else {
        format!("{}…", &h[..16])
    }
}

fn describe_entry(vk: &VerifyingKey, name: &Bytes) -> String {
    let name_str = match std::str::from_utf8(name) {
        Ok(s) => s.to_string(),
        Err(_) => format!("hex:{}", hex::encode(name)),
    };
    format!("vk={} name={}", vk_short(vk), name_str)
}

fn format_filter(f: &Filter) -> String {
    match f {
        Filter::Specific(vk, name) => {
            format!(
                "Specific(vk={}, name={})",
                vk_short(vk),
                String::from_utf8_lossy(name.as_ref())
            )
        }
        Filter::Keyspace(vk) => format!("Keyspace(vk={})", vk_short(vk)),
        Filter::Namespace(name) => {
            format!("Namespace({})", String::from_utf8_lossy(name.as_ref()))
        }
        Filter::NamePrefix(prefix) => {
            if prefix.is_empty() {
                "All (NamePrefix \"\")".to_string()
            } else {
                format!("NamePrefix({})", String::from_utf8_lossy(prefix.as_ref()))
            }
        }
        Filter::Union(filters) => {
            let parts: Vec<_> = filters.iter().map(format_filter).collect();
            format!("Union[{}]", parts.join(", "))
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

fn human_secs_until(expires_at: u64, now: u64) -> String {
    if expires_at <= now {
        return "expired".to_string();
    }
    human_duration_secs(expires_at - now)
}

fn human_duration_secs(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d{h}h{m}m")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::StoreStats;

    #[test]
    fn identity_json_has_expected_shape() {
        let snap = IdentitySnapshot {
            ed25519_public: [0xab; 32],
            x25519_public: [0xcd; 32],
            dial_url: "ws://relay.example:8443".into(),
            webtransport_address: None,
        };
        let json = render_identity(&snap);
        assert_eq!(
            json,
            "{\"ed25519\":\"abababababababababababababababababababababababababababababababab\",\
             \"x25519\":\"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\",\
             \"address\":\"ws://relay.example:8443\"}\n"
        );
    }

    #[test]
    fn identity_json_includes_webtransport_when_present() {
        let cert_hex = "ee".repeat(32);
        let wt_url = format!("wt://relay.example:8443#cert-sha256={cert_hex}");
        let snap = IdentitySnapshot {
            ed25519_public: [0xab; 32],
            x25519_public: [0xcd; 32],
            dial_url: "ws://relay.example:8443".into(),
            webtransport_address: Some(wt_url.clone()),
        };
        let json = render_identity(&snap);
        assert!(
            json.contains(&format!("\"webtransport_address\":\"{wt_url}\"")),
            "missing wt field in: {json}"
        );
    }

    #[test]
    fn dashboard_renders_minimal_snapshot() {
        let snap = DashboardSnapshot {
            ed25519_public: [0; 32],
            x25519_public: [0; 32],
            listen_addr: "127.0.0.1:8443".parse().unwrap(),
            dial_url: "ws://127.0.0.1:8443".into(),
            configured_peers: vec![],
            connected_peers: vec![],
            subscriptions: vec![],
            data_dir: std::path::PathBuf::from("/tmp/relay"),
            on_disk_size: 0,
            store_stats: StoreStats::default(),
        };
        let html = render_dashboard(&snap);
        assert!(html.starts_with("sunset-relay\n"));
        assert!(html.contains("connected peers:            (none)"));
        assert!(html.contains("subscriptions (advertised by connected peers)"));
    }
}
