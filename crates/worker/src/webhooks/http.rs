//! `WebhookHttp` over the runtime's `fetch`: POST, no redirects followed, a hard timeout.

use plumblog_webhook::{HTTP_TIMEOUT_MS, HttpError, WebhookHttp};
use worker::js_sys;
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::web_sys::{
    AbortSignal, Headers, RequestInit, RequestRedirect, Response, WorkerGlobalScope,
};

/// The Worker's `fetch`.
pub struct Fetch;

impl WebhookHttp for Fetch {
    async fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> Result<u16, HttpError> {
        let h = Headers::new().map_err(|_| HttpError::Network)?;
        for (name, value) in headers {
            h.set(name, value).map_err(|_| HttpError::Network)?;
        }
        // The timer covers connect, request and response headers; an abort rejects the
        // fetch, so a receiver that never answers ends as Timeout instead of hanging.
        let signal =
            AbortSignal::timeout_with_u32(u32::try_from(HTTP_TIMEOUT_MS).unwrap_or(u32::MAX));
        let init = RequestInit::new();
        init.set_method("POST");
        init.set_headers(&h);
        init.set_body(&JsValue::from_str(body));
        // A 3xx comes back as itself: following it could reach a host the URL guard never saw.
        init.set_redirect(RequestRedirect::Manual);
        init.set_signal(Some(&signal));
        crate::subrequests::count();
        let scope: WorkerGlobalScope = js_sys::global().unchecked_into();
        let res = JsFuture::from(scope.fetch_with_str_and_init(url, &init)).await;
        match res {
            Ok(r) => {
                let r: Response = r.unchecked_into();
                let status = r.status();
                // workerd returns a manual redirect as its real 3xx; a browser-style runtime
                // gives an opaque one (type `opaqueredirect`, status 0). Both are redirects.
                let kind = js_sys::Reflect::get(&r, &JsValue::from_str("type"))
                    .ok()
                    .and_then(|t| t.as_string());
                if status == 0 || kind.as_deref() == Some("opaqueredirect") {
                    return Err(HttpError::Redirect);
                }
                // Only the status matters. Cancelling frees the connection at once (a Worker
                // has few simultaneous ones); the timer still bounds a body that stalls.
                if let Some(b) = r.body() {
                    let _ = JsFuture::from(b.cancel()).await;
                }
                Ok(status)
            }
            Err(_) if signal.aborted() => Err(HttpError::Timeout),
            Err(e) => {
                // The error's name only: its message may quote the URL, which may hold a
                // receiver's token. The system log entry has the host and reason.
                let name = js_sys::Reflect::get(&e, &JsValue::from_str("name"))
                    .ok()
                    .and_then(|n| n.as_string())
                    .unwrap_or_default();
                worker::console_warn!("webhook.fetch_failed error_name={name}");
                Err(HttpError::Network)
            }
        }
    }
}
