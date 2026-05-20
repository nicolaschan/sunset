//! Native-target fallback macro for browser-only `RawTransport` crates.
//!
//! Each `sunset-sync-{ws,webtransport,webrtc}-browser` crate ships a wasm
//! implementation plus a non-wasm stub so the workspace builds on native
//! hosts that lack wasm tooling. The struct definitions vary per crate
//! (constructor surface mirrors the wasm side), but the trait impls are
//! identical up to the crate-name literal embedded in the error strings.
//!
//! `native_stub_impls!` emits both trait impls for a given
//! `(transport, connection)` pair. Call it once per browser-transport
//! crate's `stub` module; the caller's crate name is picked up at
//! expansion time via `env!("CARGO_PKG_NAME")`, since `#[macro_export]`
//! macros expand textually in the *caller's* crate.

/// Emit `RawTransport` and `RawConnection` impls whose method bodies
/// return `Error::Transport` tagged with the caller's crate name, except
/// `accept` (parks forever — the workspace builds native, but native
/// callers are never expected to drive these transports) and `close`
/// (returns `Ok`).
#[macro_export]
macro_rules! native_stub_impls {
    (
        transport = $transport:ident,
        connection = $connection:ident $(,)?
    ) => {
        #[::async_trait::async_trait(?Send)]
        impl ::sunset_sync::RawTransport for $transport {
            type Connection = $connection;

            async fn connect(
                &self,
                _: ::sunset_sync::PeerAddr,
            ) -> ::sunset_sync::Result<Self::Connection> {
                Err(::sunset_sync::Error::Transport(
                    concat!(env!("CARGO_PKG_NAME"), ": native stub — must be built for wasm32")
                        .into(),
                ))
            }

            async fn accept(&self) -> ::sunset_sync::Result<Self::Connection> {
                ::std::future::pending::<()>().await;
                unreachable!();
            }
        }

        #[::async_trait::async_trait(?Send)]
        impl ::sunset_sync::RawConnection for $connection {
            async fn send_reliable(&self, _: ::bytes::Bytes) -> ::sunset_sync::Result<()> {
                Err(::sunset_sync::Error::Transport(
                    concat!(env!("CARGO_PKG_NAME"), ": native stub").into(),
                ))
            }
            async fn recv_reliable(&self) -> ::sunset_sync::Result<::bytes::Bytes> {
                Err(::sunset_sync::Error::Transport(
                    concat!(env!("CARGO_PKG_NAME"), ": native stub").into(),
                ))
            }
            async fn send_unreliable(&self, _: ::bytes::Bytes) -> ::sunset_sync::Result<()> {
                Err(::sunset_sync::Error::Transport(
                    concat!(env!("CARGO_PKG_NAME"), ": native stub").into(),
                ))
            }
            async fn recv_unreliable(&self) -> ::sunset_sync::Result<::bytes::Bytes> {
                Err(::sunset_sync::Error::Transport(
                    concat!(env!("CARGO_PKG_NAME"), ": native stub").into(),
                ))
            }
            async fn close(&self) -> ::sunset_sync::Result<()> {
                Ok(())
            }
        }
    };
}
