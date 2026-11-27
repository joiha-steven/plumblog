//! Every Cloudflare API call `blog` makes, with the token permission it needs and its doc page.
//!
//! The client only accepts an [`Endpoint`], never a free-form path, so a call cannot exist
//! without a row here, and `blog guide token` (generated from these rows) always asks for
//! exactly the permissions the code uses.
//! Permission labels: <https://developers.cloudflare.com/fundamentals/api/reference/permissions/>

use crate::text::Text;
use plumblog_i18n::{Key, key};
use std::fmt::Write;

/// HTTP method of an endpoint. Only GETs are ever retried automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
}

impl Method {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
        }
    }
}

/// Which row of the dashboard token form a permission sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Account,
    Zone,
}

impl Scope {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Account => "Account",
            Self::Zone => "Zone",
        }
    }
}

/// A token permission, named exactly as the dashboard's "Create Custom Token" form shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    D1Edit,
    WorkersScriptsEdit,
    WorkersR2StorageEdit,
    TurnstileEdit,
    /// Only with a custom domain: find the domain's zone.
    ZoneRead,
}

impl Permission {
    /// `(scope, permission group, access)` as the three dropdowns of the token form.
    #[must_use]
    pub const fn parts(self) -> (Scope, &'static str, &'static str) {
        match self {
            Self::D1Edit => (Scope::Account, "D1", "Edit"),
            Self::WorkersScriptsEdit => (Scope::Account, "Workers Scripts", "Edit"),
            Self::WorkersR2StorageEdit => (Scope::Account, "Workers R2 Storage", "Edit"),
            Self::TurnstileEdit => (Scope::Account, "Turnstile", "Edit"),
            Self::ZoneRead => (Scope::Zone, "Zone", "Read"),
        }
    }

    /// One line, e.g. `Account | D1 | Edit`.
    #[must_use]
    pub fn label(self) -> String {
        let (scope, group, access) = self.parts();
        format!("{} | {group} | {access}", scope.label())
    }
}

/// What the `Authorization: Bearer` header carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// The owner's API token.
    ApiToken,
    /// The short-lived JWT from an assets upload session; the API token must not be sent.
    UploadJwt,
}

/// One row of the table.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub method: Method,
    /// Path under the API base; `{name}` segments are filled by the client.
    pub path: &'static str,
    /// `None`: any valid token may call it.
    pub permission: Option<Permission>,
    pub auth: Auth,
    /// Short purpose ("to <summary>"), used in error messages and the token guide.
    pub summary: Key,
    pub doc: &'static str,
}

macro_rules! endpoints {
    ($($name:ident => $method:ident $path:literal, $perm:expr, $auth:ident, $summary:expr, $doc:literal;)+) => {
        /// Every Cloudflare API call `blog` may make.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Endpoint { $($name),+ }

        impl Endpoint {
            /// Every endpoint, in table order.
            pub const ALL: &[Endpoint] = &[$(Endpoint::$name),+];

            #[must_use]
            pub const fn spec(self) -> Spec {
                match self {
                    $(Endpoint::$name => Spec {
                        method: Method::$method,
                        path: $path,
                        permission: $perm,
                        auth: Auth::$auth,
                        summary: $summary,
                        doc: $doc,
                    }),+
                }
            }
        }
    };
}

use Permission::{D1Edit, TurnstileEdit, WorkersR2StorageEdit, WorkersScriptsEdit, ZoneRead};

