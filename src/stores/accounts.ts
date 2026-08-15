import { create } from "zustand";
import * as api from "@/lib/api";
import type { AccountMeta, AppStatus } from "@/lib/types";

interface AccountsState {
  accounts: AccountMeta[];
  status: AppStatus | null;
  loading: boolean;
  error: string | null;
  fetchAll: () => Promise<void>;
  deleteAccount: (id: string) => Promise<void>;
  importLocal: () => Promise<AccountMeta>;
  upsertAccount: (acc: AccountMeta) => void;
}

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  status: null,
  loading: false,
  error: null,

  async fetchAll() {
    set({ loading: true, error: null });
    try {
      const [status, { accounts }] = await Promise.all([api.getStatus(), api.getAccounts()]);
      set({ status, accounts, loading: false });
    } catch (e) {
      set({ error: api.asError(e), loading: false });
    }
  },

  async deleteAccount(id: string) {
    await api.deleteAccount(id);
    set({ accounts: get().accounts.filter((a) => a.id !== id) });
  },

  async importLocal() {
    const res = await api.importLocal();
    get().upsertAccount(res.account);
    return res.account;
  },

  upsertAccount(acc: AccountMeta) {
    const accounts = get().accounts;
    const idx = accounts.findIndex((a) => a.id === acc.id);
    if (idx >= 0) {
      const next = [...accounts];
      next[idx] = acc;
      set({ accounts: next });
    } else {
      set({ accounts: [...accounts, acc] });
    }
  },
}));
