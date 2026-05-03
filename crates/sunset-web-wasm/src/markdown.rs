//! WASM export for `sunset_markdown::parse`. Returns the parsed
//! `Document` to JS as a structured value via `serde-wasm-bindgen`.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn parse_markdown(input: &str) -> JsValue {
    let doc = sunset_markdown::parse(input);
    serde_wasm_bindgen::to_value(&doc).expect("AST is plain data; serialization cannot fail")
}

#[wasm_bindgen]
pub fn to_plain_markdown(input: &str) -> String {
    sunset_markdown::to_plain(&sunset_markdown::parse(input))
}
