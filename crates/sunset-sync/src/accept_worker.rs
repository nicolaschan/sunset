//! Reusable "spawn one task per inbound item" worker for resilient,
//! parallel transport `accept()` paths.
//!
//! Background: a transport that accepts inbound connections typically
//! has a slow per-item phase (TCP→WS upgrade, Noise responder, WebRTC
//! ICE/DTLS). Running these inline in `Transport::accept` serializes
//! them — one slow or misbehaving peer wedges the engine's accept
//! loop. This helper spawns one task per item so the slow path runs
//! concurrently and a stuck task only consumes its own slot.
//!
//! Two policies are baked in:
//!   * **Per-task timeout** — bounds how long any single handshake
//!     can wedge a slot. On timeout the task's future is dropped,
//!     which closes the underlying TCP / data channel.
//!   * **Inflight cap (semaphore)** — bounds total concurrent
//!     handshakes so a flood of bad peers can't exhaust task / FD
//!     budgets. Items wait for a permit before spawning.

#[allow(unused_imports)]
use std::future::Future;
#[allow(unused_imports)]
use std::rc::Rc;
#[allow(unused_imports)]
use std::time::Duration;

#[allow(unused_imports)]
use futures::stream::{Stream, StreamExt};
#[allow(unused_imports)]
use tokio::sync::{Mutex, mpsc};

#[allow(unused_imports)]
use crate::error::{Error, Result};
#[allow(unused_imports)]
use crate::spawn::spawn_local;

// (Implementation will be added in Task 2.)
