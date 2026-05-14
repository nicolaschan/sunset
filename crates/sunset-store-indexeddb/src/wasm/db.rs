//! IndexedDB open / delete + transaction-helper plumbing.

use js_sys::{Function, Uint8Array};
use sunset_store::Error;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use web_sys::{IdbDatabase, IdbFactory, IdbTransaction, IdbTransactionMode};

use super::req::{await_open, await_request};

/// An open `IdbDatabase` plus the closures that keep its event handlers
/// alive. Drop this to release the JS-side closures and (transitively)
/// stop receiving `versionchange` events.
pub(crate) struct OpenDb {
    pub db: IdbDatabase,
    /// Registered on the database via `onversionchange`. When something
    /// else (a sibling tab, or our own `delete_database`) requests a
    /// version bump or deletion, browsers fire `versionchange` on every
    /// open connection. The handler closes the connection so the
    /// requesting context can proceed without "blocked".
    _on_version_change: Closure<dyn FnMut(web_sys::IdbVersionChangeEvent)>,
}

pub(crate) const STORE_ENTRIES: &str = "entries";
pub(crate) const STORE_BLOBS: &str = "blobs";
pub(crate) const STORE_META: &str = "meta";

/// Schema version. Bump when adding indexes or new stores.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Resolve the IDB factory from `window` (browser) or `self` (worker
/// scope, used by Node's `wasm-bindgen-test --node` polyfilled with
/// `fake-indexeddb`).
fn idb_factory() -> Result<IdbFactory, Error> {
    if let Some(window) = web_sys::window() {
        return window
            .indexed_db()
            .map_err(|e| js_to_backend_err(&e))?
            .ok_or_else(|| Error::Backend("indexedDB unavailable on window".to_string()));
    }
    let global = js_sys::global();
    if let Ok(scope) = global.dyn_into::<web_sys::WorkerGlobalScope>() {
        return scope
            .indexed_db()
            .map_err(|e| js_to_backend_err(&e))?
            .ok_or_else(|| Error::Backend("indexedDB unavailable on worker".to_string()));
    }
    Err(Error::Backend("no IndexedDB host context".to_string()))
}

/// Open `database_name`, applying the v1 schema if needed. Returns the
/// open database paired with the JS closures it depends on (the caller
/// must keep `OpenDb` alive — dropping it frees the closures).
pub(crate) async fn open_database(database_name: &str) -> Result<OpenDb, Error> {
    let factory = idb_factory()?;
    let req = factory
        .open_with_u32(database_name, SCHEMA_VERSION)
        .map_err(|e| js_to_backend_err(&e))?;
    let result = await_open(req, |db, _old, _new| {
        // Idempotent: createObjectStore throws if the name already
        // exists, so check first via objectStoreNames(). The caller
        // only fires this on fresh creation or schema upgrade, so a
        // missing store always means "create me".
        let existing = db.object_store_names();
        let mut names = Vec::with_capacity(existing.length() as usize);
        for i in 0..existing.length() {
            if let Some(n) = existing.item(i) {
                names.push(n);
            }
        }
        if !names.iter().any(|n| n == STORE_ENTRIES) {
            let _ = db.create_object_store(STORE_ENTRIES);
        }
        if !names.iter().any(|n| n == STORE_BLOBS) {
            let _ = db.create_object_store(STORE_BLOBS);
        }
        if !names.iter().any(|n| n == STORE_META) {
            let _ = db.create_object_store(STORE_META);
        }
    })
    .await?;
    let db: IdbDatabase = result
        .dyn_into()
        .map_err(|_| Error::Backend("open_database: result was not IdbDatabase".to_string()))?;
    let on_version_change = {
        let db = db.clone();
        Closure::wrap(Box::new(move |_event: web_sys::IdbVersionChangeEvent| {
            // Close the connection so a sibling delete / upgrade isn't
            // blocked. Subsequent operations on this `IdbDatabase` will
            // fail; the surrounding context (the browser app) is about
            // to reload anyway.
            db.close();
        }) as Box<dyn FnMut(web_sys::IdbVersionChangeEvent)>)
    };
    db.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref::<Function>()));
    Ok(OpenDb {
        db,
        _on_version_change: on_version_change,
    })
}

/// Delete `database_name`. Used by the "reset local state" flow.
pub async fn delete_database(database_name: &str) -> Result<(), Error> {
    let factory = idb_factory()?;
    let req = factory
        .delete_database(database_name)
        .map_err(|e| js_to_backend_err(&e))?;
    let opener: web_sys::IdbOpenDbRequest = req;
    let _ = await_open(opener, |_, _, _| {}).await?;
    Ok(())
}

/// Convert a `JsValue` (from a wasm-bindgen Result<_, JsValue>) into an
/// `Error::Backend` describing the JS-side failure.
pub(crate) fn js_to_backend_err(v: &JsValue) -> Error {
    let s = v
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(v, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{v:?}"));
    Error::Backend(format!("idb js error: {s}"))
}

/// Build a read-only transaction over `stores`.
pub(crate) fn txn_ro(db: &IdbDatabase, stores: &[&str]) -> Result<IdbTransaction, Error> {
    let arr = js_sys::Array::new();
    for s in stores {
        arr.push(&JsValue::from_str(s));
    }
    db.transaction_with_str_sequence(&arr)
        .map_err(|e| js_to_backend_err(&e))
}

/// Build a read-write transaction over `stores`.
pub(crate) fn txn_rw(db: &IdbDatabase, stores: &[&str]) -> Result<IdbTransaction, Error> {
    let arr = js_sys::Array::new();
    for s in stores {
        arr.push(&JsValue::from_str(s));
    }
    db.transaction_with_str_sequence_and_mode(&arr, IdbTransactionMode::Readwrite)
        .map_err(|e| js_to_backend_err(&e))
}

/// Wrap raw bytes as a `Uint8Array` for use as an IDB key or value.
pub(crate) fn bytes_to_js(b: &[u8]) -> Uint8Array {
    let arr = Uint8Array::new_with_length(b.len() as u32);
    arr.copy_from(b);
    arr
}

/// Read a value back into raw bytes if it is `undefined`/`null`-free.
pub(crate) fn js_to_bytes(value: &JsValue) -> Option<Vec<u8>> {
    if value.is_undefined() || value.is_null() {
        return None;
    }
    let arr = value.dyn_ref::<Uint8Array>()?;
    Some(arr.to_vec())
}

/// Read all `(key, value)` pairs from an object store. Used for the
/// initial scan in `iter()` / history snapshot in `subscribe()`.
pub(crate) async fn read_all_values(
    txn: &IdbTransaction,
    store: &str,
) -> Result<Vec<Vec<u8>>, Error> {
    let object_store = txn.object_store(store).map_err(|e| js_to_backend_err(&e))?;
    let req = object_store.get_all().map_err(|e| js_to_backend_err(&e))?;
    let result = await_request(req).await?;
    let array = js_sys::Array::from(&result);
    let mut out = Vec::with_capacity(array.length() as usize);
    for i in 0..array.length() {
        let v = array.get(i);
        if let Some(b) = js_to_bytes(&v) {
            out.push(b);
        }
    }
    Ok(out)
}
