//! Command dispatcher correctness — no UI, no relay involved.

mod helpers;

use sunset_cli::command::{Command, parse};
use sunset_cli::dispatch::{DispatchOutcome, dispatch};

#[tokio::test(flavor = "current_thread")]
async fn join_then_switch_then_leave() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let client = helpers::fresh_client();
            assert!(matches!(
                dispatch(&client, parse("/join alpha")).await,
                DispatchOutcome::Continue
            ));
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("alpha"));

            dispatch(&client, parse("/join beta")).await;
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("beta"));

            dispatch(&client, parse("/switch alpha")).await;
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("alpha"));

            dispatch(&client, parse("/leave")).await;
            // alpha was active; should fall back to beta.
            assert_eq!(client.snapshot_top().active_room.as_deref(), Some("beta"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn quit_returns_quit() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let client = helpers::fresh_client();
            assert!(matches!(
                dispatch(&client, Command::Quit).await,
                DispatchOutcome::Quit
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn voice_is_a_stub_message() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let client = helpers::fresh_client();
            dispatch(&client, parse("/voice")).await;
            let log = client.snapshot_top().system_log;
            assert!(
                log.iter().any(|l| l.contains("not yet implemented")),
                "system log: {log:?}"
            );
        })
        .await;
}
