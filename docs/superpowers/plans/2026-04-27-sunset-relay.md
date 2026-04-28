# sunset-relay (Plan D) — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or superpowers:subagent-driven-development) to execute this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Land Plan D from the web roadmap. Native `sunset-relay` binary that runs `sunset-sync` over `sunset-sync-ws-native` + `sunset-noise`, with persistent storage via `sunset-store-fs`. Acts as a real `sunset-sync` peer (binds an inbound listener AND optionally dials configured federated peers). Multi-relay integration tests prove that messages propagate from a client connected at relay-A to a client connected at relay-B via the relay-to-relay link.

**Spec:** `docs/superpowers/specs/2026-04-27-sunset-relay-design.md`.

**Out of scope (deferred):**

- HTTP admin / status surface
- Allowlists, rate limiting, per-room admission
- TLS termination at the relay (use a fronting proxy)
- Reconnection-with-backoff for federated peer links
- Loop-suppression smarts for federated propagation
- Encrypted identity-at-rest
- Push notifications for offline clients

---

## File structure

```
sunset/
├── Cargo.toml                                  # MODIFY: workspace add sunset-relay member + clap + toml + tracing deps
├── flake.nix                                   # MODIFY: add packages.sunset-relay + packages.sunset-relay-docker
├── crates/
│   └── sunset-relay/                           # NEW
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── error.rs
│           ├── config.rs
│           ├── identity.rs
│           ├── relay.rs
│           └── main.rs
└── crates/sunset-relay/tests/
    └── multi_relay.rs
```

---

## Tasks

### Task 1: Scaffold the `sunset-relay` crate + workspace deps

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `crates/sunset-relay/Cargo.toml`
- Create: `crates/sunset-relay/src/lib.rs`
- Create: `crates/sunset-relay/src/error.rs`
- Create: `crates/sunset-relay/src/main.rs` (placeholder)
- Create: `crates/sunset-relay/src/{config,identity,relay}.rs` (placeholders)

