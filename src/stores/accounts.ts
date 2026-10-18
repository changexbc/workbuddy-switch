import { create } from "zustand";
import * as api from "@/lib/api";
import { newerCreditResult } from "@/lib/newer-credit-result";
import { DEFAULT_VARIANT, normalizeVariant } from "@/lib/variant";
import type {
  AccountMeta,
  AppStatus,
  CodeBuddyCliStatus,
  CodeBuddyCnIdeStatus,
  CreditExpiry,
  JetbrainsStatus,
  VscodeExtStatus,
  WbVariant,
} from "@/lib/types";

/** In-flight credit fetches, shared so a remount does not start a second round. */
const creditInflight = new Set<string>();
/** 状态查询按档位各留一个在途请求，避免切档位时复用另一档位的结果。 */
let statusInflight: { variant: WbVariant; promise: Promise<AppStatus> } | undefined;
/** Account reads share one ordering across page loads and external-change events. */
let accountReadGeneration = 0;
/** A silent account reconciliation must not strand a page's status loading state. */
let fetchAllGeneration = 0;
/** Polls and switch/revision events may receive CLI status replies out of order. */
let cliStatusGeneration = 0;

function fetchStatus(variant: WbVariant): Promise<AppStatus> {
  if (!statusInflight || statusInflight.variant !== variant) {
    const promise = api.getStatus(variant).finally(() => {
      if (statusInflight?.promise === promise) statusInflight = undefined;
    });
    statusInflight = { variant, promise };
  }
  return statusInflight.promise;
}

async function fetchCreditExpiry(id: string): Promise<CreditExpiry> {
  const startedAt = Date.now();
  try {
    const result = await api.getCreditExpiry(id);
    return { ...result, updatedAt: result.updatedAt ?? startedAt };
  } catch (e) {
    return { ok: false, error: api.asError(e), updatedAt: startedAt };
  }
}

/** 各客户端最近一次读到的状态（账号页顶栏徽标与卡片入口共用）。 */
export interface ClientStatusSnapshot {
  codebuddyCli: CodeBuddyCliStatus | null;
  codebuddyCnIde: CodeBuddyCnIdeStatus | null;
  vscodeExt: VscodeExtStatus | null;
  jetbrains: JetbrainsStatus | null;
}

const EMPTY_CLIENT_STATUS: ClientStatusSnapshot = {
  codebuddyCli: null,
  codebuddyCnIde: null,
  vscodeExt: null,
  jetbrains: null,
};

