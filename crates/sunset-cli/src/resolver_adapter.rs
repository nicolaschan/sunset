//! `reqwest`-backed [`HttpFetch`] for the CLI's hostname-based
//! relay descriptor lookups. Mirrors `sunset-relay`'s adapter; that
//! one is `pub(crate)`, so we duplicate rather than dragging the
//! relay binary into our dep graph.

use async_trait::async_trait;
use sunset_relay_resolver::{Error, HttpFetch, Result};

pub struct ReqwestFetch {
    client: reqwest::Client,
}

impl ReqwestFetch {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestFetch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait(?Send)]
impl HttpFetch for ReqwestFetch {
    async fn get(&self, url: &str) -> Result<String> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Http(format!("send: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Http(format!("status {}", resp.status())));
        }
        resp.text()
            .await
            .map_err(|e| Error::Http(format!("body: {e}")))
    }
}
