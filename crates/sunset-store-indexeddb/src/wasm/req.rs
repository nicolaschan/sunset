//! Bridge between the callback-based IDBRequest API and Rust async.
//!
//! Each helper takes an `IDBRequest` (or factory open-request) and yields
//! a `Future` that resolves on `success` and rejects on `error`. We do
//! NOT use `JsFuture::from(Promise)` here because IDB exposes `EventTarget`,
//! not Promises, so we wire `onsuccess` / `onerror` closures manually.

use std::cell::RefCell;
use std::rc::Rc;

use futures::channel::oneshot;
use js_sys::Function;
use sunset_store::Error;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;
use web_sys::{DomException, IdbOpenDbRequest, IdbRequest, IdbTransaction};

/// One-shot bridge: install onsuccess / onerror on `request` and return
/// a future that resolves to `request.result()` (on success) or to a
/// `sunset_store::Error::Backend` describing the DOMException (on error).
///
/// The closures live until the request fires; we keep both alive via
/// `Rc` so dropping the future before completion does not detach the
/// callbacks (IDBRequest will fire success/error regardless and the
/// Rc guard prevents UB in the closure body).
pub(crate) async fn await_request(request: IdbRequest) -> Result<wasm_bindgen::JsValue, Error> {
    let (tx, rx) = oneshot::channel::<Result<wasm_bindgen::JsValue, Error>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let success = {
        let req = request.clone();
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            if let Some(sender) = tx.borrow_mut().take() {
                let result = req.result().unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
                let _ = sender.send(Ok(result));
            }
        }) as Box<dyn FnMut()>)
    };
    let error = {
        let req = request.clone();
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            if let Some(sender) = tx.borrow_mut().take() {
                let _ = sender.send(Err(req_error(&req)));
            }
        }) as Box<dyn FnMut()>)
    };

    request.set_onsuccess(Some(success.as_ref().unchecked_ref::<Function>()));
    request.set_onerror(Some(error.as_ref().unchecked_ref::<Function>()));

    let outcome = rx
        .await
        .map_err(|_| Error::Backend("IDB request closure dropped".to_string()))?;

    // Closures were owned by this function; they drop on return. The
    // request's onsuccess/onerror handlers, by then, have fired or were
    // already dispatched by the event loop before we got control back.
    drop(success);
    drop(error);
    outcome
}

/// Like `await_request`, but for an `IDBOpenDBRequest`. Includes
/// `onupgradeneeded` so callers can install their schema during the
/// upgrade transaction. The upgrade callback runs synchronously
/// inside the event handler.
pub(crate) async fn await_open<F>(
    request: IdbOpenDbRequest,
    on_upgrade: F,
) -> Result<wasm_bindgen::JsValue, Error>
where
    F: FnOnce(&web_sys::IdbDatabase, u32, u32) + 'static,
{
    let (tx, rx) = oneshot::channel::<Result<wasm_bindgen::JsValue, Error>>();
    let tx = Rc::new(RefCell::new(Some(tx)));
    let on_upgrade = Rc::new(RefCell::new(Some(on_upgrade)));

    let upgrade = {
        let req = request.clone();
        let on_upgrade = on_upgrade.clone();
        Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
            // `event.target` is the IDBOpenDBRequest; result() is the
            // newly-created (or upgrading) IDBDatabase.
            let db_value = req.result().expect("upgrade has result");
            let db: web_sys::IdbDatabase =
                db_value.dyn_into().expect("upgrade target is IdbDatabase");
            let old_version = event.old_version() as u32;
            let new_version = event.new_version().unwrap_or(0.0) as u32;
            if let Some(callback) = on_upgrade.borrow_mut().take() {
                callback(&db, old_version, new_version);
            }
        }) as Box<dyn FnMut(web_sys::IdbVersionChangeEvent)>)
    };

    let success = {
        let req = request.clone();
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            if let Some(sender) = tx.borrow_mut().take() {
                let result = req.result().unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
                let _ = sender.send(Ok(result));
            }
        }) as Box<dyn FnMut()>)
    };
    let error = {
        let req: IdbRequest = request.clone().unchecked_into();
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            if let Some(sender) = tx.borrow_mut().take() {
                let _ = sender.send(Err(req_error(&req)));
            }
        }) as Box<dyn FnMut()>)
    };
    let blocked = {
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            // Another tab still holds an old version of the database.
            // Surface as a backend error rather than hanging forever.
            if let Some(sender) = tx.borrow_mut().take() {
                let _ = sender.send(Err(Error::Backend(
                    "IDB open blocked: another tab still holds the database open".to_string(),
                )));
            }
        }) as Box<dyn FnMut()>)
    };

    request.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref::<Function>()));
    request.set_onsuccess(Some(success.as_ref().unchecked_ref::<Function>()));
    request.set_onerror(Some(error.as_ref().unchecked_ref::<Function>()));
    request.set_onblocked(Some(blocked.as_ref().unchecked_ref::<Function>()));

    let outcome = rx
        .await
        .map_err(|_| Error::Backend("IDB open closure dropped".to_string()))?;

    drop(upgrade);
    drop(success);
    drop(error);
    drop(blocked);
    outcome
}

/// Wait for an IDBTransaction to complete (or abort / error). IDB does
/// not require an explicit `commit()` — the transaction auto-commits
/// when control returns to the event loop with no pending requests —
/// but we DO want to wait for that auto-commit so callers know writes
/// are durable before broadcasting.
pub(crate) async fn await_transaction(txn: IdbTransaction) -> Result<(), Error> {
    let (tx, rx) = oneshot::channel::<Result<(), Error>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let on_complete = {
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            if let Some(sender) = tx.borrow_mut().take() {
                let _ = sender.send(Ok(()));
            }
        }) as Box<dyn FnMut()>)
    };
    let on_abort = {
        let txn = txn.clone();
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            if let Some(sender) = tx.borrow_mut().take() {
                let _ = sender.send(Err(txn_error(&txn, "transaction aborted")));
            }
        }) as Box<dyn FnMut()>)
    };
    let on_error = {
        let txn = txn.clone();
        let tx = tx.clone();
        Closure::wrap(Box::new(move || {
            if let Some(sender) = tx.borrow_mut().take() {
                let _ = sender.send(Err(txn_error(&txn, "transaction error")));
            }
        }) as Box<dyn FnMut()>)
    };

    txn.set_oncomplete(Some(on_complete.as_ref().unchecked_ref::<Function>()));
    txn.set_onabort(Some(on_abort.as_ref().unchecked_ref::<Function>()));
    txn.set_onerror(Some(on_error.as_ref().unchecked_ref::<Function>()));

    let outcome = rx
        .await
        .map_err(|_| Error::Backend("IDB transaction closure dropped".to_string()))?;

    drop(on_complete);
    drop(on_abort);
    drop(on_error);
    outcome
}

fn req_error(req: &IdbRequest) -> Error {
    match req.error() {
        Ok(Some(dom)) => Error::Backend(format_dom_exception(&dom)),
        Ok(None) => Error::Backend("IDB request failed (no DOMException)".to_string()),
        Err(_) => Error::Backend("IDB request failed (no DOMException accessor)".to_string()),
    }
}

fn txn_error(txn: &IdbTransaction, prefix: &str) -> Error {
    match txn.error() {
        Some(dom) => Error::Backend(format!("{prefix}: {}", format_dom_exception(&dom))),
        None => Error::Backend(prefix.to_string()),
    }
}

fn format_dom_exception(dom: &DomException) -> String {
    let name = dom.name();
    let msg = dom.message();
    if msg.is_empty() {
        name
    } else {
        format!("{name}: {msg}")
    }
}
