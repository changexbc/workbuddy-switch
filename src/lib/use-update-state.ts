import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";

import * as api from "@/lib/api";
import type { UpdateSnapshot } from "@/lib/types";

/**
 * 统一更新服务状态（Rust 单一真相源）。
 *
 * 托盘与前端弹窗读同一份快照：阶段与下载进度都由 Rust 经 `update-state` 事件推送，
 * 前端不再自己调用 JS 版 updater，也不再轮询检查更新。
 *
 * 模块级单例订阅：多个组件共用一条 `listen`；首次订阅时用 `update_state` 拉一次
 * 当前快照（覆盖「事件在挂载前已发出」的情形），订阅全部释放后解绑监听，
 * 再次订阅会重新拉取，不会漏状态。
 */

const IDLE_SNAPSHOT: UpdateSnapshot = {
  phase: "idle",
  latest: null,
  percent: null,
  message: null,
  checkedAt: null,
};

let snapshot: UpdateSnapshot = IDLE_SNAPSHOT;
const listeners = new Set<() => void>();
let unlisten: (() => void) | undefined;
/** 订阅代际：`start` / `stop` 都自增；迟到的 Promise 回调据此判断自己是否已作废。 */
let generation = 0;

function publish(next: UpdateSnapshot) {
  snapshot = next;
  for (const listener of listeners) listener();
}

function start() {
  const current = ++generation;
  void api
    .updateState()
    .then((state) => {
      if (current === generation && state) publish(state);
    })
    .catch(() => {
      // 拉取失败保持上一次快照：事件推送仍会覆盖它。
    });
  if (api.isWebui()) return;
  void listen<UpdateSnapshot>("update-state", (event) => {
    if (event.payload) publish(event.payload);
  }).then((fn) => {
    // StrictMode 下「挂载 → 卸载 → 再挂载」时 Promise 可能在清理之后才 resolve：
    // 代际不符就立刻解绑，避免同一事件被两个监听器重复处理。
    if (current !== generation) {
      fn();
      return;
    }
    unlisten = fn;
  });
}

function stop() {
  generation += 1;
  unlisten?.();
  unlisten = undefined;
}

function subscribe(onStoreChange: () => void): () => void {
  listeners.add(onStoreChange);
  if (listeners.size === 1) start();
  return () => {
    listeners.delete(onStoreChange);
    if (listeners.size === 0) stop();
  };
}

function getSnapshot(): UpdateSnapshot {
  return snapshot;
}

/** 订阅当前更新状态快照（阶段 / 目标版本 / 下载进度 / 错误文案）。 */
export function useUpdateState(): UpdateSnapshot {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
