//! Plaintext HTTP status page for the relay.
//!
//! Rendered at `GET /dashboard` on the relay's single WS port. The accept
//! loop and routing live in `router.rs`; this module owns only the render
//! logic and the per-connection response writer.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use sunset_noise::NoiseTransport;
use sunset_store::{Filter, Store};
use sunset_store_fs::FsStore;
use sunset_sync::{PeerId, SyncEngine};
use sunset_sync_ws_native::WebSocketRawTransport;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Result;

type Engine = SyncEngine<FsStore, NoiseTransport<WebSocketRawTransport>>;

/// Read-only handles + identity values needed to render the page.
pub(crate) struct StatusContext {
    pub engine: Rc<Engine>,
    pub store: Arc<FsStore>,
    pub data_dir: PathBuf,
    pub dial_url: String,
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    pub configured_peers: Vec<String>,
    pub listen_addr: SocketAddr,
}

/// Render the dashboard for a TcpStream that the router has already
/// classified as a `GET /dashboard` request. Reads (and discards) the
/// rest of the request, then writes the rendered body.
pub(crate) async fn serve_dashboard(mut tcp: TcpStream, ctx: Rc<StatusContext>) -> Result<()> {
    drain_request(&mut tcp).await;
    let body = render(ctx).await;
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    tcp.write_all(head.as_bytes()).await.ok();
    tcp.write_all(body.as_bytes()).await.ok();
    tcp.shutdown().await.ok();
    Ok(())
}

/// Render the JSON identity descriptor for `GET /` (without a WS
/// upgrade). The same URL serves the WebSocket endpoint when an
/// `Upgrade: websocket` header is present — the router decides which
/// way to go.
pub(crate) async fn serve_identity(mut tcp: TcpStream, ctx: Rc<StatusContext>) -> Result<()> {
    drain_request(&mut tcp).await;
    let body = identity_json(&ctx.ed25519_public, &ctx.x25519_public, &ctx.dial_url);
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    tcp.write_all(head.as_bytes()).await.ok();
    tcp.write_all(body.as_bytes()).await.ok();
    tcp.shutdown().await.ok();
    Ok(())
}

fn identity_json(ed25519: &[u8; 32], x25519: &[u8; 32], dial_url: &str) -> String {
    // Hex-only field values, so plain string interpolation is safe —
    // no characters that need JSON escaping. Keeping the writer
    // hand-rolled avoids pulling serde_json in for three fields. The
    // dial_url is a relay-controlled string we already trust to be
    // ASCII (`ws://host:port` shape from config + listen addr).
    format!(
        "{{\"ed25519\":\"{}\",\"x25519\":\"{}\",\"address\":\"{}\"}}\n",
        hex::encode(ed25519),
        hex::encode(x25519),
        dial_url,
    )
}

