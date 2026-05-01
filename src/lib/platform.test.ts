import { describe, it, expect, vi, beforeEach } from "vitest";

// The platform module reads `platform()` once at import time, so each test
// re-imports a fresh copy with a different mock.

describe("platform.ts", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("identifies macOS", async () => {
    vi.doMock("@tauri-apps/plugin-os", () => ({
      platform: () => "macos",
    }));
    const m = await import("./platform");
    expect(m.IS_MAC).toBe(true);
    expect(m.IS_WIN).toBe(false);
    expect(m.PRIMARY_MOD).toBe("cmd");
  });

  it("identifies Windows", async () => {
    vi.doMock("@tauri-apps/plugin-os", () => ({
      platform: () => "windows",
    }));
    const m = await import("./platform");
    expect(m.IS_MAC).toBe(false);
    expect(m.IS_WIN).toBe(true);
    expect(m.PRIMARY_MOD).toBe("ctrl");
  });

  it("falls back to macOS outside Tauri", async () => {
    vi.doMock("@tauri-apps/plugin-os", () => ({
      platform: () => {
        throw new Error("not in Tauri");
      },
    }));
    const m = await import("./platform");
    // Fallback prevents crashes in Vitest/jsdom.
    expect(m.IS_MAC).toBe(true);
  });
});

describe("prettyMod", () => {
  it("renders Mac symbols on Mac", async () => {
    vi.resetModules();
    vi.doMock("@tauri-apps/plugin-os", () => ({ platform: () => "macos" }));
    const { prettyMod } = await import("./platform");
    expect(prettyMod("cmd")).toBe("⌘");
    expect(prettyMod("ctrl")).toBe("⌃");
    expect(prettyMod("alt")).toBe("⌥");
    expect(prettyMod("shift")).toBe("⇧");
  });

  it("renders Windows text labels on Windows", async () => {
    vi.resetModules();
    vi.doMock("@tauri-apps/plugin-os", () => ({ platform: () => "windows" }));
    const { prettyMod } = await import("./platform");
    expect(prettyMod("ctrl")).toBe("Ctrl");
    expect(prettyMod("alt")).toBe("Alt");
    expect(prettyMod("shift")).toBe("Shift");
    expect(prettyMod("win")).toBe("Win");
    // On Windows, an authored "cmd" token (e.g., from a shared library file)
    // is interpreted as Ctrl — matches Tauri's CmdOrCtrl semantics.
    expect(prettyMod("cmd")).toBe("Ctrl");
  });
});
