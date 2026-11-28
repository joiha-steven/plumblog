//! `/api/v1` site configuration: redirects, settings, theme and plugin options, plugin state.
//! Owner work (`settings.manage`, `theme.manage`, `plugin.manage`); reads are open to any
//! signed-in caller, as everything here but internal settings shows on reader pages anyway,
//! except plugin options, which may hold a plugin's secrets. The origin is session only.
//! Values are JSON, stored as re-serialised from the strict parse.

use plumblog_model::{Error, ErrorCode, Permission, Result, api};
use plumblog_store::{
    LAST_BACKUP_KEY, ORIGIN_KEY, OptionOwner, PluginState, RedirectStore, SITE_ID_KEY,
    SettingsStore, is_internal_setting, public_settings,
};

use super::{Input, Principal, changed, stored_json};
use crate::api::body::{Out, json_string, object};
use crate::api::{Ctx, Reply, site};

/// Longest redirect path; real URLs are far shorter.
const MAX_PATH: usize = 1000;

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

/// A setting or option key, a theme or a plugin name: 1-64 of `a-z`, `0-9`, `_`, `-`,
/// starting with a letter (plugin folder names follow the same rule).
fn name(field: &str, v: &str) -> Result<()> {
    let ok = v.len() <= 64
        && v.starts_with(|c: char| c.is_ascii_lowercase())
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(invalid(field, "a-z, 0-9, _ or -, starting with a letter"))
    }
}

/// A site path: starts with one `/`, no backslash, whitespace or control characters, so it
/// can never become a link to another host (`//host`, `/\host`).
fn site_path(field: &str, v: &str) -> Result<()> {
    let ok = v.starts_with('/')
        && !v.starts_with("//")
        && v.len() <= MAX_PATH
        && !v
            .chars()
            .any(|c| c == '\\' || c.is_whitespace() || c.is_control());
    if ok {
        Ok(())
    } else {
        Err(invalid(field, "a path on this site"))
    }
}

/// `{"<key>":<value>,...}` under `wrap`.
fn values_json(ctx: &Ctx, wrap: &str, rows: &[(String, String)]) -> String {
    let fields: Vec<(&str, Out)> = rows
        .iter()
        .map(|(k, v)| (k.as_str(), stored_json(ctx, k, v)))
        .collect();
    object(&[(wrap, Out::Raw(object(&fields)))])
}

pub async fn redirects(ctx: &Ctx) -> Result<Reply> {
    let items: Vec<String> = ctx
        .store
        .list_redirects()
        .await?
        .iter()
        .map(|r| {
            object(&[
                ("from", Out::Str(r.from_path.clone())),
                ("to", Out::Str(r.to_path.clone())),
                ("created_at", Out::Int(r.created_at)),
            ])
        })
        .collect();
    Ok(Reply::json(
        200,
        object(&[("redirects", Out::Raw(format!("[{}]", items.join(","))))]),
    ))
}

/// `PUT /api/v1/redirects`: upsert; the store rewrites chains and drops loops.
pub async fn redirect_put(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::ManageSettings)?;
    let from = input.str("from").unwrap_or_default();
    let to = input.str("to").unwrap_or_default();
    site_path("from", from)?;
    site_path("to", to)?;
    if from == to {
        return Err(invalid("to", "same as from"));
    }
    ctx.store.put_redirect(from, to, ctx.now).await?;
    changed(ctx, p, "redirect.put", from).await;
    Ok(Reply::empty())
}

pub async fn redirect_delete(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::ManageSettings)?;
    let from = input.str("from").unwrap_or_default();
    site_path("from", from)?;
    ctx.store.delete_redirect(from).await?;
    changed(ctx, p, "redirect.delete", from).await;
    Ok(Reply::empty())
}

/// `GET /api/v1/settings`: every setting but the internal ones.
pub async fn list(ctx: &Ctx) -> Result<Reply> {
    let rows = public_settings(ctx.store.all_settings().await?);
    Ok(Reply::json(200, values_json(ctx, "settings", &rows)))
}

/// A key the API may write. Internal keys are refused like a bad name, so the answer says
/// nothing about them; the site id is read-only, as every password's browser salt holds it,
/// and so is the last backup record, which only `blog backup` may write.
fn writable_key(key: &str) -> Result<()> {
    name("key", key)?;
    if is_internal_setting(key) || key == SITE_ID_KEY || key == LAST_BACKUP_KEY {
        return Err(invalid("key", "not writable"));
    }
    Ok(())
}

/// Refuses an API token on a key in [`api::SESSION_ONLY_SETTINGS`], before its value is checked.
fn session_only(p: &Principal, key: &str) -> Result<()> {
    if api::SESSION_ONLY_SETTINGS.contains(&key) {
        p.session_user()?;
    }
    Ok(())
}

pub async fn put(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::ManageSettings)?;
    let key = input.str("key").unwrap_or_default();
    writable_key(key)?;
    session_only(p, key)?;
    let value = input.json("value").unwrap_or_default();
    if key == ORIGIN_KEY {
        // The same rule the sign-in code reads it with, so a stored origin always loads.
        let text = json_string(&value).ok_or_else(|| invalid("value", "a string"))?;
        site::parse(&text).map_err(|why| invalid("value", &why))?;
    }
    ctx.store.put_setting(key, &value, ctx.now).await?;
    changed(ctx, p, "setting.put", key).await;
    Ok(Reply::empty())
}

