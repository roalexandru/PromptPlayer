import { describe, it, expect, vi } from "vitest";

// `ipc.ts` is a thin façade over the auto-generated tauri-specta bindings
// in `ipc.gen.ts`. Mocking the generated file lets us assert that the
// façade preserves the expected wrapper shape (a regression guard for the
// API surface — if a method goes missing the caller breaks at compile time
// in TS, but rename drift in the gen file would break at runtime).

describe("ipc façade", () => {
  it("exposes the expected method surface", async () => {
    vi.resetModules();

    // Build a stub `commands` object whose methods all resolve to a
    // tauri-specta-shaped Result. This is enough for the façade to evaluate.
    const ok = (v: unknown = null) =>
      Promise.resolve({ status: "ok" as const, data: v });
    const stub = new Proxy(
      {},
      {
        get: () => () => ok(),
      },
    );

    vi.doMock("./ipc.gen", () => ({
      commands: stub,
      // Type re-exports — no runtime values needed beyond `commands`.
    }));

    const { ipc } = await import("./ipc");
    const expected = [
      "getArmed",
      "toggleArmed",
      "kill",
      "isPlaying",
      "isHookAlive",
      "openAccessibilitySettings",
      "listPrompts",
      "libraryRoot",
      "savePrompt",
      "createPrompt",
      "deletePrompt",
      "setPromptEnabled",
      "setPromptPinned",
      "pickerOpen",
      "pickerSearch",
      "pickerSelect",
      "pickerDismiss",
      "trayOpen",
      "trayQuit",
      "trayPopupHide",
      "trayFirePrompt",
      "updaterCurrentVersion",
      "updaterCheck",
      "updaterInstall",
      "captureForegroundApp",
      "expandPromptText",
      "importPrompt",
      "exportPrompt",
      "openExternal",
    ];
    for (const key of expected) {
      expect(typeof (ipc as Record<string, unknown>)[key]).toBe("function");
    }
  });
});
