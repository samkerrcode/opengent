import { describe, expect, it } from "vitest";
import type { McpServerView, ToolInfo } from "$lib/ipc/commands";
import { allowedSummary, ownAccess, snapshotAccess, toolOn, toolPolicy, withTool } from "./permissions";

const tool = (name: string, policy: ToolInfo["policy"], access: ToolInfo["access"]): ToolInfo => ({
  name,
  exposed: `fs__${name}`,
  title: null,
  description: null,
  policy,
  access,
  changed: false,
});

const server = { id: "fs", tools: [
  tool("read", "always_allow", "read_only"),
  tool("write", "always_allow", "write"),
  tool("rm", "ask", "unannotated"),
  tool("nuke", "block", "destructive"),
] } as unknown as McpServerView;

describe("agent permission snapshot", () => {
  it("keeps Allow only for read-only tools and defaults new tools to Ask", () => {
    const a = snapshotAccess(server);
    expect(a.tools).toBeNull();
    expect(a.default_policy).toBe("ask");
    expect(a.policies).toEqual({ read: "always_allow", write: "ask", rm: "ask", nuke: "block" });
    expect(allowedSummary(a, server)).toEqual({ allowed: 1, risky: 0 });
  });

  it("converts a legacy allow-list into own permissions with unlisted tools off", () => {
    const legacy = { server_id: "fs", tools: ["read"], default_policy: null, policies: {} };
    expect(toolOn(legacy, "write")).toBe(false);
    const a = ownAccess(legacy, server);
    expect(a.tools).toBeNull();
    expect(a.default_policy).toBe("block");
    expect(toolPolicy(a, "read")).toBe("always_allow");
    expect(toolOn(a, "write")).toBe(false);
    expect(toolOn(a, "brand-new")).toBe(false);
  });

  it("turns tools on with their snapshot policy and counts risky allows", () => {
    const [, write, rm] = server.tools as [ToolInfo, ToolInfo, ToolInfo];
    let a = snapshotAccess(server);
    a = withTool(a, rm, "block");
    expect(toolOn(a, "rm")).toBe(false);
    a = withTool(a, rm, "on");
    expect(toolPolicy(a, "rm")).toBe("ask");
    a = withTool(a, write, "always_allow");
    expect(allowedSummary(a, server)).toEqual({ allowed: 2, risky: 1 });
  });
});
