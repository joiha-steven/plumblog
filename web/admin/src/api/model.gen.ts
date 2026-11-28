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
  "input.too_complex",
  "post.published_in_future",
  "comments.unavailable",
  "comments.closed",
  "comments.name_invalid",
  "comments.body_invalid",
  "comments.too_many_links",
  "comments.form_invalid",
  "comments.check_failed",
  "comments.rate_limited",
  "theme.unavailable",
  "auth.reset_expired",
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
  "media.delete_any",
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
  "media.delete_any": ["owner"],
  "media.upload": ["owner", "writer"],
  "token.create_own": ["owner", "writer"],
  "settings.manage": ["owner"],
  "theme.manage": ["owner"],
  "plugin.manage": ["owner"],
  "user.manage": ["owner"],
  "comment.moderate": ["owner", "writer"],
  "syslog.view": ["owner"],
};

export const POST_STATUSES = [
  "draft",
  "scheduled",
  "published",
  "trashed",
] as const;
export type PostStatus = (typeof POST_STATUSES)[number];

export const POST_KINDS = [
  "post",
  "page",
] as const;
export type PostKind = (typeof POST_KINDS)[number];

export const COMMENT_STATUSES = [
  "pending",
  "approved",
  "spam",
  "trashed",
] as const;
export type CommentStatus = (typeof COMMENT_STATUSES)[number];

export const RENDER_WARNINGS = [
  "markdown.math_invalid",
] as const;
export type RenderWarning = (typeof RENDER_WARNINGS)[number];

export const OPTION_TYPES = [
  "short_text",
  "long_text",
  "number",
  "toggle",
  "choice",
  "color",
  "image",
  "list",
] as const;
export type OptionType = (typeof OPTION_TYPES)[number];

export const THEME_SOURCES = [
  "embedded",
  "pushed",
] as const;
export type ThemeSource = (typeof THEME_SOURCES)[number];

export const LOG_KINDS = [
  "auth.sign_in",
  "auth.sign_in_failed",
  "auth.sign_in_locked",
  "auth.global_cap",
  "auth.password_changed",
  "auth.owner_reset",
  "auth.owner_reset_redeemed",
  "auth.sign_out",
  "auth.passkey_added",
  "auth.passkey_removed",
  "auth.token_created",
  "auth.token_revoked",
  "user.created",
  "content.changed",
  "settings.changed",
  "theme.changed",
  "plugin.changed",
  "plugin.disabled",
  "system.deploy",
  "system.migration",
  "job.run",
  "webhook.delivery",
  "error",
  "comment.created",
  "comment.moderated",
] as const;
export type LogKind = (typeof LOG_KINDS)[number];

export const LOG_LEVELS = [
  "info",
  "warning",
  "error",
] as const;
export type LogLevel = (typeof LOG_LEVELS)[number];

