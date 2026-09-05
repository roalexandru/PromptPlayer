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
      // Companion surface: config, setlist, transport, sources, imports.
      "getConfig",
      "saveConfig",
      "getSetlist",
      "setSetlist",
      "fireNextCue",
      "resetSetlist",
      "playbackStatus",
      "togglePlaybackPause",
      "nudgePlaybackSpeed",
      "listSources",
      "addSource",
      "removeSource",
      "refreshSources",
      "setRemotePromptEnabled",
      "forkPrompt",
      "promptStops",
      "importAgentPrompts",
      "agentImportCandidates",
      "captureLastTyped",
      "sourcePendingChanges",
      "applySourceUpdates",
    ];
    for (const key of expected) {
      expect(typeof (ipc as Record<string, unknown>)[key]).toBe("function");
    }
    // Both directions: a new command added to the façade without being listed
    // here is drift too, and the one-way check let `getKeepAwake` go unnoticed.
    expect(Object.keys(ipc).sort()).toEqual([...expected].sort());
  });
});

describe("prompt origin helpers", () => {
  it("identifies remote prompts and their source", async () => {
    vi.resetModules();
    vi.doMock("./ipc.gen", () => ({ commands: {} }));
    const { isRemote, sourceIdOf } = await import("./ipc");

    const remote = { origin: { kind: "remote", source_id: "org-repo@main" } };
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(isRemote(remote as any)).toBe(true);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(sourceIdOf(remote as any)).toBe("org-repo@main");
  });

  it("treats a local prompt, and a payload with no origin at all, as local", async () => {
    vi.resetModules();
    vi.doMock("./ipc.gen", () => ({ commands: {} }));
    const { isRemote, sourceIdOf } = await import("./ipc");

    // `origin` is `#[serde(default)]` in Rust, so an older payload can omit
    // it; absent must mean local rather than throwing.
    for (const p of [{ origin: { kind: "local" } }, {}]) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      expect(isRemote(p as any)).toBe(false);
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      expect(sourceIdOf(p as any)).toBeNull();
    }
  });

  it("routes enable/disable to the command that matches the origin", async () => {
    vi.resetModules();
    const calls: string[] = [];
    const ok = () => Promise.resolve({ status: "ok" as const, data: null });
    vi.doMock("./ipc.gen", () => ({
      commands: {
        setPromptEnabled: (id: string, on: boolean) => {
          calls.push(`local:${id}:${on}`);
          return ok();
        },
        setRemotePromptEnabled: (id: string, on: boolean) => {
          calls.push(`remote:${id}:${on}`);
          return ok();
        },
      },
    }));
    const { setEnabled } = await import("./ipc");

    // A remote prompt's enablement lives in promptplayer.yaml, because its
    // cache directory is replaced wholesale on every refresh.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await setEnabled({ id: "src/p", origin: { kind: "remote", source_id: "src" } } as any, true);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await setEnabled({ id: "mine", origin: { kind: "local" } } as any, false);
    expect(calls).toEqual(["remote:src/p:true", "local:mine:false"]);
  });
});