/// Read whatever the client sent until we see `\r\n\r\n` or 8 KiB cap or 2s.
async fn drain_request(tcp: &mut TcpStream) {
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if total >= buf.len() {
                break;
            }
            match tcp.read(&mut buf[total..]).await {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
    .await;
}

async fn render(ctx: Rc<StatusContext>) -> String {
    let mut out = String::new();
    out.push_str("sunset-relay\n");
    out.push_str("============\n\n");

    out.push_str("identity\n--------\n");
    out.push_str(&format!("ed25519:  {}\n", hex::encode(ctx.ed25519_public)));
    out.push_str(&format!("x25519:   {}\n", hex::encode(ctx.x25519_public)));
    out.push_str(&format!("listen:   ws://{}\n", ctx.listen_addr));
    out.push_str(&format!("dial:     {}\n\n", ctx.dial_url));

    out.push_str("peers\n-----\n");
    if ctx.configured_peers.is_empty() {
        out.push_str("configured federated peers: (none)\n");
    } else {
        out.push_str(&format!(
            "configured federated peers ({}):\n",
            ctx.configured_peers.len()
        ));
        for p in &ctx.configured_peers {
            out.push_str(&format!("  - {}\n", p));
        }
    }
    let connected = ctx.engine.connected_peers().await;
    if connected.is_empty() {
        out.push_str("connected peers:            (none)\n");
    } else {
        out.push_str(&format!("connected peers ({}):\n", connected.len()));
        for p in &connected {
            out.push_str(&format!("  - ed25519:{}\n", peer_short(p)));
        }
    }
    out.push('\n');

    out.push_str("subscriptions (advertised by connected peers)\n");
    out.push_str("---------------------------------------------\n");
    let subs = ctx.engine.subscriptions_snapshot().await;
    if subs.is_empty() {
        out.push_str("(none)\n\n");
    } else {
        for (peer, filter) in &subs {
            out.push_str(&format!(
                "  ed25519:{} -> {}\n",
                peer_short(peer),
                format_filter(filter)
            ));
        }
        out.push('\n');
    }

    out.push_str("store\n-----\n");
    let stats = collect_store_stats(&*ctx.store).await;
    let on_disk = dir_size(&ctx.data_dir).unwrap_or(0);
    out.push_str(&format!("data dir:           {}\n", ctx.data_dir.display()));
    out.push_str(&format!("on-disk size:       {}\n", human_bytes(on_disk)));
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
    if let Some(s) = stats.soonest_expiry {
        out.push_str(&format!(
            "soonest expiry:     in {} ({})\n",
            human_secs_until(s.expires_at, now),
            describe_entry(&s.vk, &s.name),
        ));
    } else {
        out.push_str("soonest expiry:     (no entries with ttl)\n");
    }
    if let Some(l) = stats.latest_expiry {
        out.push_str(&format!(
            "latest expiry:      in {} ({})\n",
            human_secs_until(l.expires_at, now),
            describe_entry(&l.vk, &l.name),
        ));
    }

    out
}

// --- store stats ---

#[derive(Default)]
struct StoreStats {
    entry_count: u64,
    entries_with_ttl: u64,
    entries_without_ttl: u64,
    subscription_entries: u64,
    soonest_expiry: Option<EntryTtl>,
    latest_expiry: Option<EntryTtl>,
    cursor: Option<u64>,
}

struct EntryTtl {
    expires_at: u64,
    vk: sunset_store::VerifyingKey,
    name: Bytes,
}

async fn collect_store_stats<S: Store>(store: &S) -> StoreStats {
    let mut stats = StoreStats::default();
    if let Ok(c) = store.current_cursor().await {
        stats.cursor = Some(c.0);
    }
    let mut iter = match store.iter(Filter::NamePrefix(Bytes::new())).await {
        Ok(s) => s,
        Err(_) => return stats,
    };
    while let Some(item) = iter.next().await {
        let entry = match item {
            Ok(e) => e,
            Err(_) => continue,
        };
        stats.entry_count += 1;
        if entry.name.as_ref() == sunset_sync::reserved::SUBSCRIBE_NAME {
            stats.subscription_entries += 1;
        }
        match entry.expires_at {
            None => stats.entries_without_ttl += 1,
            Some(t) => {
                stats.entries_with_ttl += 1;
                let candidate = EntryTtl {
                    expires_at: t,
                    vk: entry.verifying_key.clone(),
                    name: entry.name.clone(),
                };
                if stats
                    .soonest_expiry
                    .as_ref()
                    .is_none_or(|s| t < s.expires_at)
                {
                    stats.soonest_expiry = Some(EntryTtl {
                        expires_at: candidate.expires_at,
                        vk: candidate.vk.clone(),
                        name: candidate.name.clone(),
                    });
                }
                if stats
                    .latest_expiry
                    .as_ref()
                    .is_none_or(|s| t > s.expires_at)
                {
                    stats.latest_expiry = Some(candidate);
                }
            }
        }
    }
    stats
}

// --- formatting helpers ---

fn peer_short(p: &PeerId) -> String {
    let h = hex::encode(p.0.as_bytes());
    if h.len() <= 16 {
        h
    } else {
        format!("{}…", &h[..16])
    }
}

fn vk_short(vk: &sunset_store::VerifyingKey) -> String {
    let h = hex::encode(vk.as_bytes());
    if h.len() <= 16 {
        h
    } else {
        format!("{}…", &h[..16])
    }
}

fn describe_entry(vk: &sunset_store::VerifyingKey, name: &Bytes) -> String {
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

fn dir_size(root: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let rd = match std::fs::read_dir(&p) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_json_has_expected_shape() {
        let ed = [0xab; 32];
        let xx = [0xcd; 32];
        let json = identity_json(&ed, &xx, "ws://relay.example:8443");
        assert_eq!(
            json,
            "{\"ed25519\":\"abababababababababababababababababababababababababababababababab\",\
             \"x25519\":\"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\",\
             \"address\":\"ws://relay.example:8443\"}\n"
        );
    }
}