endpoints! {
    // Fails with code 1000 for account-owned tokens; those verify under the account instead.
    UserTokenVerify => Get "/user/tokens/verify", None, ApiToken, key!("cli.call.user_token_verify"),
        "https://developers.cloudflare.com/api/resources/user/subresources/tokens/methods/verify/";
    AccountTokenVerify => Get "/accounts/{account_id}/tokens/verify", None, ApiToken,
        key!("cli.call.account_token_verify"),
        "https://developers.cloudflare.com/api/resources/accounts/subresources/tokens/methods/verify/";
    AccountsList => Get "/accounts", None, ApiToken, key!("cli.call.accounts_list"),
        "https://developers.cloudflare.com/api/resources/accounts/methods/list/";

    D1List => Get "/accounts/{account_id}/d1/database", Some(D1Edit), ApiToken,
        key!("cli.call.d1_list"),
        "https://developers.cloudflare.com/api/resources/d1/subresources/database/methods/list/";
    D1Create => Post "/accounts/{account_id}/d1/database", Some(D1Edit), ApiToken,
        key!("cli.call.d1_create"),
        "https://developers.cloudflare.com/api/resources/d1/subresources/database/methods/create/";
    D1Query => Post "/accounts/{account_id}/d1/database/{database_id}/query", Some(D1Edit),
        ApiToken, key!("cli.call.d1_query"),
        "https://developers.cloudflare.com/api/resources/d1/subresources/database/methods/query/";
    D1BookmarkGet => Get "/accounts/{account_id}/d1/database/{database_id}/time_travel/bookmark",
        Some(D1Edit), ApiToken, key!("cli.call.d1_bookmark"),
        "https://developers.cloudflare.com/api/resources/d1/subresources/database/subresources/time_travel/methods/get_bookmark/";
    D1Restore => Post "/accounts/{account_id}/d1/database/{database_id}/time_travel/restore",
        Some(D1Edit), ApiToken, key!("cli.call.d1_restore"),
        "https://developers.cloudflare.com/api/resources/d1/subresources/database/subresources/time_travel/methods/restore/";
    D1Export => Post "/accounts/{account_id}/d1/database/{database_id}/export", Some(D1Edit),
        ApiToken, key!("cli.call.d1_export"),
        "https://developers.cloudflare.com/api/resources/d1/subresources/database/methods/export/";

    R2BucketList => Get "/accounts/{account_id}/r2/buckets", Some(WorkersR2StorageEdit), ApiToken,
        key!("cli.call.r2_list"),
        "https://developers.cloudflare.com/api/resources/r2/subresources/buckets/methods/list/";
    R2BucketCreate => Post "/accounts/{account_id}/r2/buckets", Some(WorkersR2StorageEdit),
        ApiToken, key!("cli.call.r2_create"),
        "https://developers.cloudflare.com/api/resources/r2/subresources/buckets/methods/create/";

    // The first upload of a new script; versions cannot be the first upload.
    ScriptUpload => Put "/accounts/{account_id}/workers/scripts/{script_name}",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.script_upload"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/methods/update/";
    VersionUpload => Post "/accounts/{account_id}/workers/scripts/{script_name}/versions",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.version_upload"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/versions/methods/create/";
    VersionList => Get "/accounts/{account_id}/workers/scripts/{script_name}/versions",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.version_list"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/versions/methods/list/";
    DeploymentCreate => Post "/accounts/{account_id}/workers/scripts/{script_name}/deployments",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.deployment_create"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/create/";
    DeploymentList => Get "/accounts/{account_id}/workers/scripts/{script_name}/deployments",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.deployment_list"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/list/";
    AssetsSession => Post "/accounts/{account_id}/workers/scripts/{script_name}/assets-upload-session",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.assets_session"),
        "https://developers.cloudflare.com/workers/static-assets/direct-upload/";
    // Authorized by the session's JWT, not the API token (same doc page).
    AssetsUpload => Post "/accounts/{account_id}/workers/assets/upload", None, UploadJwt,
        key!("cli.call.assets_upload"),
        "https://developers.cloudflare.com/workers/static-assets/direct-upload/";
    SecretPut => Put "/accounts/{account_id}/workers/scripts/{script_name}/secrets",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.secret_put"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/secrets/methods/update/";
    SecretList => Get "/accounts/{account_id}/workers/scripts/{script_name}/secrets",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.secret_list"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/secrets/methods/list/";
    SchedulesPut => Put "/accounts/{account_id}/workers/scripts/{script_name}/schedules",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.schedules_put"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/schedules/methods/update/";
    SubdomainGet => Get "/accounts/{account_id}/workers/subdomain", Some(WorkersScriptsEdit),
        ApiToken, key!("cli.call.subdomain_get"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/subdomains/methods/get/";
    ScriptSubdomainSet => Post "/accounts/{account_id}/workers/scripts/{script_name}/subdomain",
        Some(WorkersScriptsEdit), ApiToken, key!("cli.call.script_subdomain_set"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/subdomain/methods/create/";
    DomainList => Get "/accounts/{account_id}/workers/domains", Some(WorkersScriptsEdit),
        ApiToken, key!("cli.call.domain_list"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/domains/methods/list/";
    DomainAttach => Put "/accounts/{account_id}/workers/domains", Some(WorkersScriptsEdit),
        ApiToken, key!("cli.call.domain_attach"),
        "https://developers.cloudflare.com/api/resources/workers/subresources/domains/methods/update/";
    ZonesList => Get "/zones", Some(ZoneRead), ApiToken, key!("cli.call.zones_list"),
        "https://developers.cloudflare.com/api/resources/zones/methods/list/";

    TurnstileCreate => Post "/accounts/{account_id}/challenges/widgets", Some(TurnstileEdit),
        ApiToken, key!("cli.call.turnstile_create"),
        "https://developers.cloudflare.com/api/resources/turnstile/subresources/widgets/methods/create/";
    TurnstileList => Get "/accounts/{account_id}/challenges/widgets", Some(TurnstileEdit),
        ApiToken, key!("cli.call.turnstile_list"),
        "https://developers.cloudflare.com/api/resources/turnstile/subresources/widgets/methods/list/";
    TurnstileGet => Get "/accounts/{account_id}/challenges/widgets/{sitekey}", Some(TurnstileEdit),
        ApiToken, key!("cli.call.turnstile_get"),
        "https://developers.cloudflare.com/api/resources/turnstile/subresources/widgets/methods/get/";
}

/// Permissions the table uses, in dashboard order, each with what it is used for.
#[must_use]
pub fn permissions() -> Vec<(Permission, Vec<Key>)> {
    let mut out: Vec<(Permission, Vec<Key>)> = Vec::new();
    for e in Endpoint::ALL {
        let spec = e.spec();
        let Some(p) = spec.permission else { continue };
        match out.iter_mut().find(|(q, _)| *q == p) {
            Some((_, uses)) => uses.push(spec.summary),
            None => out.push((p, vec![spec.summary])),
        }
    }
    out.sort_by_key(|(p, _)| *p);
    out
}

/// Where the owner creates and edits tokens.
pub const TOKENS_PAGE: &str = "https://dash.cloudflare.com/profile/api-tokens";

/// `blog guide token`: how to create the API token, generated from the table. Sentences come
/// from locale keys; permission names stay exactly as the dashboard shows them (they are data).
#[must_use]
pub fn token_guide(text: &Text) -> String {
    let t = |k: Key| text.t(k, &[]);
    let perms = permissions();
    let mut out = String::new();
    for line in [
        t(key!("cli.guide_token.title")),
        t(key!("cli.guide_token.intro")),
        t(key!("cli.guide_token.global_key")),
    ] {
        writeln!(out, "{line}\n").unwrap();
    }
    writeln!(
        out,
        "{}",
        text.t(
            key!("cli.guide_token.step_open"),
            &[("page", TOKENS_PAGE.to_string())]
        )
    )
    .unwrap();
    writeln!(out, "{}", t(key!("cli.guide_token.step_permissions"))).unwrap();
    for (scope, heading) in [
        (Scope::Account, t(key!("cli.guide_token.account_heading"))),
        (Scope::Zone, t(key!("cli.guide_token.zone_heading"))),
    ] {
        let rows: Vec<_> = perms.iter().filter(|(p, _)| p.parts().0 == scope).collect();
        if rows.is_empty() {
            continue;
        }
        write!(
            out,
            "\n{heading}\n\n{}\n|---|---|---|---|\n",
            t(key!("cli.guide_token.table_head"))
        )
        .unwrap();
        for (p, uses) in rows {
            let (_, group, access) = p.parts();
            let uses: Vec<String> = uses.iter().map(|k| t(*k)).collect();
            writeln!(
                out,
                "| {} | {group} | {access} | {} |",
                scope.label(),
                uses.join("; ")
            )
            .unwrap();
        }
    }
    writeln!(
        out,
        "\n{}\n{}\n{}\n",
        t(key!("cli.guide_token.step_account_resources")),
        t(key!("cli.guide_token.step_zone_resources")),
        t(key!("cli.guide_token.step_env"))
    )
    .unwrap();
    writeln!(
        out,
        "```sh\nCLOUDFLARE_API_TOKEN={}\nCLOUDFLARE_ACCOUNT_ID={}\n```\n",
        t(key!("cli.guide_token.env_token")),
        t(key!("cli.guide_token.env_account"))
    )
    .unwrap();
    writeln!(
        out,
        "{}\n\n```sh\nblog setup --check-token\n```\n\n{}",
        t(key!("cli.guide_token.step_check")),
        t(key!("cli.guide_token.never_share"))
    )
    .unwrap();
    out
}