interface AccountsState {
  accounts: AccountMeta[];
  /**
   * 全局当前档位：状态、本机导入、轮询都以它为准。
   * 缺省国内版，因此在未引入档位切换时行为与改造前一致。
   */
  variant: WbVariant;
  status: AppStatus | null;
  /**
   * 各客户端状态留在 store 而不是页面局部 state：账号页每次进入都会重挂载，
   * 局部 state 会被重置为 `null`，界面先按「未接入」渲染、等状态探测回来才改口
   * （issue #84 的「回切账号管理页状态显示延迟」）。放这里则先按上次结果渲染，
   * 后台刷新回来后自然覆盖。
   */
  clientStatus: ClientStatusSnapshot;
  loading: boolean;
  error: string | null;
  creditMap: Record<string, CreditExpiry>;
  creditLoadingMap: Record<string, boolean>;
  /** 账号 id -> 最近一次积分查询完成时间（成功/失败都记录） */
  creditUpdatedAtMap: Record<string, number>;
  refreshingCredits: boolean;
  lastCreditRefreshAt: number;
  setVariant: (variant: WbVariant) => void;
  /** 合并式更新各客户端状态：只覆盖传入的键，并发刷新不会互相清空。 */
  setClientStatus: (patch: Partial<ClientStatusSnapshot>) => void;
  refreshCodebuddyCliStatus: () => Promise<void>;
  fetchAll: () => Promise<void>;
  refreshStatus: (signal?: AbortSignal) => Promise<void>;
  deleteAccount: (id: string) => Promise<void>;
  /** Fetch credits only for ids not already cached. */
  ensureCredits: (accountIds: string[]) => Promise<void>;
  /** Force-refresh credits. `silent` skips toolbar/card loading flicker (timer). */
  refreshCredits: (accountIds: string[], opts?: { silent?: boolean }) => Promise<void>;
  importLocal: () => Promise<AccountMeta>;
  reconcileAccounts: () => Promise<void>;
}

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  variant: DEFAULT_VARIANT,
  status: null,
  clientStatus: EMPTY_CLIENT_STATUS,
  loading: false,
  error: null,
  creditMap: {},
  creditLoadingMap: {},
  creditUpdatedAtMap: {},
  refreshingCredits: false,
  lastCreditRefreshAt: 0,

  setVariant(variant) {
    const next = normalizeVariant(variant);
    if (get().variant === next) return;
    set({ variant: next });
    // 状态卡（运行中 / 当前账号 / 应用路径）随档位整体换一份。
    void get().fetchAll();
  },

  setClientStatus(patch) {
    set((state) => ({ clientStatus: { ...state.clientStatus, ...patch } }));
  },

  async refreshCodebuddyCliStatus() {
    const generation = ++cliStatusGeneration;
    try {
      const codebuddyCli = await api.getCodebuddyCliStatus();
      if (generation === cliStatusGeneration) get().setClientStatus({ codebuddyCli });
    } catch {
      // Keep the last committed snapshot while another host owns the auth lock.
    }
  },

  async fetchAll() {
    const variant = get().variant;
    const accountGeneration = ++accountReadGeneration;
    const generation = ++fetchAllGeneration;
    set({ loading: true, error: null });
    try {
      const [status, { accounts }] = await Promise.all([fetchStatus(variant), api.getAccounts()]);
      // 迟到结果不得覆盖已切换档位的状态。
      if (get().variant !== variant || generation !== fetchAllGeneration) return;
      set({
        status,
        loading: false,
        ...(accountGeneration === accountReadGeneration ? { accounts } : {}),
      });
    } catch (e) {
      if (get().variant !== variant || generation !== fetchAllGeneration) return;
      set({ error: api.asError(e), loading: false });
    }
  },

  async refreshStatus(signal) {
    const variant = get().variant;
    try {
      const status = await fetchStatus(variant);
      if (!signal?.aborted && get().variant === variant) set({ status });
    } catch {
      // 后台探测失败时保留最后一次成功状态，下一轮轮询继续尝试。
    }
  },

  async deleteAccount(id: string) {
    await api.deleteAccount(id);
    // A pre-delete read may still contain the removed account when it resolves.
    ++accountReadGeneration;
    creditInflight.delete(id);
    const { creditMap, creditLoadingMap, creditUpdatedAtMap } = get();
    const nextCredits = { ...creditMap };
    const nextLoading = { ...creditLoadingMap };
    const nextUpdatedAt = { ...creditUpdatedAtMap };
    delete nextCredits[id];
    delete nextLoading[id];
    delete nextUpdatedAt[id];
    set({
      accounts: get().accounts.filter((a) => a.id !== id),
      creditMap: nextCredits,
      creditLoadingMap: nextLoading,
      creditUpdatedAtMap: nextUpdatedAt,
    });
  },

  async ensureCredits(accountIds) {
    await loadCredits(accountIds, false, false);
  },

  async refreshCredits(accountIds, opts) {
    await loadCredits(accountIds, true, opts?.silent === true);
  },

  async importLocal() {
    // 本机导入按当前档位读取对应登录态文件。
    const res = await api.importLocal(get().variant);
    ++accountReadGeneration;
    await get().reconcileAccounts();
    return res.account;
  },

  async reconcileAccounts() {
    const generation = ++accountReadGeneration;
    const { accounts } = await api.getAccounts();
    if (generation === accountReadGeneration) set({ accounts });
  },
}));

async function loadCredits(accountIds: string[], force: boolean, silent: boolean) {
  const ids = [...new Set(accountIds.filter(Boolean))];
  if (ids.length === 0) return;

  const state = useAccountsStore.getState();
  const toFetch = force
    ? ids
    : ids.filter((id) => state.creditMap[id] === undefined && !creditInflight.has(id));
  if (toFetch.length === 0) return;

  for (const id of toFetch) creditInflight.add(id);
  if (!silent) {
    useAccountsStore.setState((s) => {
      const creditLoadingMap = { ...s.creditLoadingMap };
      for (const id of toFetch) creditLoadingMap[id] = true;
      return {
        creditLoadingMap,
        refreshingCredits: force ? true : s.refreshingCredits,
      };
    });
  }

  await Promise.all(
    toFetch.map(async (id) => {
      const result = await fetchCreditExpiry(id);
      creditInflight.delete(id);
      useAccountsStore.setState((s) => ({
        creditMap: { ...s.creditMap, [id]: newerCreditResult(s.creditMap[id], result) },
        creditUpdatedAtMap: { ...s.creditUpdatedAtMap, [id]: Date.now() },
        creditLoadingMap: silent ? s.creditLoadingMap : { ...s.creditLoadingMap, [id]: false },
      }));
    }),
  );

  useAccountsStore.setState((s) => ({
    lastCreditRefreshAt: Date.now(),
    refreshingCredits: silent ? s.refreshingCredits : force ? false : s.refreshingCredits,
  }));

  // A successful query may have renewed credentials and cleared an old login
  // warning in core. Re-read metadata for webui too, where Tauri events do not run.
  if (useAccountsStore.getState().accounts.some((a) => a.needsRelogin && toFetch.includes(a.id))) {
    await useAccountsStore.getState().reconcileAccounts().catch(() => {});
  }
}
