// Pure helpers for the Connections form: status wording and the Authentication fieldset.
import type { McpAuth, McpAuthInput, McpServerView } from "$lib/ipc/commands";

export type AuthKind = McpAuth["kind"];

export type AuthForm = {
  kind: AuthKind;
  client_id: string;
  /** Blank keeps a saved secret (while the client ID is unchanged). */
  client_secret: string;
  /** Space-separated. */
  scopes: string;
  /** Blank = a free port per sign-in. */
  redirect_port: string;
  /** A secret is saved in the keychain. */
  has_client_secret: boolean;
};

export function blankAuth(): AuthForm {
  return { kind: "none", client_id: "", client_secret: "", scopes: "", redirect_port: "", has_client_secret: false };
}

export function authForm(auth: McpAuth): AuthForm {
  switch (auth.kind) {
    case "none":
      return blankAuth();
    case "oauth_user":
      return {
        kind: auth.kind,
        client_id: auth.client_id ?? "",
        client_secret: "",
        scopes: auth.scopes.join(" "),
        redirect_port: auth.redirect_port == null ? "" : String(auth.redirect_port),
        has_client_secret: auth.has_client_secret,
      };
    case "oauth_client_credentials":
      return {
        kind: auth.kind,
        client_id: auth.client_id,
        client_secret: "",
        scopes: auth.scopes.join(" "),
        redirect_port: "",
        has_client_secret: auth.has_client_secret,
      };
  }
}

/** Blank means "any free port" (`null`); anything outside 1024–65535 is `undefined`. */
function parsePort(raw: string): number | null | undefined {
  const t = raw.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isInteger(n) && n >= 1024 && n <= 65535 ? n : undefined;
}

export function authInput(f: AuthForm): McpAuthInput {
  switch (f.kind) {
    case "none":
      return { kind: "none" };
    case "oauth_user": {
      const redirect_port = parsePort(f.redirect_port);
      if (redirect_port === undefined) throw new Error("callback port must be 1024–65535");
      return { kind: "oauth_user", client_id: f.client_id, client_secret: f.client_secret, scopes: f.scopes, redirect_port };
    }
    case "oauth_client_credentials":
      return { kind: "oauth_client_credentials", client_id: f.client_id, client_secret: f.client_secret, scopes: f.scopes };
  }
}

/** The redirect URI to register with an authorization server for browser sign-in. */
export function redirectUri(port: string): string {
  const p = parsePort(port);
  return `http://127.0.0.1:${p === null ? "<any port>" : (p ?? "<invalid port>")}/callback`;
}

/** Short connection status for the server list and status bar. */
export function statusText(s: Pick<McpServerView, "enabled" | "status">): string {
  if (!s.enabled) return "disabled";
  switch (s.status.kind) {
    case "connected":
      return `${s.status.tool_count} tools`;
    case "connecting":
      return "connecting…";
    case "failed":
      return "failed";
    case "needs_auth":
      return "sign-in required";
    case "authorizing":
      return "waiting for browser…";
    default:
      return "idle";
  }
}
