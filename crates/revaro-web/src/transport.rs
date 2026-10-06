//! Browser bridge to the shared file transport. UI components own no recovery policy.
use crate::api::RequestError;
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use web_sys::{AbortSignal, Blob};

#[wasm_bindgen(raw_module = "/transport-client.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = putBlob)]
    async fn put_blob_js(
        url: &str,
        body: &Blob,
        content_type: Option<&str>,
        signal: &AbortSignal,
        on_progress: &js_sys::Function,
    ) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = blobHash)]
    async fn blob_hash_js(body: &Blob) -> Result<JsValue, JsValue>;
}
fn error(value: JsValue) -> RequestError {
    let status = js_sys::Reflect::get(&value, &JsValue::from_str("status"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u16;
    let message = js_sys::Reflect::get(&value, &JsValue::from_str("message"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "文件传输失败，请重试".to_owned());
    RequestError {
        status,
        code: None,
        message,
    }
}
pub async fn blob_hash(body: &Blob) -> Result<String, RequestError> {
    blob_hash_js(body)
        .await
        .map(|v| v.as_string().unwrap_or_default())
        .map_err(error)
}
pub async fn put_blob(
    url: &str,
    body: &Blob,
    content_type: Option<&str>,
    signal: &AbortSignal,
    on_progress: Rc<dyn Fn(u64)>,
) -> Result<String, RequestError> {
    let callback =
        Closure::<dyn FnMut(f64)>::new(move |loaded: f64| on_progress(loaded.max(0.0) as u64));
    put_blob_js(
        url,
        body,
        content_type,
        signal,
        callback.as_ref().unchecked_ref(),
    )
    .await
    .map(|v| v.as_string().unwrap_or_default())
    .map_err(error)
}
