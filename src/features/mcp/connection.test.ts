import { describe, expect, it } from "vitest";
import { authForm, authInput, blankAuth, redirectUri, statusText } from "./connection";

describe("statusText", () => {
  it("words the sign-in states", () => {
    expect(statusText({ enabled: true, status: { kind: "needs_auth", reason: null } })).toBe("sign-in required");
    expect(statusText({ enabled: true, status: { kind: "authorizing" } })).toBe("waiting for browser…");
    expect(statusText({ enabled: true, status: { kind: "connected", tool_count: 3 } })).toBe("3 tools");
    expect(statusText({ enabled: false, status: { kind: "needs_auth", reason: null } })).toBe("disabled");
  });
});

describe("auth form", () => {
  it("round-trips a browser sign-in config without its secret", () => {
    const f = authForm({ kind: "oauth_user", client_id: null, has_client_secret: false, scopes: ["a", "b"], redirect_port: 8123 });
    expect(f).toMatchObject({ kind: "oauth_user", client_id: "", scopes: "a b", redirect_port: "8123", client_secret: "" });
    expect(authInput(f)).toEqual({ kind: "oauth_user", client_id: "", client_secret: "", scopes: "a b", redirect_port: 8123 });
  });

  it("maps client credentials and none", () => {
    const f = authForm({ kind: "oauth_client_credentials", client_id: "svc", has_client_secret: true, scopes: [] });
    expect(f.has_client_secret).toBe(true);
    expect(authInput({ ...f, client_secret: "s" })).toEqual({ kind: "oauth_client_credentials", client_id: "svc", client_secret: "s", scopes: "" });
    expect(authInput(blankAuth())).toEqual({ kind: "none" });
  });

  it("treats a blank port as any port and rejects an invalid one", () => {
    expect(authInput({ ...blankAuth(), kind: "oauth_user", redirect_port: " " })).toMatchObject({ redirect_port: null });
    for (const bad of ["70000", "80", "abc"]) {
      expect(() => authInput({ ...blankAuth(), kind: "oauth_user", redirect_port: bad })).toThrow("callback port");
    }
    expect(redirectUri("")).toBe("http://127.0.0.1:<any port>/callback");
    expect(redirectUri("9000")).toBe("http://127.0.0.1:9000/callback");
    expect(redirectUri("abc")).toBe("http://127.0.0.1:<invalid port>/callback");
  });
});
