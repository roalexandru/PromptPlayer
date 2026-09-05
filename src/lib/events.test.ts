import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Tauri creates every configured window at startup, `visible: false` included,
// so a bare `setInterval` in a webview's `onMount` runs from launch to exit for
// a window nobody opened. These tests pin that `pollWhileVisible` only runs
// while the window is on screen.

type Handler = (e: unknown) => void;

function setup(initiallyVisible: boolean) {
  const handlers = new Map<string, Handler[]>();
  const unlistened: string[] = [];

  vi.doMock("@tauri-apps/api/event", () => ({
    listen: (event: string, handler: Handler) => {
      handlers.set(event, [...(handlers.get(event) ?? []), handler]);
      return Promise.resolve(() => unlistened.push(event));
    },
  }));
  vi.doMock("@tauri-apps/api/window", () => ({
    getCurrentWindow: () => ({
      isVisible: () => Promise.resolve(initiallyVisible),
    }),
  }));

  const emit = (event: string) => {
    for (const h of handlers.get(event) ?? []) h(null);
  };
  return { emit, unlistened };
}

describe("pollWhileVisible", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.doUnmock("@tauri-apps/api/event");
    vi.doUnmock("@tauri-apps/api/window");
  });

  it("does not tick for a window that was never shown", async () => {
    const { emit } = setup(false);
    const { pollWhileVisible } = await import("./events");
    const tick = vi.fn();

    await pollWhileVisible(tick, 2000);
    vi.advanceTimersByTime(60_000);

    expect(tick).not.toHaveBeenCalled();
    void emit;
  });

  it("starts on show and stops on hide", async () => {
    const { emit } = setup(false);
    const { pollWhileVisible, WINDOW_SHOWN, WINDOW_HIDDEN } = await import("./events");
    const tick = vi.fn();

    await pollWhileVisible(tick, 2000);
    expect(tick).toHaveBeenCalledTimes(0);

    // A show ticks once immediately so the first frame is current.
    emit(WINDOW_SHOWN);
    expect(tick).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(4000);
    expect(tick).toHaveBeenCalledTimes(3);

    emit(WINDOW_HIDDEN);
    vi.advanceTimersByTime(60_000);
    expect(tick).toHaveBeenCalledTimes(3);
  });

  it("polls straight away when the window is already up", async () => {
    setup(true);
    const { pollWhileVisible } = await import("./events");
    const tick = vi.fn();

    await pollWhileVisible(tick, 1000);
    expect(tick).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(2000);
    expect(tick).toHaveBeenCalledTimes(3);
  });

  it("a second show does not stack a second interval", async () => {
    const { emit } = setup(false);
    const { pollWhileVisible, WINDOW_SHOWN } = await import("./events");
    const tick = vi.fn();

    await pollWhileVisible(tick, 1000);
    emit(WINDOW_SHOWN);
    emit(WINDOW_SHOWN);
    tick.mockClear();

    vi.advanceTimersByTime(1000);
    expect(tick).toHaveBeenCalledTimes(1);
  });

  it("teardown stops the timer and releases the listeners", async () => {
    const { emit, unlistened } = setup(true);
    const { pollWhileVisible, WINDOW_SHOWN, WINDOW_HIDDEN } = await import("./events");
    const tick = vi.fn();

    const stop = await pollWhileVisible(tick, 1000);
    stop();
    tick.mockClear();

    vi.advanceTimersByTime(10_000);
    expect(tick).not.toHaveBeenCalled();
    expect(unlistened).toEqual([WINDOW_SHOWN, WINDOW_HIDDEN]);
    void emit;
  });
});