pub async fn delete(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::ManageSettings)?;
    let key = input.str("key").unwrap_or_default();
    writable_key(key)?;
    session_only(p, key)?;
    ctx.store.delete_setting(key).await?;
    changed(ctx, p, "setting.delete", key).await;
    Ok(Reply::empty())
}

fn manage(owner: OptionOwner) -> Permission {
    match owner {
        OptionOwner::Theme => Permission::ManageThemes,
        OptionOwner::Plugin => Permission::ManagePlugins,
    }
}

/// The theme or plugin a route names. Names are checked by shape only: options of a theme
/// or plugin that is not installed are kept and simply not read.
fn owner_name(input: &Input) -> Result<&str> {
    let n = input.str("name").unwrap_or_default();
    name("name", n)?;
    Ok(n)
}

/// Theme options are read by any signed-in caller; plugin options only like they are written,
/// as a plugin may keep secrets there.
pub async fn options(ctx: &Ctx, p: &Principal, owner: OptionOwner, input: &Input) -> Result<Reply> {
    match owner {
        OptionOwner::Theme => p.require_read()?,
        OptionOwner::Plugin => p.require(manage(owner))?,
    };
    let rows = ctx.store.options(owner, owner_name(input)?).await?;
    Ok(Reply::json(200, values_json(ctx, "options", &rows)))
}

pub async fn option_put(
    ctx: &Ctx,
    p: &Principal,
    owner: OptionOwner,
    input: &Input,
) -> Result<Reply> {
    p.require(manage(owner))?;
    let n = owner_name(input)?;
    let key = input.str("key").unwrap_or_default();
    name("key", key)?;
    let value = input.json("value").unwrap_or_default();
    ctx.store.put_option(owner, n, key, &value, ctx.now).await?;
    changed(
        ctx,
        p,
        "option.put",
        &format!("{}:{n}:{key}", owner.as_str()),
    )
    .await;
    Ok(Reply::empty())
}

/// Resets every option of one theme or plugin to its declared default.
pub async fn options_delete(
    ctx: &Ctx,
    p: &Principal,
    owner: OptionOwner,
    input: &Input,
) -> Result<Reply> {
    p.require(manage(owner))?;
    let n = owner_name(input)?;
    ctx.store.delete_options(owner, n).await?;
    changed(ctx, p, "option.reset", &format!("{}:{n}", owner.as_str())).await;
    Ok(Reply::empty())
}

/// Plugins with a stored state; a bundled plugin without one is disabled.
pub async fn plugins(ctx: &Ctx) -> Result<Reply> {
    let items: Vec<String> = ctx
        .store
        .list_plugins()
        .await?
        .iter()
        .map(|s| {
            object(&[
                ("name", Out::Str(s.id.clone())),
                ("enabled", Out::Bool(s.enabled)),
                (
                    "disabled_reason",
                    s.disabled_reason.clone().map_or(Out::Null, Out::Str),
                ),
                ("updated_at", Out::Int(s.updated_at)),
            ])
        })
        .collect();
    Ok(Reply::json(
        200,
        object(&[("plugins", Out::Raw(format!("[{}]", items.join(","))))]),
    ))
}

/// `PUT /api/v1/plugins/<name>`: enable or disable; clears an automatic disable's reason.
pub async fn plugin_put(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::ManagePlugins)?;
    let n = owner_name(input)?;
    let enabled = input.bool("enabled").unwrap_or(false);
    ctx.store
        .put_plugin(PluginState {
            id: n.to_owned(),
            enabled,
            disabled_reason: None,
            updated_at: ctx.now,
        })
        .await?;
    let action = if enabled {
        "plugin.enable"
    } else {
        "plugin.disable"
    };
    changed(ctx, p, action, n).await;
    Ok(Reply::empty())
}

#[cfg(test)]
mod tests {
    use super::{name, site_path, writable_key};

    #[test]
    fn rules() {
        for ok in ["title", "excerpt_words", "a-b", "x1"] {
            assert!(name("key", ok).is_ok(), "{ok}");
        }
        for bad in ["", "1a", "Title", "a.b", "a/b", "_a", &"a".repeat(65)] {
            assert!(name("key", bad).is_err(), "{bad}");
        }
        for ok in ["/", "/old", "/en/a-b?x=1", "/%20"] {
            assert!(site_path("from", ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "old",
            "//evil.test",
            "/\\evil.test",
            "/a b",
            "/a\n",
            "https://x.test",
        ] {
            assert!(site_path("from", bad).is_err(), "{bad}");
        }
        for key in plumblog_store::INTERNAL_SETTING_KEYS
            .iter()
            .chain(&[plumblog_store::SITE_ID_KEY, plumblog_store::LAST_BACKUP_KEY])
        {
            assert!(writable_key(key).is_err(), "{key}");
        }
        assert!(writable_key("origin").is_ok());
        // The model's list spells the store's keys by hand; keep them bound together.
        for key in [
            plumblog_store::ORIGIN_KEY,
            plumblog_store::TURNSTILE_SITE_KEY,
        ] {
            assert!(super::api::SESSION_ONLY_SETTINGS.contains(&key), "{key}");
        }
    }
}
