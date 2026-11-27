//! Blocking client for the Cloudflare REST API.
//!
//! Envelope `{success, errors: [{code, message}], result, result_info}`:
//! <https://developers.cloudflare.com/fundamentals/api/how-to/make-api-calls/>

use super::endpoint::{Auth, Endpoint, Method, Permission, TOKENS_PAGE};
use super::token::ApiToken;
use crate::text::{Code, Text};
use plumblog_i18n::key;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt::{self, Write};
use std::thread::sleep;
use std::time::Duration;

/// The only host the token is ever sent to, outside tests.
pub const DEFAULT_BASE: &str = "https://api.cloudflare.com/client/v4";
/// Test-only base override; accepted only for a loopback `http://` address.
pub const BASE_ENV: &str = "PLUMBLOG_CF_API_BASE";

/// Tries per GET (writes are tried once: a retried write could run twice).
const GET_TRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_millis(500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole call, body included; a Worker upload is about 1 MB.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Cloudflare error codes that mean the token is wrong (wrangler treats 9106 the same way).
const BAD_TOKEN_CODES: &[i64] = &[1000, 9106];
/// Codes Cloudflare returns when a valid token lacks a permission or account access.
const NO_PERMISSION_CODES: &[i64] = &[9109, 10000, 10405];
/// Characters of a non-JSON reply kept in an error, after scrubbing.
const EXCERPT_CHARS: usize = 200;
/// Stops a paging loop on a server that never says it is done.
const MAX_PAGES: u32 = 1000;

/// Request body.
#[derive(Default)]
pub enum Body {
    #[default]
    None,
    Json(Value),
    /// e.g. multipart for Worker uploads; the caller builds the content type and bytes.
    Raw {
        content_type: String,
        bytes: Vec<u8>,
    },
}

impl fmt::Debug for Body {
    /// Kind and size only: bodies carry secrets (Worker secrets, D1 data).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Json(v) => write!(f, "Json({} bytes)", v.to_string().len()),
            Self::Raw {
                content_type,
                bytes,
            } => write!(f, "Raw({content_type}, {} bytes)", bytes.len()),
        }
    }
}

/// One API call: an endpoint from the table plus its path values, query, body and the
/// secrets it carries (hidden from every error).
pub struct Call {
    endpoint: Endpoint,
    params: Vec<(&'static str, String)>,
    query: Vec<(&'static str, String)>,
    body: Body,
    upload_jwt: Option<ApiToken>,
    secrets: Vec<ApiToken>,
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let query: Vec<&str> = self.query.iter().map(|(k, _)| *k).collect();
        f.debug_struct("Call")
            .field("endpoint", &self.endpoint)
            .field("params", &self.params)
            .field("query_names", &query)
            .field("body", &self.body)
            .field("upload_jwt", &self.upload_jwt)
            .field("secrets", &self.secrets.len())
            .finish()
    }
}

impl Call {
    #[must_use]
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            params: Vec::new(),
            query: Vec::new(),
            body: Body::None,
            upload_jwt: None,
            secrets: Vec::new(),
        }
    }

    /// Fills `{name}` in the endpoint path. `{account_id}` comes from the client.
    #[must_use]
    pub fn param(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.params.push((name, value.into()));
        self
    }

    #[must_use]
    pub fn query(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.query.push((name, value.into()));
        self
    }

    #[must_use]
    pub fn json(mut self, body: Value) -> Self {
        self.body = Body::Json(body);
        self
    }

    #[must_use]
    pub fn raw(mut self, content_type: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.body = Body::Raw {
            content_type: content_type.into(),
            bytes,
        };
        self
    }

    /// Required by, and only accepted for, [`Auth::UploadJwt`] endpoints. Hidden in errors.
    #[must_use]
    pub fn upload_jwt(mut self, jwt: ApiToken) -> Self {
        self.upload_jwt = Some(jwt);
        self
    }

    /// A secret value this call sends (Worker secret, Turnstile secret...): Cloudflare may
    /// echo it in an error, so every error text of this call replaces it with `[hidden]`.
    #[must_use]
    pub fn secret(mut self, value: ApiToken) -> Self {
        self.secrets.push(value);
        self
    }

    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }
}

