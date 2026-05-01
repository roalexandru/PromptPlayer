import { describe, it, expect, vi, beforeEach } from "vitest";

// HotkeyRecorder.svelte's reserved-shortcut map and modifier rendering are
// platform-conditional. We test the underlying platform.ts helpers as a
// proxy — rendering the full Svelte component would require @testing-library
// /svelte at runtime which is fine but more setup. Coverage here focuses on
// the platform-specific token mapping.

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