/** Response bodies of `/api/v1`, one interface per shape. */
export interface PostSummary {
  id: number;
  kind: PostKind;
  translation_group: number;
  lang: string;
  slug: string;
  status: PostStatus;
  trashed_from: PostStatus | null;
  published_at: number | null;
  author_id: number;
  title: string;
  excerpt: string;
  word_count: number;
  reading_minutes: number;
  pinned: boolean;
  comments_enabled: boolean;
  cover_media_id: number | null;
  series_id: number | null;
  series_position: number | null;
  created_at: number;
  updated_at: number;
  version: number;
  tags: number[];
}
export interface Post extends PostSummary {
  markdown: string;
  html: string;
  media_ids: number[];
  warnings: RenderWarning[];
}
export interface PostSaved extends PostSummary {
  markdown: string;
  html: string;
  media_ids: number[];
  warnings: RenderWarning[];
}
export interface PostPage {
  posts: PostSummary[];
  next_cursor: string | null;
  counts: PostCounts | null;
}
export interface PostCounts {
  draft: number;
  scheduled: number;
  published: number;
  trashed: number;
}
export interface Comment {
  id: number;
  post_id: number;
  parent_id: number | null;
  author_name: string;
  body: string;
  body_html: string | null;
  status: CommentStatus;
  created_at: number;
}
export interface CommentPage {
  comments: Comment[];
  next_cursor: string | null;
}
export interface User {
  id: number;
  login: string;
  slug: string;
  display_name: string;
  bio: string;
  role: string;
  disabled_at: number | null;
  created_at: number;
  updated_at: number;
}
export interface UserPage {
  users: User[];
  next_cursor: string | null;
}
export interface LogField {
  key: string;
  value: string;
}
export interface LogEntry {
  id: number;
  at: number;
  request_id: string;
  kind: LogKind;
  level: LogLevel;
  actor_id: number | null;
  actor_name: string | null;
  error_code: ErrorCode | null;
  subject: string;
  fields: LogField[];
}
export interface LogPage {
  entries: LogEntry[];
  next_cursor: string | null;
}
export interface Conflict {
  error: string;
  field: string;
  version: number;
}
export interface ThemeList {
  themes: ThemeInfo[];
}
export interface ThemeInfo {
  name: string;
  version: string | null;
  active: boolean;
  source: ThemeSource;
  available: boolean;
}
export interface OptionDeclarations {
  name: string;
  version: string | null;
  lang: string;
  groups: OptionGroup[];
  options: OptionDeclaration[];
}
export interface OptionGroup {
  key: string;
  label: string;
  label_key: string;
  icon: string | null;
}
export interface OptionDeclaration {
  key: string;
  label: string;
  help: string | null;
  type: OptionType;
  label_key: string;
  help_key: string | null;
  group: string | null;
  icon: string | null;
  token: string | null;
  min: number | null;
  max: number | null;
  step: number | null;
  default: unknown;
  choices: OptionChoice[];
  max_len: number | null;
  max_items: number | null;
}
export interface OptionChoice {
  label: string;
  value: string;
  label_key: string;
}

/** Request shapes of the post routes. Optional means the client may omit it. */
export interface PostCreateInput {
  lang: string;
  slug: string;
  title: string;
  markdown: string;
  kind?: PostKind;
  status?: PostStatus;
  published_at?: number | null;
  excerpt?: string;
  pinned?: boolean;
  comments_enabled?: boolean;
  cover_media_id?: number | null;
  series_id?: number | null;
  series_position?: number | null;
  tags?: number[];
  translation_of?: number;
  client_id?: string;
}
export interface PostUpdateInput {
  expected_version: number;
  lang?: string;
  slug?: string;
  title?: string;
  markdown?: string;
  kind?: PostKind;
  status?: PostStatus;
  published_at?: number | null;
  excerpt?: string;
  pinned?: boolean;
  comments_enabled?: boolean;
  cover_media_id?: number | null;
  series_id?: number | null;
  series_position?: number | null;
  tags?: number[];
}
export interface PostListQuery {
  kind?: PostKind;
  status?: PostStatus;
  lang?: string;
  tag?: number;
  series?: number;
  author?: number;
  order?: string;
  cursor?: string;
  limit?: number;
}

/**
 * Each shape's props in the order the Worker writes them, inherited ones first.
 * Descriptor: `string`, `integer`, `number`, `boolean`, `json` (any value),
 * `code:<CONSTANT>` (a list above, e.g. `code:POST_STATUSES`) or `shape:<name>`; then
 * `[]` for a list; then `|null` for nullable (`integer[]|null`).
 */