/// One `{code, message}` from the Cloudflare envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudflareMessage {
    pub code: i64,
    pub message: String,
}

/// The envelope's `result_info`: page numbers, or a cursor (R2), depending on the endpoint.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ResultInfo {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub count: Option<u32>,
    pub total_count: Option<u32>,
    pub total_pages: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// 401 or a bad-token code: the token is wrong, deleted or rolled.
    InvalidToken,
    /// 403 or a permission code; carries the table's permission for the endpoint.
    MissingPermission(Option<Permission>),
    /// Any other error status or `success: false`.
    Http,
    /// No reply: DNS, connect, TLS, timeout, dropped connection.
    Network,
    /// A reply that is not the Cloudflare envelope.
    Malformed,
    /// `PLUMBLOG_CF_API_BASE` is not a loopback test server.
    BaseRefused,
    /// Refused before sending: a bug in the caller (bad path value, missing account).
    Usage,
}

/// A failed call. Every text in it is scrubbed of the token and the call's secrets.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    pub endpoint: Option<Endpoint>,
    pub status: Option<u16>,
    pub errors: Vec<CloudflareMessage>,
    /// Transport error, reply excerpt, refused value or internal diagnostic. Not translated.
    pub detail: String,
    pub tries: u32,
}

impl ApiError {
    fn usage(endpoint: Option<Endpoint>, detail: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::Usage,
            endpoint,
            status: None,
            errors: Vec::new(),
            detail: detail.into(),
            tries: 0,
        }
    }

    /// True if any Cloudflare error code in the reply equals `code`.
    #[must_use]
    pub fn has_code(&self, code: i64) -> bool {
        self.errors.iter().any(|e| e.code == code)
    }

    fn is_write(&self) -> bool {
        self.endpoint
            .is_some_and(|e| e.spec().method != Method::Get)
    }

    #[must_use]
    pub fn code(&self) -> Code {
        match self.kind {
            ApiErrorKind::InvalidToken => Code::TokenInvalid,
            ApiErrorKind::MissingPermission(Some(_)) => Code::PermissionMissing,
            ApiErrorKind::MissingPermission(None) => Code::AccessDenied,
            ApiErrorKind::Http => Code::HttpFailed,
            ApiErrorKind::Network if self.is_write() => Code::NetworkWriteUncertain,
            ApiErrorKind::Network => Code::Network,
            ApiErrorKind::Malformed => Code::ReplyMalformed,
            ApiErrorKind::BaseRefused => Code::BaseRefused,
            ApiErrorKind::Usage => Code::Internal,
        }
    }

    /// `HTTP 403; [10000] Authentication error`: Cloudflare's own words, not translated.
    fn reply(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = self.status {
            parts.push(format!("HTTP {s}"));
        }
        for e in &self.errors {
            parts.push(format!("[{}] {}", e.code, e.message));
        }
        if self.errors.is_empty() && !self.detail.is_empty() {
            parts.push(self.detail.clone());
        }
        parts.join("; ")
    }

    /// `GET /accounts/{account_id}/d1/database`.
    fn call_name(&self) -> String {
        self.endpoint
            .map(|e| format!("{} {}", e.spec().method.as_str(), e.spec().path))
            .unwrap_or_default()
    }

    /// The message for the owner, in `text`'s language.
    #[must_use]
    pub fn message(&self, text: &Text) -> String {
        let what = self
            .endpoint
            .map(|e| text.t(e.spec().summary, &[]))
            .unwrap_or_default();
        let args = [
            ("what", what),
            ("call", self.call_name()),
            ("reply", self.reply()),
            ("detail", self.detail.clone()),
            ("page", TOKENS_PAGE.to_string()),
            ("var", BASE_ENV.to_string()),
            ("value", self.detail.clone()),
            (
                "tries",
                text.t(
                    key!("cli.error.tries"),
                    &[("count", self.tries.to_string())],
                ),
            ),
            (
                "permission",
                match self.kind {
                    ApiErrorKind::MissingPermission(Some(p)) => p.label(),
                    _ => String::new(),
                },
            ),
            ("token_var", super::token::TOKEN_VAR.to_string()),
        ];
        let key = match self.code() {
            Code::TokenInvalid => key!("cli.error.token_invalid"),
            Code::PermissionMissing => key!("cli.error.permission_missing"),
            Code::AccessDenied => key!("cli.error.access_denied"),
            Code::Network => key!("cli.error.network"),
            Code::NetworkWriteUncertain => key!("cli.error.network_write_uncertain"),
            Code::ReplyMalformed => key!("cli.error.reply_malformed"),
            Code::BaseRefused => key!("cli.error.base_refused"),
            Code::Internal => key!("cli.error.internal"),
            _ => key!("cli.error.http_failed"),
        };
        text.t(key, &args)
    }
}

