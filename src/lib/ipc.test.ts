import { describe, it, expect, vi } from "vitest";

// Mocking `ipc.gen.ts` lets us assert the façade's shape. TS catches a missing
// method at compile time, but rename drift in the gen file breaks at runtime.

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
      "resetAccessibility",
      "getKeepAwake",
      "toggleKeepAwake",
      "setKeepAwakeDuration",
      "setKeepAwakeRestore",
      "getDiagnostics",
      "runSelfTest",
      "selfTestType",
      "openDiagnostics",
      "getSettings",
      "setRestoreArmed",
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
      "updaterAnnounced",
      "updaterDismiss",
      "captureForegroundApp",
      "expandPromptText",
      "importPrompt",
      "exportPrompt",
      "openExternal",
    ];
    for (const key of expected) {
      expect(typeof (ipc as Record<string, unknown>)[key]).toBe("function");
    }
    // Both directions: a new command added to the façade without being listed
    // here is drift too, and the one-way check let `getKeepAwake` go unnoticed.
    expect(Object.keys(ipc).sort()).toEqual([...expected].sort());
  });
});
