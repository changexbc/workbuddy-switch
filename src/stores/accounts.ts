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
  reconcileAccounts: () => Promise<void>;
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
    await get().reconcileAccounts();
    return res.account;
  },

  async reconcileAccounts() {
    const { accounts } = await api.getAccounts();
    set({ accounts });
  },
}));