/// Language-free, for logs and `--json`: the code, the call and Cloudflare's reply.
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.code().as_str(),
            self.call_name(),
            self.reply()
        )?;
        if !self.errors.is_empty() && !self.detail.is_empty() {
            write!(f, " {}", self.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

/// The Cloudflare API, authenticated with one token.
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: ApiToken,
    account_id: Option<String>,
    retry_delay: Duration,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token)
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

/// Checks a base URL override. Only loopback over plain HTTP passes, so a stray or hostile
/// env value can never send the token to another host.
pub(crate) fn base_override_ok(base: &str) -> bool {
    let Some(rest) = base
        .strip_prefix("http://127.0.0.1")
        .or_else(|| base.strip_prefix("http://[::1]"))
    else {
        return false;
    };
    // After the host: nothing, `:port`, then nothing or a path. Anything else (`@`, `.`) could
    // turn the loopback text into userinfo or a subdomain of another host.
    let after_port = match rest.strip_prefix(':') {
        Some(p) => {
            let digits = p.bytes().take_while(u8::is_ascii_digit).count();
            if !p[..digits].parse::<u16>().is_ok_and(|n| n > 0) {
                return false;
            }
            &p[digits..]
        }
        None => rest,
    };
    (after_port.is_empty() || after_port.starts_with('/'))
        && after_port
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
}

fn scrub_control(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Path values become URL segments; only id-like text may, so no value can add a segment,
/// a query or `..`.
fn valid_segment(v: &str) -> bool {
    !v.is_empty()
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// What one attempt produced, before deciding on a retry.
enum Attempt {
    Reply(u16, String),
    Failed(String),
}

impl Client {
    /// A client for [`DEFAULT_BASE`], or `base_override` if it is a loopback test server.
    ///
    /// # Errors
    /// `BaseRefused` if `base_override` is not `http://127.0.0.1[:port]` or
    /// `http://[::1][:port]`.
    pub fn new(token: ApiToken, base_override: Option<&str>) -> Result<Self, ApiError> {
        if let Some(b) = base_override
            && !base_override_ok(b)
        {
            let shown = scrub_control(&b.replace(token.expose(), "[hidden]"));
            return Err(ApiError {
                kind: ApiErrorKind::BaseRefused,
                ..ApiError::usage(None, shown)
            });
        }
        let local = base_override.is_some();
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // A redirect could carry the Authorization header somewhere else.
            .max_redirects(0)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(CALL_TIMEOUT))
            .user_agent(concat!("plumblog-cli/", env!("CARGO_PKG_VERSION")))
            .https_only(!local);
        if local {
            // A proxy from the environment must not see test traffic or reroute it.
            config = config.proxy(None);
        }
        Ok(Self {
            agent: config.build().into(),
            base: base_override.map_or_else(
                || DEFAULT_BASE.to_string(),
                |b| b.trim_end_matches('/').to_string(),
            ),
            token,
            account_id: None,
            retry_delay: RETRY_DELAY,
        })
    }

    /// [`Client::new`] with the override read from `PLUMBLOG_CF_API_BASE` (empty = unset).
    ///
    /// # Errors
    /// As [`Client::new`].
    pub fn from_env(token: ApiToken) -> Result<Self, ApiError> {
        let base = std::env::var(BASE_ENV).ok().filter(|v| !v.is_empty());
        Self::new(token, base.as_deref())
    }

    /// The account every `{account_id}` path uses.
    ///
    /// # Errors
    /// `Usage` if `id` is not id-like.
    pub fn set_account(&mut self, id: &str) -> Result<(), ApiError> {
        if !valid_segment(id) {
            return Err(ApiError::usage(
                None,
                format!("not a Cloudflare account id: {:?}", self.scrub(id, &[])),
            ));
        }
        self.account_id = Some(id.to_string());
        Ok(())
    }

    /// True when talking to a loopback test server instead of Cloudflare.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.base != DEFAULT_BASE
    }

    #[must_use]
    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    /// First wait between GET tries (doubles each time). Tests set zero.
    pub fn set_retry_delay(&mut self, delay: Duration) {
        self.retry_delay = delay;
    }

    /// Replaces the token and `extra` secrets wherever they might be echoed back.
    pub(crate) fn scrub(&self, text: &str, extra: &[&ApiToken]) -> String {
        let mut s = text.to_string();
        for secret in std::iter::once(&self.token).chain(extra.iter().copied()) {
            let v = secret.expose();
            if !v.is_empty() {
                s = s.replace(v, "[hidden]");
            }
        }
        scrub_control(&s)
    }

    fn url(&self, call: &Call, extra_query: &[(&str, String)]) -> Result<String, ApiError> {
        let spec = call.endpoint.spec();
        let usage = |msg: String| ApiError::usage(Some(call.endpoint), msg);
        let mut path = String::new();
        let mut used = 0;
        for seg in spec.path.split('/').skip(1) {
            path.push('/');
            let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
                path.push_str(seg);
                continue;
            };
            let value = if name == "account_id" {
                self.account_id
                    .as_deref()
                    .ok_or_else(|| usage(format!("no account chosen before {}", spec.path)))?
            } else {
                used += 1;
                call.params
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.as_str())
                    .ok_or_else(|| usage(format!("{} needs {{{name}}}", spec.path)))?
            };
            if !valid_segment(value) {
                return Err(usage(format!(
                    "refusing {{{name}}} = {:?} in {}: only letters, digits, - and _",
                    self.scrub(value, &call.secret_refs()),
                    spec.path
                )));
            }
            path.push_str(value);
        }
        if used != call.params.len() {
            return Err(usage(format!(
                "{} got path values it does not use",
                spec.path
            )));
        }
        let mut url = format!("{}{path}", self.base);
        let pairs = call
            .query
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .chain(extra_query.iter().map(|(k, v)| (*k, v.as_str())));
        for (i, (k, v)) in pairs.enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&percent_encode(k));
            url.push('=');
            url.push_str(&percent_encode(v));
        }
        Ok(url)
    }

    fn attempt(&self, call: &Call, url: &str, bearer: &str) -> Attempt {
        let spec = call.endpoint.spec();
        let auth = format!("Bearer {bearer}");
        let sent = match spec.method {
            Method::Get => self
                .agent
                .get(url)
                .header("Authorization", &auth)
                .header("Accept", "application/json")
                .call(),
            Method::Post | Method::Put => {
                let req = if spec.method == Method::Post {
                    self.agent.post(url)
                } else {
                    self.agent.put(url)
                }
                .header("Authorization", &auth)
                .header("Accept", "application/json");
                match &call.body {
                    Body::None => req.send_empty(),
                    Body::Json(v) => req
                        .content_type("application/json")
                        .send(v.to_string().as_bytes()),
                    Body::Raw {
                        content_type,
                        bytes,
                    } => req.content_type(content_type.as_str()).send(bytes),
                }
            }
        };
        match sent {
            Ok(resp) => {
                let status = resp.status().as_u16();
                match resp.into_body().read_to_string() {
                    Ok(text) => Attempt::Reply(status, text),
                    Err(e) => Attempt::Failed(format!("reading the reply: {e}")),
                }
            }
            Err(e) => Attempt::Failed(e.to_string()),
        }
    }

    /// Sends a call and returns the envelope's `result` and `result_info`.
    ///
    /// GETs are tried up to 3 times on 5xx or network errors; writes are sent once.
    ///
    /// # Errors
    /// [`ApiError`], classified by [`ApiErrorKind`], every text scrubbed of the token and
    /// the call's secrets.
    pub fn call_page(&self, call: &Call) -> Result<(Value, Option<ResultInfo>), ApiError> {
        self.send(call, &[])
    }

    /// [`Client::call_page`] without `result_info`.
    ///
    /// # Errors
    /// As [`Client::call_page`].
    pub fn call(&self, call: &Call) -> Result<Value, ApiError> {
        Ok(self.send(call, &[])?.0)
    }

    /// [`Client::call`], then the `result` read as `T`.
    ///
    /// # Errors
    /// As [`Client::call`], or `Malformed` if `result` does not fit `T`.
    pub fn call_as<T: DeserializeOwned>(&self, call: &Call) -> Result<T, ApiError> {
        let value = self.call(call)?;
        self.decode(call, value)
    }

    fn decode<T: DeserializeOwned>(&self, call: &Call, value: Value) -> Result<T, ApiError> {
        serde_json::from_value(value).map_err(|e| ApiError {
            kind: ApiErrorKind::Malformed,
            endpoint: Some(call.endpoint),
            status: None,
            errors: Vec::new(),
            detail: self.scrub(
                &format!("unexpected result shape: {e}"),
                &call.secret_refs(),
            ),
            tries: 1,
        })
    }

    /// Every item of a paged GET list, following `cursor` or `page` until the last page.
    /// `list_field`: where the items are when `result` is an object (R2: `buckets`).
    ///
    /// # Errors
    /// As [`Client::call_page`]; `Usage` for a write; `Malformed` if a page has no list.
    pub fn call_all(
        &self,
        call: &Call,
        per_page: u32,
        list_field: Option<&str>,
    ) -> Result<Vec<Value>, ApiError> {
        if call.endpoint.spec().method != Method::Get {
            return Err(ApiError::usage(
                Some(call.endpoint),
                format!("paging a write: {}", call.endpoint.spec().path),
            ));
        }
        let mut items = Vec::new();
        let mut page = 1;
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut q = vec![("per_page", per_page.to_string())];
            match &cursor {
                Some(c) => q.push(("cursor", c.clone())),
                None => q.push(("page", page.to_string())),
            }
            let (mut result, info) = self.send(call, &q)?;
            let list = match list_field {
                Some(f) => result.get_mut(f).map(Value::take),
                None => Some(result),
            };
            let Some(Value::Array(list)) = list else {
                return Err(Self::malformed(call, "a list page without a list"));
            };
            let got = list.len();
            items.extend(list);
            let info = info.unwrap_or_default();
            if let Some(c) = info.cursor.filter(|c| !c.is_empty()) {
                cursor = Some(c);
                continue;
            }
            let more = match info.total_pages {
                Some(total) => page < total,
                None => u32::try_from(got).is_ok_and(|g| g >= per_page) && cursor.is_none(),
            };
            if got == 0 || !more || cursor.is_some() {
                return Ok(items);
            }
            page += 1;
        }
        Err(Self::malformed(call, "more than 1000 pages"))
    }

    /// [`Client::call_all`] with every item read as `T`.
    ///
    /// # Errors
    /// As [`Client::call_all`], or `Malformed` if an item does not fit `T`.
    pub fn call_all_as<T: DeserializeOwned>(
        &self,
        call: &Call,
        per_page: u32,
        list_field: Option<&str>,
    ) -> Result<Vec<T>, ApiError> {
        let items = self.call_all(call, per_page, list_field)?;
        self.decode(call, Value::Array(items))
    }

    fn malformed(call: &Call, detail: &str) -> ApiError {
        ApiError {
            kind: ApiErrorKind::Malformed,
            endpoint: Some(call.endpoint),
            status: None,
            errors: Vec::new(),
            detail: detail.to_string(),
            tries: 1,
        }
    }

    fn send(
        &self,
        call: &Call,
        extra_query: &[(&str, String)],
    ) -> Result<(Value, Option<ResultInfo>), ApiError> {
        let endpoint = call.endpoint;
        let spec = endpoint.spec();
        let bearer = match (spec.auth, &call.upload_jwt) {
            (Auth::ApiToken, None) => self.token.expose(),
            (Auth::UploadJwt, Some(jwt)) => jwt.expose(),
            _ => {
                return Err(ApiError::usage(
                    Some(endpoint),
                    format!("wrong credentials for {}", spec.path),
                ));
            }
        };
        if spec.method == Method::Get && !matches!(call.body, Body::None) {
            return Err(ApiError::usage(
                Some(endpoint),
                format!("GET {} with a body", spec.path),
            ));
        }
        let url = self.url(call, extra_query)?;
        let max = if spec.method == Method::Get {
            GET_TRIES
        } else {
            1
        };
        let mut tries = 0;
        loop {
            tries += 1;
            let outcome = self.attempt(call, &url, bearer);
            let retryable = match &outcome {
                Attempt::Reply(status, _) => *status >= 500,
                Attempt::Failed(_) => true,
            };
            if retryable && tries < max {
                sleep(self.retry_delay * 2u32.pow(tries - 1));
                continue;
            }
            return match outcome {
                Attempt::Reply(status, text) => self.envelope(call, status, &text, tries),
                Attempt::Failed(detail) => Err(ApiError {
                    kind: ApiErrorKind::Network,
                    endpoint: Some(endpoint),
                    status: None,
                    errors: Vec::new(),
                    detail: self.scrub(&detail, &call.secret_refs()),
                    tries,
                }),
            };
        }
    }

    fn envelope(
        &self,
        call: &Call,
        status: u16,
        text: &str,
        tries: u32,
    ) -> Result<(Value, Option<ResultInfo>), ApiError> {
        let endpoint = call.endpoint;
        let secrets = call.secret_refs();
        let parsed: Option<Value> = serde_json::from_str(text).ok();
        let success = parsed
            .as_ref()
            .and_then(|v| v.get("success"))
            .and_then(Value::as_bool);
        let errors: Vec<CloudflareMessage> = parsed
            .as_ref()
            .and_then(|v| v.get("errors"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|e| CloudflareMessage {
                        code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: self.scrub(
                            e.get("message").and_then(Value::as_str).unwrap_or(""),
                            &secrets,
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ok = (200..300).contains(&status);
        if ok && success == Some(true) {
            let mut parsed = parsed.unwrap_or_default();
            let info = parsed
                .get_mut("result_info")
                .map(Value::take)
                .and_then(|v| serde_json::from_value(v).ok());
            let result = parsed
                .get_mut("result")
                .map(Value::take)
                .unwrap_or_default();
            return Ok((result, info));
        }
        let has = |codes: &[i64]| errors.iter().any(|e| codes.contains(&e.code));
        let kind = if status == 401 || has(BAD_TOKEN_CODES) {
            ApiErrorKind::InvalidToken
        } else if status == 403 || has(NO_PERMISSION_CODES) {
            ApiErrorKind::MissingPermission(endpoint.spec().permission)
        } else if ok && success.is_none() {
            ApiErrorKind::Malformed
        } else {
            ApiErrorKind::Http
        };
        // Scrub the whole reply first: cutting first could keep the head of a secret that
        // straddles the cut.
        let detail = if parsed.is_some() {
            String::new()
        } else {
            let clean = self.scrub(text, &secrets);
            let excerpt: String = clean.chars().take(EXCERPT_CHARS).collect();
            format!("reply was not JSON: {excerpt:?}")
        };
        Err(ApiError {
            kind,
            endpoint: Some(endpoint),
            status: Some(status),
            errors,
            detail,
            tries,
        })
    }
}

impl Call {
    fn secret_refs(&self) -> Vec<&ApiToken> {
        self.upload_jwt.iter().chain(self.secrets.iter()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_override_accepts_only_loopback_http() {
        for ok in [
            "http://127.0.0.1:8787",
            "http://127.0.0.1:8787/",
            "http://127.0.0.1:1/client/v4",
            "http://[::1]:9000",
            "http://[::1]",
        ] {
            assert!(base_override_ok(ok), "{ok}");
        }
        for bad in [
            "https://api.cloudflare.com/client/v4",
            "http://127.0.0.1.evil.example:80",
            "http://127.0.0.1:80@evil.example",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://localhost:8787",
            "http://127.0.0.1:80/x?y=1",
            "http://127.0.0.1:80#frag",
            "ftp://127.0.0.1:21",
            "",
        ] {
            assert!(!base_override_ok(bad), "{bad}");
            let err = Client::new(ApiToken::new("t").unwrap(), Some(bad)).unwrap_err();
            assert_eq!(err.kind, ApiErrorKind::BaseRefused, "{bad}");
            assert!(err.message(&Text::new("en")).contains(BASE_ENV), "{bad}");
        }
    }

    #[test]
    fn path_values_cannot_escape_their_segment() {
        let mut c = Client::new(
            ApiToken::new("tok_abc").unwrap(),
            Some("http://127.0.0.1:9"),
        )
        .unwrap();
        c.set_account("acc1").unwrap();
        let url = c
            .url(
                &Call::new(Endpoint::D1Query).param("database_id", "db-1"),
                &[],
            )
            .unwrap();
        assert_eq!(
            url,
            "http://127.0.0.1:9/accounts/acc1/d1/database/db-1/query"
        );
        for bad in ["..", "a/b", "a?b", "", "a b", "a%2F"] {
            let call = Call::new(Endpoint::D1Query).param("database_id", bad);
            assert_eq!(
                c.url(&call, &[]).unwrap_err().kind,
                ApiErrorKind::Usage,
                "{bad}"
            );
        }
        // Missing, and unused, path values are refused too.
        assert!(c.url(&Call::new(Endpoint::D1Query), &[]).is_err());
        assert!(
            c.url(&Call::new(Endpoint::D1List).param("database_id", "x"), &[])
                .is_err()
        );
        let q = c
            .url(
                &Call::new(Endpoint::ZonesList).query("name", "a b&c=d"),
                &[],
            )
            .unwrap();
        assert!(q.ends_with("/zones?name=a%20b%26c%3Dd"), "{q}");
        assert!(c.set_account("../x").is_err());
        // An account id holding the token is refused without echoing it.
        let err = c.set_account("tok_abc/").unwrap_err();
        assert!(!err.detail.contains("tok_abc"), "{}", err.detail);
    }

    #[test]
    fn debug_never_shows_bodies_or_secrets() {
        let secret = "s3cr3t-webhook-key-value";
        let call = Call::new(Endpoint::SecretPut)
            .param("script_name", "blog")
            .query("q", secret)
            .json(json!({"name": "WEBHOOK_KEY", "text": secret, "type": "secret_text"}))
            .secret(ApiToken::new(secret).unwrap());
        let shown = format!("{call:?}");
        assert!(!shown.contains(secret), "{shown}");
        assert!(
            shown.contains("SecretPut") && shown.contains("bytes"),
            "{shown}"
        );
        let raw = Call::new(Endpoint::VersionUpload).raw("multipart/form-data", secret.into());
        assert!(!format!("{raw:?}").contains(secret));
    }
}
