//! `web-sys::fetch`-backed [`HttpFetch`] for the browser. Mirrors the
//! native `ReqwestFetch` adapter; both implement the same trait so
//! the resolver crate stays platform-neutral.

use async_trait::async_trait;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

use sunset_relay_resolver::{Error, HttpFetch, Result};

pub(crate) struct WebSysFetch;

#[async_trait(?Send)]
impl HttpFetch for WebSysFetch {
    async fn get(&self, url: &str) -> Result<String> {
        let opts = RequestInit::new();
        opts.set_method("GET");
        let req = Request::new_with_str_and_init(url, &opts)
            .map_err(|e| Error::Http(format!("Request::new: {e:?}")))?;
        let window = web_sys::window().ok_or_else(|| Error::Http("no window".into()))?;
        let resp_value = JsFuture::from(window.fetch_with_request(&req))
            .await
            .map_err(|e| Error::Http(format!("fetch: {e:?}")))?;
        let resp: Response = resp_value
            .dyn_into()
            .map_err(|_| Error::Http("not a Response".into()))?;
        if !resp.ok() {
            return Err(Error::Http(format!("status {}", resp.status())));
        }
        let text_promise = resp
            .text()
            .map_err(|e| Error::Http(format!("text(): {e:?}")))?;
        let text = JsFuture::from(text_promise)
            .await
            .map_err(|e| Error::Http(format!("await text: {e:?}")))?;
        text.as_string()
            .ok_or_else(|| Error::Http("body not a string".into()))
    }
}
