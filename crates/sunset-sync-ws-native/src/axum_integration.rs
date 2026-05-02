//! axum 0.7 integration for `sunset-sync-ws-native`.
//!
//! Behind the optional `axum` feature. Provides a WebSocket upgrade
//! handler and the channel-fed `WebSocketRawTransport::serving()` mode
//! (the constructor itself stays in `lib.rs` — see below).