export const SHAPES: Readonly<Record<string, Readonly<Record<string, string>>>> = {
  "post_summary": {
    "id": "integer",
    "kind": "code:POST_KINDS",
    "translation_group": "integer",
    "lang": "string",
    "slug": "string",
    "status": "code:POST_STATUSES",
    "trashed_from": "code:POST_STATUSES|null",
    "published_at": "integer|null",
    "author_id": "integer",
    "title": "string",
    "excerpt": "string",
    "word_count": "integer",
    "reading_minutes": "integer",
    "pinned": "boolean",
    "comments_enabled": "boolean",
    "cover_media_id": "integer|null",
    "series_id": "integer|null",
    "series_position": "integer|null",
    "created_at": "integer",
    "updated_at": "integer",
    "version": "integer",
    "tags": "integer[]",
  },
  "post": {
    "id": "integer",
    "kind": "code:POST_KINDS",
    "translation_group": "integer",
    "lang": "string",
    "slug": "string",
    "status": "code:POST_STATUSES",
    "trashed_from": "code:POST_STATUSES|null",
    "published_at": "integer|null",
    "author_id": "integer",
    "title": "string",
    "excerpt": "string",
    "word_count": "integer",
    "reading_minutes": "integer",
    "pinned": "boolean",
    "comments_enabled": "boolean",
    "cover_media_id": "integer|null",
    "series_id": "integer|null",
    "series_position": "integer|null",
    "created_at": "integer",
    "updated_at": "integer",
    "version": "integer",
    "tags": "integer[]",
    "markdown": "string",
    "html": "string",
    "media_ids": "integer[]",
    "warnings": "code:RENDER_WARNINGS[]",
  },
  "post_saved": {
    "id": "integer",
    "kind": "code:POST_KINDS",
    "translation_group": "integer",
    "lang": "string",
    "slug": "string",
    "status": "code:POST_STATUSES",
    "trashed_from": "code:POST_STATUSES|null",
    "published_at": "integer|null",
    "author_id": "integer",
    "title": "string",
    "excerpt": "string",
    "word_count": "integer",
    "reading_minutes": "integer",
    "pinned": "boolean",
    "comments_enabled": "boolean",
    "cover_media_id": "integer|null",
    "series_id": "integer|null",
    "series_position": "integer|null",
    "created_at": "integer",
    "updated_at": "integer",
    "version": "integer",
    "tags": "integer[]",
    "markdown": "string",
    "html": "string",
    "media_ids": "integer[]",
    "warnings": "code:RENDER_WARNINGS[]",
  },
  "post_page": {
    "posts": "shape:post_summary[]",
    "next_cursor": "string|null",
    "counts": "shape:post_counts|null",
  },
  "post_counts": {
    "draft": "integer",
    "scheduled": "integer",
    "published": "integer",
    "trashed": "integer",
  },
  "comment": {
    "id": "integer",
    "post_id": "integer",
    "parent_id": "integer|null",
    "author_name": "string",
    "body": "string",
    "body_html": "string|null",
    "status": "code:COMMENT_STATUSES",
    "created_at": "integer",
  },
  "comment_page": {
    "comments": "shape:comment[]",
    "next_cursor": "string|null",
  },
  "user": {
    "id": "integer",
    "login": "string",
    "slug": "string",
    "display_name": "string",
    "bio": "string",
    "role": "string",
    "disabled_at": "integer|null",
    "created_at": "integer",
    "updated_at": "integer",
  },
  "user_page": {
    "users": "shape:user[]",
    "next_cursor": "string|null",
  },
  "log_field": {
    "key": "string",
    "value": "string",
  },
  "log_entry": {
    "id": "integer",
    "at": "integer",
    "request_id": "string",
    "kind": "code:LOG_KINDS",
    "level": "code:LOG_LEVELS",
    "actor_id": "integer|null",
    "actor_name": "string|null",
    "error_code": "code:ERROR_CODES|null",
    "subject": "string",
    "fields": "shape:log_field[]",
  },
  "log_page": {
    "entries": "shape:log_entry[]",
    "next_cursor": "string|null",
  },
  "conflict": {
    "error": "string",
    "field": "string",
    "version": "integer",
  },
  "theme_list": {
    "themes": "shape:theme_info[]",
  },
  "theme_info": {
    "name": "string",
    "version": "string|null",
    "active": "boolean",
    "source": "code:THEME_SOURCES",
    "available": "boolean",
  },
  "option_declarations": {
    "name": "string",
    "version": "string|null",
    "lang": "string",
    "groups": "shape:option_group[]",
    "options": "shape:option_declaration[]",
  },
  "option_group": {
    "key": "string",
    "label": "string",
    "label_key": "string",
    "icon": "string|null",
  },
  "option_declaration": {
    "key": "string",
    "label": "string",
    "help": "string|null",
    "type": "code:OPTION_TYPES",
    "label_key": "string",
    "help_key": "string|null",
    "group": "string|null",
    "icon": "string|null",
    "token": "string|null",
    "min": "number|null",
    "max": "number|null",
    "step": "number|null",
    "default": "json",
    "choices": "shape:option_choice[]",
    "max_len": "integer|null",
    "max_items": "integer|null",
  },
  "option_choice": {
    "label": "string",
    "value": "string",
    "label_key": "string",
  },
};