- [ ] **Step 1:** Add to root `Cargo.toml`'s `[workspace.dependencies]` (alphabetical):

  ```toml
  clap = { version = "4", features = ["derive", "env"] }
  rand_core = { workspace = true, features = ["getrandom"] }   # if not already enabled at workspace level
  toml = { version = "0.8", default-features = false, features = ["parse", "display"] }
  tracing = "0.1"
  tracing-subscriber = { version = "0.3", default-features = false, features = ["env-filter", "fmt"] }
  ```

  (`rand_core` may already be in workspace deps without `getrandom`; if so, add the feature only at the relay's crate level — see Step 3.)

  Add `crates/sunset-relay` to `[workspace] members`. Don't add a `sunset-relay` path entry to `[workspace.dependencies]` — no other crate will depend on it as a Rust library.

- [ ] **Step 2:** Create `crates/sunset-relay/Cargo.toml`:

  ```toml
  [package]
  name = "sunset-relay"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  rust-version.workspace = true

  [lints]
  workspace = true

  [[bin]]
  name = "sunset-relay"
  path = "src/main.rs"

  [dependencies]
  bytes.workspace = true
  clap.workspace = true
  rand_core = { workspace = true, features = ["getrandom"] }
  serde = { workspace = true, features = ["derive"] }
  sunset-core.workspace = true
  sunset-noise.workspace = true
  sunset-store.workspace = true
  sunset-store-fs.workspace = true
  sunset-sync.workspace = true
  sunset-sync-ws-native.workspace = true
  thiserror.workspace = true
  tokio = { workspace = true, features = ["macros", "rt", "signal", "fs", "sync", "net"] }
  toml.workspace = true
  tracing.workspace = true
  tracing-subscriber.workspace = true
  zeroize.workspace = true

  [dev-dependencies]
  curve25519-dalek.workspace = true
  hex.workspace = true
  tempfile.workspace = true
  tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time", "signal", "fs", "sync", "net"] }
  ```

- [ ] **Step 3:** Create `crates/sunset-relay/src/error.rs`:

  ```rust
  use thiserror::Error;

  #[derive(Debug, Error)]
  pub enum Error {
      #[error("config: {0}")]
      Config(String),

      #[error("io: {0}")]
      Io(#[from] std::io::Error),

      #[error("toml: {0}")]
      Toml(String),

      #[error("store: {0}")]
      Store(#[from] sunset_store::Error),

      #[error("sync: {0}")]
      Sync(#[from] sunset_sync::Error),

      #[error("noise: {0}")]
      Noise(#[from] sunset_noise::Error),

      #[error("identity: {0}")]
      Identity(String),
  }

  pub type Result<T> = std::result::Result<T, Error>;
  ```

- [ ] **Step 4:** Create `crates/sunset-relay/src/lib.rs`:

  ```rust
  //! Native sunset.chat relay binary + library for in-process testing.
  //!
  //! See `docs/superpowers/specs/2026-04-27-sunset-relay-design.md`.

  pub mod config;
  pub mod error;
  pub mod identity;
  pub mod relay;

  pub use config::Config;
  pub use error::{Error, Result};
  pub use relay::{Relay, RelayHandle};
  ```

- [ ] **Step 5:** Create placeholder modules (each contains `//! Placeholder; populated in a later task of this plan.`):
  - `crates/sunset-relay/src/config.rs`
  - `crates/sunset-relay/src/identity.rs`
  - `crates/sunset-relay/src/relay.rs`

- [ ] **Step 6:** Create `crates/sunset-relay/src/main.rs` (placeholder, populated in Task 5):

  ```rust
  //! sunset-relay binary entrypoint. Populated in Task 5.

  fn main() {
      eprintln!("sunset-relay: placeholder; populated in Task 5");
      std::process::exit(1);
  }
  ```

- [ ] **Step 7:** Verify the crate compiles:
  ```
  nix develop --command cargo build -p sunset-relay
  ```

- [ ] **Step 8:** Commit:
  ```
  git add Cargo.toml crates/sunset-relay/
  git commit -m "Scaffold sunset-relay crate with error type + module skeleton"
  ```

---

### Task 2: Config module

**Files:**
- Modify: `crates/sunset-relay/src/config.rs`

- [ ] **Step 1:** Replace `crates/sunset-relay/src/config.rs` with the full module:

  ```rust
  //! TOML config parsing + defaults.
  //!
  //! See the spec at `docs/superpowers/specs/2026-04-27-sunset-relay-design.md`
  //! § "Configuration".

  use std::net::SocketAddr;
  use std::path::PathBuf;

  use serde::Deserialize;

  use crate::error::{Error, Result};

  /// Fully-resolved relay config (defaults applied; ready to use).
  #[derive(Clone, Debug)]
  pub struct Config {
      pub listen_addr: SocketAddr,
      pub data_dir: PathBuf,
      pub interest_filter: InterestFilter,
      pub identity_secret_path: PathBuf,
      pub peers: Vec<String>,
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum InterestFilter {
      /// Subscribe to everything (NamePrefix("")).
      All,
  }

  /// Raw on-disk shape — every field is optional so partial configs are accepted.
  #[derive(Debug, Default, Deserialize)]
  struct RawConfig {
      listen_addr: Option<String>,
      data_dir: Option<String>,
      interest_filter: Option<String>,
      identity_secret: Option<String>,
      #[serde(default)]
      peers: Vec<String>,
  }

  impl Config {
      /// Resolve from a TOML string (used by both file-loaded and embedded configs).
      pub fn from_toml(text: &str) -> Result<Self> {
          let raw: RawConfig =
              toml::from_str(text).map_err(|e| Error::Toml(e.to_string()))?;
          Self::from_raw(raw)
      }

      /// Resolve when no config file is present: pure defaults.
      pub fn defaults() -> Result<Self> {
          Self::from_raw(RawConfig::default())
      }

      fn from_raw(raw: RawConfig) -> Result<Self> {
          let listen_addr: SocketAddr = raw
              .listen_addr
              .as_deref()
              .unwrap_or("0.0.0.0:8443")
              .parse()
              .map_err(|e| Error::Config(format!("listen_addr parse: {e}")))?;

          let data_dir = PathBuf::from(raw.data_dir.unwrap_or_else(|| "./data".to_owned()));

          let interest_filter = match raw.interest_filter.as_deref().unwrap_or("all") {
              "all" => InterestFilter::All,
              other => return Err(Error::Config(format!(
                  "interest_filter: unknown value `{other}` (only `all` supported in v0)"
              ))),
          };

          let identity_secret_path = match raw.identity_secret.as_deref() {
              None | Some("auto") => data_dir.join("identity.key"),
              Some(path) => PathBuf::from(path),
          };

          Ok(Config {
              listen_addr,
              data_dir,
              interest_filter,
              identity_secret_path,
              peers: raw.peers,
          })
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn defaults_resolve() {
          let c = Config::defaults().unwrap();
          assert_eq!(c.listen_addr.to_string(), "0.0.0.0:8443");
          assert_eq!(c.data_dir, PathBuf::from("./data"));
          assert_eq!(c.interest_filter, InterestFilter::All);
          assert_eq!(c.identity_secret_path, PathBuf::from("./data/identity.key"));
          assert!(c.peers.is_empty());
      }

      #[test]
      fn full_toml_parses() {
          let toml = r#"
              listen_addr = "127.0.0.1:9000"
              data_dir = "/var/lib/sunset-relay"
              interest_filter = "all"
              identity_secret = "/etc/sunset/relay.key"
              peers = ["ws://other:8443#x25519=ab"]
          "#;
          let c = Config::from_toml(toml).unwrap();
          assert_eq!(c.listen_addr.to_string(), "127.0.0.1:9000");
          assert_eq!(c.data_dir, PathBuf::from("/var/lib/sunset-relay"));
          assert_eq!(c.identity_secret_path, PathBuf::from("/etc/sunset/relay.key"));
          assert_eq!(c.peers.len(), 1);
      }

      #[test]
      fn auto_identity_resolves_under_data_dir() {
          let toml = r#"
              listen_addr = "0.0.0.0:8443"
              data_dir = "/tmp/relay"
              identity_secret = "auto"
          "#;
          let c = Config::from_toml(toml).unwrap();
          assert_eq!(c.identity_secret_path, PathBuf::from("/tmp/relay/identity.key"));
      }

      #[test]
      fn rejects_unknown_interest_filter() {
          let toml = r#"
              listen_addr = "0.0.0.0:8443"
              interest_filter = "room/general"
          "#;
          let err = Config::from_toml(toml).unwrap_err();
          assert!(matches!(err, Error::Config(_)));
      }
  }
  ```

- [ ] **Step 2:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-relay
  nix develop --command cargo test -p sunset-relay config::tests
  nix develop --command cargo clippy -p sunset-relay --all-targets -- -D warnings
  ```

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-relay/src/config.rs
  git commit -m "Add Config: TOML parsing + defaults + InterestFilter"
  ```

---

### Task 3: Identity persistence module

**Files:**
- Modify: `crates/sunset-relay/src/identity.rs`

- [ ] **Step 1:** Replace `crates/sunset-relay/src/identity.rs` with:

  ```rust
  //! Load-or-generate the relay's Ed25519 identity, persisted as a 32-byte
  //! secret seed in a file with mode 0600.
  //!
  //! On generation, prints a startup banner with both the Ed25519 and
  //! derived X25519 pubkeys + a copy-pasteable address line.

  use std::path::Path;

  use rand_core::{OsRng, RngCore};
  use zeroize::Zeroizing;

  use sunset_core::Identity;
  use sunset_noise::ed25519_seed_to_x25519_secret;

  use crate::error::{Error, Result};

  /// Load the secret seed from `path`, or generate a fresh one and persist it.
  /// Returns the constructed `Identity`.
  pub async fn load_or_generate(path: &Path) -> Result<Identity> {
      if path.exists() {
          load(path).await
      } else {
          generate_and_persist(path).await
      }
  }

  async fn load(path: &Path) -> Result<Identity> {
      let bytes = tokio::fs::read(path).await?;
      let seed: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
          Error::Identity(format!(
              "expected 32 bytes at {}, got {}",
              path.display(),
              bytes.len(),
          ))
      })?;
      let mode = file_permissions(path).await;
      if let Some(mode) = mode {
          if mode & 0o077 != 0 {
              tracing::warn!(
                  path = %path.display(),
                  mode = format!("{:o}", mode),
                  "identity key file has wider-than-0600 permissions",
              );
          }
      }
      Ok(Identity::from_secret_bytes(&seed))
  }

  async fn generate_and_persist(path: &Path) -> Result<Identity> {
      // Make sure the parent directory exists.
      if let Some(parent) = path.parent() {
          tokio::fs::create_dir_all(parent).await?;
      }

      let mut seed = Zeroizing::new([0u8; 32]);
      OsRng.fill_bytes(&mut *seed);

      // Write to a temp file with mode 0600, then rename atomically.
      let tmp = path.with_extension("key.tmp");
      tokio::fs::write(&tmp, &*seed).await?;
      set_mode_0600(&tmp).await?;
      tokio::fs::rename(&tmp, path).await?;

      Ok(Identity::from_secret_bytes(&seed))
  }

  #[cfg(unix)]
  async fn set_mode_0600(path: &Path) -> Result<()> {
      use std::os::unix::fs::PermissionsExt;
      let mut perms = tokio::fs::metadata(path).await?.permissions();
      perms.set_mode(0o600);
      tokio::fs::set_permissions(path, perms).await?;
      Ok(())
  }

  #[cfg(not(unix))]
  async fn set_mode_0600(_path: &Path) -> Result<()> {
      // Non-unix targets: skip; relay is documented as Linux-first.
      Ok(())
  }

  #[cfg(unix)]
  async fn file_permissions(path: &Path) -> Option<u32> {
      use std::os::unix::fs::PermissionsExt;
      let meta = tokio::fs::metadata(path).await.ok()?;
      Some(meta.permissions().mode())
  }

  #[cfg(not(unix))]
  async fn file_permissions(_path: &Path) -> Option<u32> {
      None
  }

  /// Format the relay's startup address banner (printed by main on startup).
  pub fn format_address(listen_addr: &std::net::SocketAddr, identity: &Identity) -> String {
      let ed_pub = identity.public().as_bytes();
      let x_secret = ed25519_seed_to_x25519_secret(&identity.secret_bytes());
      let x_pub = {
          use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
          let scalar = Scalar::from_bytes_mod_order(*x_secret);
          MontgomeryPoint::mul_base(&scalar).to_bytes()
      };
      format!(
          "sunset-relay starting\n  ed25519: {}\n  x25519:  {}\n  listen:  ws://{}\n  address: ws://{}#x25519={}",
          hex::encode(ed_pub),
          hex::encode(x_pub),
          listen_addr,
          listen_addr,
          hex::encode(x_pub),
      )
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[tokio::test(flavor = "current_thread")]
      async fn load_or_generate_creates_then_loads() {
          let dir = tempfile::tempdir().unwrap();
          let path = dir.path().join("identity.key");
          assert!(!path.exists());

          let id1 = load_or_generate(&path).await.unwrap();
          assert!(path.exists());

          let id2 = load_or_generate(&path).await.unwrap();
          assert_eq!(id1.public(), id2.public());
          assert_eq!(id1.secret_bytes(), id2.secret_bytes());
      }

      #[cfg(unix)]
      #[tokio::test(flavor = "current_thread")]
      async fn generated_file_has_mode_0600() {
          use std::os::unix::fs::PermissionsExt;
          let dir = tempfile::tempdir().unwrap();
          let path = dir.path().join("identity.key");
          let _ = load_or_generate(&path).await.unwrap();
          let meta = tokio::fs::metadata(&path).await.unwrap();
          assert_eq!(meta.permissions().mode() & 0o777, 0o600);
      }

      #[test]
      fn address_format_is_parseable() {
          let id = Identity::from_secret_bytes(&[7u8; 32]);
          let addr = "127.0.0.1:8443".parse().unwrap();
          let s = format_address(&addr, &id);
          assert!(s.contains("ed25519: "));
          assert!(s.contains("x25519:  "));
          assert!(s.contains("address: ws://127.0.0.1:8443#x25519="));
      }
  }
  ```

  Add to `crates/sunset-relay/Cargo.toml`'s `[dev-dependencies]` if missing:
  ```toml
  tempfile.workspace = true
  ```
  (Tempfile is a workspace dep already.)

- [ ] **Step 2:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-relay
  nix develop --command cargo test -p sunset-relay identity::tests
  nix develop --command cargo clippy -p sunset-relay --all-targets -- -D warnings
  ```

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-relay/src/identity.rs crates/sunset-relay/Cargo.toml
  git commit -m "Add identity load-or-generate (mode 0600) + address banner"
  ```

---

### Task 4: `Relay` struct + setup wiring

**Files:**
- Modify: `crates/sunset-relay/src/relay.rs`

- [ ] **Step 1:** Replace `crates/sunset-relay/src/relay.rs` with the relay struct + setup logic. The setup phase: open store, load/generate identity, bind listener, wrap with Noise, build SyncEngine, publish broad subscription, dial federated peers.

  ```rust
  //! Relay: the wired-up store + identity + transport + engine.
  //!
  //! `Relay::new(config)` does all the setup synchronously (in async fn form).
  //! The returned `RelayHandle` exposes the relay's address + a `run` method
  //! that drives the engine until shutdown.

  use std::rc::Rc;
  use std::sync::Arc;
  use std::time::Duration;

  use bytes::Bytes;
  use zeroize::Zeroizing;

  use sunset_core::Identity;
  use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
  use sunset_store::{Filter, VerifyingKey};
  use sunset_store_fs::FsStore;
  use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
  use sunset_sync_ws_native::WebSocketRawTransport;

  use crate::config::{Config, InterestFilter};
  use crate::error::Result;
  use crate::identity;

  type Engine = SyncEngine<FsStore, NoiseTransport<WebSocketRawTransport>>;

  pub struct Relay { /* sealed; see RelayHandle */ }

  pub struct RelayHandle {
      pub local_address: String,
      pub ed25519_public: [u8; 32],
      pub x25519_public: [u8; 32],

      engine: Rc<Engine>,
      peers: Vec<String>,
      subscription_filter: Filter,
  }

  /// Adapter so sunset-core's `Identity` can be used as a `NoiseIdentity`.
  /// (Duplicated from Plan C's integration test — moves to sunset-core in a
  /// follow-up.)
  struct IdentityNoiseAdapter(Identity);

  impl NoiseIdentity for IdentityNoiseAdapter {
      fn ed25519_public(&self) -> [u8; 32] {
          self.0.public().as_bytes()
      }
      fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
          Zeroizing::new(self.0.secret_bytes())
      }
  }

  impl Relay {
      /// Open store, load identity, bind listener, build engine. Returns a
      /// handle ready for `run()`.
      pub async fn new(config: Config) -> Result<RelayHandle> {
          // 1. Identity (load-or-generate; persists to disk on first start).
          tokio::fs::create_dir_all(&config.data_dir).await?;
          let identity = identity::load_or_generate(&config.identity_secret_path).await?;
          let banner = identity::format_address(&config.listen_addr, &identity);
          tracing::info!("\n{}", banner);
          println!("{}", banner);

          let ed25519_public = identity.public().as_bytes();
          let x25519_public = {
              let s = ed25519_seed_to_x25519_secret(&identity.secret_bytes());
              use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
              let scalar = Scalar::from_bytes_mod_order(*s);
              MontgomeryPoint::mul_base(&scalar).to_bytes()
          };

          // 2. Store (FsStore with Ed25519Verifier).
          let store_root = config.data_dir.join("store");
          tokio::fs::create_dir_all(&store_root).await?;
          let store = Arc::new(
              FsStore::open(&store_root, Arc::new(sunset_core::Ed25519Verifier))
                  .await?,
          );

          // 3. Listener + Noise wrapper.
          let raw = WebSocketRawTransport::listening_on(config.listen_addr).await?;
          let bound = raw
              .local_addr()
              .unwrap_or(config.listen_addr);
          let local_address = format!("ws://{}#x25519={}", bound, hex::encode(x25519_public));
          let noise = NoiseTransport::new(raw, Arc::new(IdentityNoiseAdapter(identity.clone())));

          // 4. SyncEngine.
          let local_peer = PeerId(VerifyingKey::new(Bytes::copy_from_slice(&ed25519_public)));
          let signer: Arc<dyn Signer> = Arc::new(identity.clone());
          let engine = Rc::new(SyncEngine::new(
              store,
              noise,
              SyncConfig::default(),
              local_peer,
              signer,
          ));

          // 5. Subscription filter.
          let subscription_filter = match config.interest_filter {
              InterestFilter::All => Filter::NamePrefix(Bytes::new()),
          };

          Ok(RelayHandle {
              local_address,
              ed25519_public,
              x25519_public,
              engine,
              peers: config.peers,
              subscription_filter,
          })
      }
  }

  impl RelayHandle {
      pub fn dial_address(&self) -> String {
          self.local_address.clone()
      }

      /// Drive the engine, dial federated peers, then run until shutdown.
      ///
      /// In the binary path, shutdown is triggered by SIGTERM/SIGINT; in
      /// integration-test paths, the caller drops the future / aborts the
      /// task.
      pub async fn run(self) -> Result<()> {
          // Spawn the engine event loop.
          let engine_clone = self.engine.clone();
          let engine_task = tokio::task::spawn_local(async move {
              engine_clone.run().await
          });

          // Publish our broad subscription so peers know what we'll accept.
          self.engine
              .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
              .await?;
          tracing::info!("published broad subscription");

          // Dial each configured federated peer.
          for peer_url in &self.peers {
              let addr = PeerAddr::new(Bytes::from(peer_url.clone()));
              match self.engine.add_peer(addr).await {
                  Ok(()) => tracing::info!(peer = %peer_url, "federated peer dialed"),
                  Err(e) => tracing::warn!(peer = %peer_url, error = %e, "federated peer dial failed (continuing)"),
              }
          }

          // Wait for shutdown.
          #[cfg(unix)]
          {
              let mut sigterm = tokio::signal::unix::signal(
                  tokio::signal::unix::SignalKind::terminate(),
              )?;
              tokio::select! {
                  _ = tokio::signal::ctrl_c() => {
                      tracing::info!("received SIGINT, shutting down");
                  }
                  _ = sigterm.recv() => {
                      tracing::info!("received SIGTERM, shutting down");
                  }
              }
          }
          #[cfg(not(unix))]
          {
              tokio::signal::ctrl_c().await?;
              tracing::info!("received Ctrl+C, shutting down");
          }

          engine_task.abort();
          Ok(())
      }

      /// For tests: directly drive the engine in the background without
      /// waiting for OS signals. Returns the engine handle so the caller
      /// can interact with it via `engine_handle.engine()`.
      ///
      /// The caller is responsible for keeping the returned handle alive for
      /// the duration of the test, and aborting it during teardown.
      #[cfg(any(test, feature = "test-helpers"))]
      pub async fn run_for_test(&self) -> Result<tokio::task::JoinHandle<sunset_sync::Result<()>>> {
          let engine_clone = self.engine.clone();
          let engine_task = tokio::task::spawn_local(async move {
              engine_clone.run().await
          });

          self.engine
              .publish_subscription(self.subscription_filter.clone(), Duration::from_secs(3600))
              .await?;

          for peer_url in &self.peers {
              let addr = PeerAddr::new(Bytes::from(peer_url.clone()));
              if let Err(e) = self.engine.add_peer(addr).await {
                  tracing::warn!(peer = %peer_url, error = %e, "federated peer dial failed (test)");
              }
          }

          Ok(engine_task)
      }

      /// For tests: access the underlying engine.
      #[cfg(any(test, feature = "test-helpers"))]
      pub fn engine(&self) -> &Rc<Engine> {
          &self.engine
      }
  }
  ```

  **Note on `run_for_test`**: integration tests need to keep the engine running while they observe behavior, without being blocked on `ctrl_c()`. The `run_for_test` method does the same setup as `run()` but returns the engine task handle so the test can abort it during teardown.

- [ ] **Step 2:** Add `[features]` to `Cargo.toml`:
  ```toml
  [features]
  test-helpers = []
  ```

- [ ] **Step 3:** Verify:
  ```
  nix develop --command cargo fmt -p sunset-relay
  nix develop --command cargo build -p sunset-relay
  nix develop --command cargo clippy -p sunset-relay --all-targets -- -D warnings
  ```

- [ ] **Step 4:** Commit:
  ```
  git add crates/sunset-relay/src/relay.rs crates/sunset-relay/Cargo.toml
  git commit -m "Add Relay::new + RelayHandle::run with federated peer dialing"
  ```

---

### Task 5: CLI binary entrypoint

**Files:**
- Modify: `crates/sunset-relay/src/main.rs`

- [ ] **Step 1:** Replace `crates/sunset-relay/src/main.rs` with:

  ```rust
  //! sunset-relay binary entrypoint.

  use std::path::PathBuf;

  use clap::Parser;
  use tracing_subscriber::EnvFilter;

  use sunset_relay::{Config, Relay, Result};

  #[derive(Parser, Debug)]
  #[command(version, about = "sunset.chat relay")]
  struct Cli {
      /// Path to the TOML config file. If omitted, runs with defaults
      /// (listen 0.0.0.0:8443, data ./data, no federated peers).
      #[arg(long, value_name = "PATH")]
      config: Option<PathBuf>,
  }

  fn main() -> Result<()> {
      tracing_subscriber::fmt()
          .with_env_filter(
              EnvFilter::try_from_default_env()
                  .unwrap_or_else(|_| EnvFilter::new("sunset_relay=info,sunset_sync=warn")),
          )
          .init();

      let cli = Cli::parse();

      let rt = tokio::runtime::Builder::new_current_thread()
          .enable_all()
          .build()?;

      rt.block_on(async {
          let local = tokio::task::LocalSet::new();
          local
              .run_until(async {
                  let config = match cli.config {
                      Some(path) => {
                          let text = std::fs::read_to_string(&path).map_err(|e| {
                              sunset_relay::Error::Config(format!(
                                  "read {}: {e}",
                                  path.display(),
                              ))
                          })?;
                          Config::from_toml(&text)?
                      }
                      None => Config::defaults()?,
                  };
                  let handle = Relay::new(config).await?;
                  handle.run().await
              })
              .await
      })
  }
  ```

- [ ] **Step 2:** Verify the binary builds + runs (with timeout — it'll bind a port):
  ```
  nix develop --command cargo build -p sunset-relay --bin sunset-relay
  ```

  Don't actually `cargo run` the binary in CI — it would bind 0.0.0.0:8443 and block.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-relay/src/main.rs
  git commit -m "Add sunset-relay binary entrypoint with clap CLI + tracing"
  ```

---

### Task 6: Multi-relay propagation integration test

**Files:**
- Create: `crates/sunset-relay/tests/multi_relay.rs`

- [ ] **Step 1:** Write the headline test. The structure:
  - Start relay A on a random port.
  - Start relay B on a random port, with `peers = [<relay A's address>]` so B dials A on startup.
  - Wait for the federation handshake to complete.
  - Spin up alice (sunset-core Identity) connecting to relay A's address.
  - Spin up bob connecting to relay B's address.
  - Bob subscribes to "messages in room X".
  - Alice composes a message + inserts to her local store.
  - Wait for the message to propagate: alice → relay A → relay B → bob.
  - Bob decodes, asserts author + body.

  ```rust
  //! Multi-relay integration tests.

  use std::rc::Rc;
  use std::sync::Arc;
  use std::time::Duration;

  use bytes::Bytes;
  use rand_core::OsRng;
  use zeroize::Zeroizing;

  use sunset_core::crypto::constants::test_fast_params;
  use sunset_core::{
      ComposedMessage, Ed25519Verifier, Identity, Room, compose_message, decode_message,
      room_messages_filter,
  };
  use sunset_noise::{NoiseIdentity, NoiseTransport, ed25519_seed_to_x25519_secret};
  use sunset_relay::{Config, Relay};
  use sunset_store::{ContentBlock, Hash, Store as _};
  use sunset_store_memory::MemoryStore;
  use sunset_sync::{PeerAddr, PeerId, Signer, SyncConfig, SyncEngine};
  use sunset_sync_ws_native::WebSocketRawTransport;

  // -- helpers --

  struct IdentityAdapter(Identity);

  impl NoiseIdentity for IdentityAdapter {
      fn ed25519_public(&self) -> [u8; 32] { self.0.public().as_bytes() }
      fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
          Zeroizing::new(self.0.secret_bytes())
      }
  }

  fn ed25519_to_x25519_pub(secret_seed: &[u8; 32]) -> [u8; 32] {
      let s = ed25519_seed_to_x25519_secret(secret_seed);
      use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
      let scalar = Scalar::from_bytes_mod_order(*s);
      MontgomeryPoint::mul_base(&scalar).to_bytes()
  }

  /// Spin up a client SyncEngine that dials a relay address.
  async fn make_client(
      identity: Identity,
      relay_addr: &str,
  ) -> (Arc<MemoryStore>, Rc<SyncEngine<MemoryStore, NoiseTransport<WebSocketRawTransport>>>) {
      let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
      let raw = WebSocketRawTransport::dial_only();
      let noise = NoiseTransport::new(raw, Arc::new(IdentityAdapter(identity.clone())));
      let local_peer = PeerId(identity.store_verifying_key());
      let signer: Arc<dyn Signer> = Arc::new(identity.clone());
      let engine = Rc::new(SyncEngine::new(
          store.clone(),
          noise,
          SyncConfig::default(),
          local_peer,
          signer,
      ));
      let engine_clone = engine.clone();
      tokio::task::spawn_local(async move { engine_clone.run().await });

      // Dial the relay.
      let addr = PeerAddr::new(Bytes::from(relay_addr.to_owned()));
      engine.add_peer(addr).await.expect("client dial relay");

      (store, engine)
  }

  async fn wait_for<F, Fut>(deadline: Duration, interval: Duration, mut condition: F) -> bool
  where
      F: FnMut() -> Fut,
      Fut: std::future::Future<Output = bool>,
  {
      let start = tokio::time::Instant::now();
      while start.elapsed() < deadline {
          if condition().await {
              return true;
          }
          tokio::time::sleep(interval).await;
      }
      false
  }

  fn relay_config(data_dir: std::path::PathBuf, listen_addr: &str, peers: Vec<String>) -> Config {
      let toml = format!(
          r#"
          listen_addr = "{}"
          data_dir = "{}"
          interest_filter = "all"
          identity_secret = "auto"
          peers = [{}]
          "#,
          listen_addr,
          data_dir.display(),
          peers.iter().map(|p| format!("\"{}\"", p)).collect::<Vec<_>>().join(", "),
      );
      Config::from_toml(&toml).unwrap()
  }

  // -- Test 1: two-relay propagation --

  #[tokio::test(flavor = "current_thread")]
  async fn alice_to_bob_via_two_relays() {
      let local = tokio::task::LocalSet::new();
      local.run_until(async {
          let dir_a = tempfile::tempdir().unwrap();
          let dir_b = tempfile::tempdir().unwrap();

          // Relay A: listen, no federated peers yet (we'll learn its address first).
          let config_a = relay_config(
              dir_a.path().to_owned(),
              "127.0.0.1:0",
              vec![],
          );
          let relay_a = Relay::new(config_a).await.expect("relay A new");
          let relay_a_addr = relay_a.dial_address();
          let _engine_a_task = relay_a.run_for_test().await.expect("relay A run");

          // Relay B: listen, with relay A as federated peer.
          let config_b = relay_config(
              dir_b.path().to_owned(),
              "127.0.0.1:0",
              vec![relay_a_addr.clone()],
          );
          let relay_b = Relay::new(config_b).await.expect("relay B new");
          let relay_b_addr = relay_b.dial_address();
          let _engine_b_task = relay_b.run_for_test().await.expect("relay B run");

          // Brief settle for federation handshake.
          tokio::time::sleep(Duration::from_millis(200)).await;

          // Clients.
          let alice = Identity::generate(&mut OsRng);
          let bob = Identity::generate(&mut OsRng);
          let alice_room = Room::open_with_params("plan-d-test", &test_fast_params()).unwrap();
          let bob_room = Room::open_with_params("plan-d-test", &test_fast_params()).unwrap();

          let (alice_store, _alice_engine) = make_client(alice.clone(), &relay_a_addr).await;
          let (bob_store, bob_engine) = make_client(bob.clone(), &relay_b_addr).await;

          // Bob declares interest.
          bob_engine
              .publish_subscription(room_messages_filter(&bob_room), Duration::from_secs(60))
              .await.unwrap();

          // Alice composes + inserts.
          let body = "hello bob across two relays";
          let sent_at = 1_700_000_000_000u64;
          let ComposedMessage { entry, block } =
              compose_message(&alice, &alice_room, 0, sent_at, body, &mut OsRng).unwrap();
          let expected_hash: Hash = block.hash();
          alice_store.insert(entry.clone(), Some(block.clone())).await
              .expect("alice's local store accepts her entry");

          // Wait for entry + block to land at bob's store.
          let bob_has_entry = wait_for(
              Duration::from_secs(10),
              Duration::from_millis(50),
              || async {
                  bob_store
                      .get_entry(&alice.store_verifying_key(), &entry.name)
                      .await.unwrap().is_some()
              },
          ).await;
          assert!(bob_has_entry, "bob did not receive alice's entry via two relays");

          let bob_has_block = wait_for(
              Duration::from_secs(10),
              Duration::from_millis(50),
              || async {
                  bob_store.get_content(&expected_hash).await.unwrap().is_some()
              },
          ).await;
          assert!(bob_has_block, "bob did not receive alice's content block via two relays");

          // Decode + assert.
          let bob_entry = bob_store.get_entry(&alice.store_verifying_key(), &entry.name)
              .await.unwrap().unwrap();
          let bob_block: ContentBlock = bob_store.get_content(&expected_hash)
              .await.unwrap().unwrap();
          let decoded = decode_message(&bob_room, &bob_entry, &bob_block).unwrap();
          assert_eq!(decoded.author_key, alice.public());
          assert_eq!(decoded.body, body);
      }).await;
  }
  ```

- [ ] **Step 2:** Run the test:
  ```
  nix develop --command cargo test -p sunset-relay --test multi_relay alice_to_bob_via_two_relays --features test-helpers -- --nocapture
  ```

  Expect 1 passed. If timeouts: the federation handshake may take longer than 200ms in CI; bump the settle sleep to 500ms.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-relay/tests/multi_relay.rs
  git commit -m "Add multi-relay propagation integration test (alice→A→B→bob)"
  ```

---

### Task 7: Failover / redundancy integration test

**Files:**
- Modify: `crates/sunset-relay/tests/multi_relay.rs`

- [ ] **Step 1:** Append a second test to `crates/sunset-relay/tests/multi_relay.rs`:

  ```rust
  // -- Test 2: failover when one relay dies --

  #[tokio::test(flavor = "current_thread")]
  async fn failover_when_relay_a_dies() {
      let local = tokio::task::LocalSet::new();
      local.run_until(async {
          let dir_a = tempfile::tempdir().unwrap();
          let dir_b = tempfile::tempdir().unwrap();

          // Relay A.
          let config_a = relay_config(dir_a.path().to_owned(), "127.0.0.1:0", vec![]);
          let relay_a = Relay::new(config_a).await.expect("relay A new");
          let relay_a_addr = relay_a.dial_address();
          let engine_a_task = relay_a.run_for_test().await.expect("relay A run");

          // Relay B (federated to A).
          let config_b = relay_config(
              dir_b.path().to_owned(),
              "127.0.0.1:0",
              vec![relay_a_addr.clone()],
          );
          let relay_b = Relay::new(config_b).await.expect("relay B new");
          let relay_b_addr = relay_b.dial_address();
          let _engine_b_task = relay_b.run_for_test().await.expect("relay B run");

          tokio::time::sleep(Duration::from_millis(200)).await;

          // Alice connects to BOTH; bob connects to BOTH.
          let alice = Identity::generate(&mut OsRng);
          let bob = Identity::generate(&mut OsRng);
          let alice_room = Room::open_with_params("plan-d-failover", &test_fast_params()).unwrap();
          let bob_room = Room::open_with_params("plan-d-failover", &test_fast_params()).unwrap();

          let (alice_store, alice_engine) = make_client(alice.clone(), &relay_a_addr).await;
          alice_engine
              .add_peer(PeerAddr::new(Bytes::from(relay_b_addr.clone())))
              .await
              .expect("alice dial relay B");

          let (bob_store, bob_engine) = make_client(bob.clone(), &relay_a_addr).await;
          bob_engine
              .add_peer(PeerAddr::new(Bytes::from(relay_b_addr.clone())))
              .await
              .expect("bob dial relay B");

          bob_engine
              .publish_subscription(room_messages_filter(&bob_room), Duration::from_secs(60))
              .await.unwrap();

          // Compose msg-1; expect it to arrive normally.
          let ComposedMessage { entry: e1, block: b1 } =
              compose_message(&alice, &alice_room, 0, 1, "msg-1 (both relays alive)", &mut OsRng).unwrap();
          alice_store.insert(e1.clone(), Some(b1.clone())).await.unwrap();

          let bob_has_msg1 = wait_for(
              Duration::from_secs(10),
              Duration::from_millis(50),
              || async {
                  bob_store
                      .get_entry(&alice.store_verifying_key(), &e1.name)
                      .await.unwrap().is_some()
              },
          ).await;
          assert!(bob_has_msg1, "bob did not receive msg-1");

          // Kill relay A.
          engine_a_task.abort();
          tokio::time::sleep(Duration::from_millis(200)).await;

          // Compose msg-2; expect it to still arrive via relay B.
          let ComposedMessage { entry: e2, block: b2 } =
              compose_message(&alice, &alice_room, 0, 2, "msg-2 (after relay A killed)", &mut OsRng).unwrap();
          alice_store.insert(e2.clone(), Some(b2.clone())).await.unwrap();

          let bob_has_msg2 = wait_for(
              Duration::from_secs(15),
              Duration::from_millis(50),
              || async {
                  bob_store
                      .get_entry(&alice.store_verifying_key(), &e2.name)
                      .await.unwrap().is_some()
              },
          ).await;
          assert!(bob_has_msg2, "bob did not receive msg-2 after relay A died");
      }).await;
  }
  ```

- [ ] **Step 2:** Run both tests:
  ```
  nix develop --command cargo test -p sunset-relay --test multi_relay --features test-helpers -- --nocapture
  ```

  Expect 2 passed. If failover times out: ensure alice's client is configured to reach relay B (the second `add_peer` call); ensure the kill happens AFTER msg-1 confirms.

- [ ] **Step 3:** Commit:
  ```
  git add crates/sunset-relay/tests/multi_relay.rs
  git commit -m "Add failover integration test (kill relay A; bob still gets msg via B)"
  ```

---

### Task 8: Nix derivation for `sunset-relay` binary + Docker image

**Files:**
- Modify: `flake.nix`

- [ ] **Step 1:** Read `flake.nix` to find the `packages` set (Plan A added `sunset-core-wasm` here). Add two new entries: `sunset-relay` (the native binary) and `sunset-relay-docker` (the OCI image).

  Add inside the `packages = { ... }` literal (alongside the existing `sunset-core-wasm`):

  ```nix
  sunset-relay = pkgs.rustPlatform.buildRustPackage {
    pname = "sunset-relay";
    version = "0.1.0";
    src = ./.;
    cargoLock.lockFile = ./Cargo.lock;
    cargoBuildFlags = [ "-p" "sunset-relay" "--bin" "sunset-relay" ];
    doCheck = false;
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = [ pkgs.openssl ];   # used by tokio-tungstenite for wss
    cargo = rustToolchain;
    rustc = rustToolchain;
  };

  sunset-relay-docker = pkgs.dockerTools.buildLayeredImage {
    name = "sunset-relay";
    tag = "latest";
    contents = [ self.packages.${system}.sunset-relay pkgs.cacert ];
    config = {
      Entrypoint = [ "/bin/sunset-relay" ];
      Cmd = [ "--config" "/etc/sunset-relay.toml" ];
      ExposedPorts."8443/tcp" = {};
      Env = [ "RUST_LOG=sunset_relay=info" ];
      Volumes."/var/lib/sunset-relay" = {};
    };
  };
  ```

  **Note on `self.packages.${system}.sunset-relay`**: this references the binary derivation by name within the same flake. If your flake's `outputs` function uses a different binding pattern, adjust accordingly (e.g., `packages.sunset-relay` if defined in a `let` higher up).

  **Note on openssl/pkg-config**: `tokio-tungstenite` may pull in `native-tls` for wss support. If the build fails complaining about openssl, the alternatives are: (a) keep `nativeBuildInputs = [ pkgs.pkg-config ]` and `buildInputs = [ pkgs.openssl ]` as shown; (b) switch tokio-tungstenite features to use `rustls-tls-native-roots` instead. Try (a) first since it's already wired.

- [ ] **Step 2:** Verify the binary builds via Nix:
  ```
  nix build .#sunset-relay --no-link --print-out-paths
  ls "$(nix path-info .#sunset-relay 2>/dev/null)"/bin
  ```
  Expect a `bin/sunset-relay` executable. Run `--help`:
  ```
  "$(nix path-info .#sunset-relay)"/bin/sunset-relay --help
  ```
  Expect clap-formatted help output.

- [ ] **Step 3:** Verify the Docker image builds:
  ```
  nix build .#sunset-relay-docker --no-link --print-out-paths
  ```
  Expect a path to a `.tar.gz`. Confirm with `file`:
  ```
  file "$(nix path-info .#sunset-relay-docker 2>/dev/null)"
  ```
  Should report a gzipped tar archive.

  Optional manual check (skip in CI):
  ```
  docker load < "$(nix path-info .#sunset-relay-docker)"
  docker run --rm -p 9443:8443 sunset-relay:latest --help   # (won't actually start the relay since --help short-circuits)
  ```

- [ ] **Step 4:** Commit:
  ```
  git add flake.nix flake.lock
  git commit -m "Add packages.sunset-relay binary + sunset-relay-docker layered image"
  ```

  (Include `flake.lock` only if it changed.)

---

### Task 9: Final pass — fmt, clippy, full test, nix builds

- [ ] **Step 1:** Workspace-wide checks:
  ```
  nix develop --command cargo fmt --all --check
  nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
  nix develop --command cargo test --workspace --all-features
  ```
  All clean / green.

- [ ] **Step 2:** All Nix package builds still succeed:
  ```
  nix build .#sunset-core-wasm --no-link
  nix build .#sunset-relay --no-link
  nix build .#sunset-relay-docker --no-link
  ```

- [ ] **Step 3:** WASM compatibility — sunset-noise + sunset-core still build for browser:
  ```
  nix develop --command cargo build -p sunset-noise --target wasm32-unknown-unknown --lib
  nix develop --command cargo build -p sunset-core --target wasm32-unknown-unknown --lib
  ```

- [ ] **Step 4:** If any cleanup commits were needed:
  ```
  git add -u
  git commit -m "Final fmt + clippy pass"
  ```

---

## Verification (end-state acceptance)

After all 9 tasks land:

- `cargo test --workspace --all-features` — green, including `crates/sunset-relay/tests/multi_relay.rs`'s two integration tests.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` — clean.
- `cargo fmt --all --check` — clean.
- `nix build .#sunset-relay` — produces `bin/sunset-relay`.
- `nix build .#sunset-relay-docker` — produces an OCI tarball loadable by `docker load`.
- `nix build .#sunset-core-wasm` — Plan A artifact still builds.
- The two integration tests exercise: real WebSocket + Noise tunnels, sunset-store-fs persistence, multi-process Relay::new, federated peer dial, Ed25519Verifier end-to-end, sunset-core encrypted+signed message decode, and graceful failover when one relay dies.
- `git log --oneline master..HEAD` — roughly 9 task-by-task commits.

---

## What this unlocks

After Plan D:

- **Plan E.transport — browser WebSocket RawTransport.** Implements `RawTransport` over `web-sys::WebSocket` (sunset-noise already wasm-compatible from Plan C). Browser-side companion to `sunset-sync-ws-native`.
- **Plan E — Gleam UI wires to WASM bridge + browser sync engine.** With Plans A + D + E.transport in place, the Gleam app can: (a) generate identity / open room via Plan A's bridge; (b) connect to a deployed relay via Plan E.transport; (c) compose / decode messages. Result: two browsers chatting across a deployed relay.
- **Plan WebRTC + Plan WebTransport** (later) — additional `RawTransport` impls; `NoiseTransport` decorator works unchanged.
- **Plan PQC** — unified hybrid post-quantum subsystem covering Noise + Plan 7 key bundles + Plan 9 signatures. Wire-format-bumping plan; needs all v0 layers stable first (where we'll be).
