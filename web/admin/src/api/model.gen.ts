// Generated from crates/model by crates/model/tests/ts.rs. Do not edit;
// run `BLESS=1 cargo test -p plumblog-model --test ts` after changing the Rust types.

export const ERROR_CODES = [
  "post.not_found",
  "user.not_found",
  "auth.forbidden",
  "auth.unauthenticated",
  "input.invalid",
  "store.failed",
  "internal",
  "store.not_found",
  "store.conflict",
  "invite.invalid",
  "auth.invalid_credentials",
  "auth.locked",
  "auth.turnstile_required",
  "auth.passkey_invalid",
  "setup.invalid",
  "api.not_found",
  "settings.origin_missing",
  "auth.reauth_required",
  "api.body_too_large",
  "api.length_required",
  "webhook.key_invalid",
  "webhook.url_blocked",
  "webhook.secret_unreadable",
  "webhook.delivery_failed",
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];

export const ROLES = [
  "owner",
  "writer",
] as const;
export type Role = (typeof ROLES)[number];

export const PERMISSIONS = [
  "post.write",
  "post.publish",
  "post.edit_any",
  "post.delete_any",
  "media.upload",
  "token.create_own",
  "settings.manage",
  "theme.manage",
  "plugin.manage",
  "user.manage",
  "comment.moderate",
  "syslog.view",
] as const;
export type Permission = (typeof PERMISSIONS)[number];

/** Which roles hold each permission (`Permission::roles`). */
export const PERMISSION_ROLES: Readonly<Record<Permission, readonly Role[]>> = {
  "post.write": ["owner", "writer"],
  "post.publish": ["owner", "writer"],
  "post.edit_any": ["owner", "writer"],
  "post.delete_any": ["owner", "writer"],
  "media.upload": ["owner", "writer"],
  "token.create_own": ["owner", "writer"],
  "settings.manage": ["owner"],
  "theme.manage": ["owner"],
  "plugin.manage": ["owner"],
  "user.manage": ["owner"],
  "comment.moderate": ["owner"],
  "syslog.view": ["owner"],
};
