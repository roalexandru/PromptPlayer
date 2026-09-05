import { describe, it, expect, vi, beforeEach } from "vitest";

// The component's reserved-shortcut map is platform-conditional, so we test
// the `platform.ts` helpers it derives from rather than mounting it.

describe("modifier rendering per platform", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("Mac maps super/meta/win to ⌘", async () => {
    vi.doMock("@tauri-apps/plugin-os", () => ({ platform: () => "macos" }));
    const { prettyMod } = await import("../platform");
    for (const token of ["cmd", "command", "meta", "super", "win"]) {
      expect(prettyMod(token)).toBe("⌘");
    }
  });

  it("Windows distinguishes Ctrl from Win", async () => {
    vi.doMock("@tauri-apps/plugin-os", () => ({ platform: () => "windows" }));
    const { prettyMod } = await import("../platform");
    expect(prettyMod("ctrl")).toBe("Ctrl");
    expect(prettyMod("win")).toBe("Win");
    expect(prettyMod("super")).toBe("Win");
    expect(prettyMod("windows")).toBe("Win");
  });
});
