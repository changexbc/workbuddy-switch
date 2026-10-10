import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  isDefaultArchiveSyncEnabled,
  setDefaultArchiveSyncEnabled,
} from "./sync-prefs";

describe("sync-prefs (#165 方案 3)", () => {
  const store = new Map<string, string>();
  const originalLocalStorage = globalThis.localStorage;
  const originalWindow = globalThis.window;

  beforeEach(() => {
    store.clear();
    const fakeLocalStorage = {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, val: string) => {
        store.set(key, String(val));
      },
      removeItem: (key: string) => {
        store.delete(key);
      },
      clear: () => store.clear(),
      get length() {
        return store.size;
      },
      key: (index: number) => Array.from(store.keys())[index] ?? null,
    };
    Object.defineProperty(globalThis, "localStorage", {
      value: fakeLocalStorage,
      writable: true,
      configurable: true,
    });

    const fakeWindow = new EventTarget();
    Object.defineProperty(globalThis, "window", {
      value: fakeWindow,
      writable: true,
      configurable: true,
    });
  });

  afterEach(() => {
    store.clear();
    Object.defineProperty(globalThis, "localStorage", {
      value: originalLocalStorage,
      writable: true,
      configurable: true,
    });
    Object.defineProperty(globalThis, "window", {
      value: originalWindow,
      writable: true,
      configurable: true,
    });
  });

  it("默认返回 false", () => {
    expect(isDefaultArchiveSyncEnabled()).toBe(false);
  });

  it("设置为 true 后返回 true，localStorage 存为 '1'", () => {
    setDefaultArchiveSyncEnabled(true);
    expect(isDefaultArchiveSyncEnabled()).toBe(true);
    expect(globalThis.localStorage.getItem("wb-switch.sync.default-archive")).toBe("1");
  });

  it("设置为 false 后返回 false，localStorage 存为 '0'", () => {
    setDefaultArchiveSyncEnabled(true);
    setDefaultArchiveSyncEnabled(false);
    expect(isDefaultArchiveSyncEnabled()).toBe(false);
    expect(globalThis.localStorage.getItem("wb-switch.sync.default-archive")).toBe("0");
  });

  it("非法或非 '1' 的存储值一律视为 false", () => {
    globalThis.localStorage.setItem("wb-switch.sync.default-archive", "true");
    expect(isDefaultArchiveSyncEnabled()).toBe(false);

    globalThis.localStorage.setItem("wb-switch.sync.default-archive", "2");
    expect(isDefaultArchiveSyncEnabled()).toBe(false);

    globalThis.localStorage.setItem("wb-switch.sync.default-archive", "");
    expect(isDefaultArchiveSyncEnabled()).toBe(false);
  });

  it("调用 setDefaultArchiveSyncEnabled 会分发同页面通知事件", () => {
    const handler = vi.fn();
    globalThis.window.addEventListener("wb-switch:sync-prefs-changed", handler);
    try {
      setDefaultArchiveSyncEnabled(true);
      expect(handler).toHaveBeenCalledTimes(1);
    } finally {
      globalThis.window.removeEventListener("wb-switch:sync-prefs-changed", handler);
    }
  });

  it("localStorage 读取异常时兜底返回 false，不抛出崩溃", () => {
    Object.defineProperty(globalThis, "localStorage", {
      value: {
        getItem: () => {
          throw new Error("SecurityError: Access is denied");
        },
      },
      writable: true,
      configurable: true,
    });
    expect(isDefaultArchiveSyncEnabled()).toBe(false);
  });
});
