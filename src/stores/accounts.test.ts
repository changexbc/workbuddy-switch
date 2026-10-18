import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountMeta, AppStatus, CodeBuddyCliStatus, CodeBuddyCnIdeStatus } from "@/lib/types";

const api = vi.hoisted(() => ({
  getStatus: vi.fn(),
  getAccounts: vi.fn(),
  deleteAccount: vi.fn(),
  importLocal: vi.fn(),
  getCreditExpiry: vi.fn(),
  getCodebuddyCliStatus: vi.fn(),
  asError: (error: unknown) => error instanceof Error ? error.message : String(error),
}));

vi.mock("@/lib/api", () => api);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function account(id: string, note = id): AccountMeta {
  return {
    id, note, uid: id, email: null, nickname: id, enterpriseName: null,
    expiresAt: null, refreshExpiresAt: null, refreshedAt: null, createdAt: null,
    needsRelogin: false, needsReloginReason: null,
  };
}

function status(variant: "cn" | "ai"): AppStatus {
  return { variant, running: false, authFile: "test", current: null, appPath: "test", version: "test" };
}

function cliStatus(id: string): CodeBuddyCliStatus {
  return {
    configured: true, settingsPresent: true, helperPresent: false, helperSupportsAccountIds: false,
    activeIndex: 0, activeAccountId: id, activeAccountName: id, accountCount: 2, statePath: "test",
  };
}

let store: typeof import("./accounts")["useAccountsStore"];

beforeEach(async () => {
  vi.resetAllMocks();
  vi.resetModules();
  store = (await import("./accounts")).useAccountsStore;
  api.getStatus.mockResolvedValue(status("cn"));
  api.deleteAccount.mockResolvedValue({ ok: true });
});

describe("account list responses from concurrent GUI and external changes", () => {
  it("keeps reconciled metadata when an initial page load waits for slow status", async () => {
    const slowStatus = deferred<AppStatus>();
    api.getStatus.mockReturnValueOnce(slowStatus.promise);
    api.getAccounts
      .mockResolvedValueOnce({ accounts: [account("a", "old note")] })
      .mockResolvedValueOnce({ accounts: [account("a", "new note"), account("b")] });

    const initialLoad = store.getState().fetchAll();
    await store.getState().reconcileAccounts();
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["a", "b"]);
    slowStatus.resolve(status("cn"));
    await initialLoad;

    expect(store.getState().accounts[0].note).toBe("new note");
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["a", "b"]);
    expect(store.getState().status?.variant).toBe("cn");
    expect(store.getState().loading).toBe(false);
  });

  it("keeps the latest reconciliation when events finish out of order", async () => {
    const olderResponse = deferred<{ accounts: AccountMeta[] }>();
    const newerResponse = deferred<{ accounts: AccountMeta[] }>();
    api.getAccounts.mockReturnValueOnce(olderResponse.promise).mockReturnValueOnce(newerResponse.promise);
    const olderRead = store.getState().reconcileAccounts();
    const newerRead = store.getState().reconcileAccounts();
    newerResponse.resolve({ accounts: [account("new")] });
    await newerRead;
    olderResponse.resolve({ accounts: [account("old")] });
    await olderRead;
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["new"]);
  });

  it("does not restore a deleted account from either pending read path", async () => {
    const initialResponse = deferred<{ accounts: AccountMeta[] }>();
    const eventResponse = deferred<{ accounts: AccountMeta[] }>();
    api.getAccounts.mockReturnValueOnce(initialResponse.promise).mockReturnValueOnce(eventResponse.promise);
    store.setState({ accounts: [account("keep"), account("delete")] });
    const initialLoad = store.getState().fetchAll();
    const eventRead = store.getState().reconcileAccounts();
    await store.getState().deleteAccount("delete");
    initialResponse.resolve({ accounts: [account("keep"), account("delete")] });
    eventResponse.resolve({ accounts: [account("keep"), account("delete")] });
    await Promise.all([initialLoad, eventRead]);
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["keep"]);
    expect(store.getState().loading).toBe(false);
    expect(api.deleteAccount).toHaveBeenCalledWith("delete");

    api.getAccounts.mockResolvedValueOnce({ accounts: [account("keep"), account("added")] });
    await store.getState().reconcileAccounts();
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["keep", "added"]);
  });

  it("does not discard a successful local import when an earlier read arrives", async () => {
    const olderResponse = deferred<{ accounts: AccountMeta[] }>();
    api.getAccounts
      .mockReturnValueOnce(olderResponse.promise)
      .mockResolvedValueOnce({ accounts: [account("old"), account("imported")] });
    api.importLocal.mockResolvedValueOnce({ ok: true, account: account("imported") });
    const olderRead = store.getState().reconcileAccounts();
    expect((await store.getState().importLocal()).id).toBe("imported");
    olderResponse.resolve({ accounts: [account("old")] });
    await olderRead;
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["old", "imported"]);
    expect(api.importLocal).toHaveBeenCalledWith("cn");
  });

  it("preserves the new variant's status and list when the old page load finishes", async () => {
    const cnStatus = deferred<AppStatus>();
    const aiStatus = deferred<AppStatus>();
    api.getStatus.mockReturnValueOnce(cnStatus.promise).mockReturnValueOnce(aiStatus.promise);
    api.getAccounts
      .mockResolvedValueOnce({ accounts: [account("cn-old")] })
      .mockResolvedValueOnce({ accounts: [account("ai-new")] });
    const cnLoad = store.getState().fetchAll();
    store.getState().setVariant("ai");
    aiStatus.resolve(status("ai"));
    await vi.waitFor(() => expect(store.getState().loading).toBe(false));
    cnStatus.resolve(status("cn"));
    await cnLoad;
    expect(store.getState().variant).toBe("ai");
    expect(store.getState().status?.variant).toBe("ai");
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["ai-new"]);
  });

  it("does not show an old load error after a newer page load succeeds", async () => {
    const olderResponse = deferred<{ accounts: AccountMeta[] }>();
    api.getAccounts
      .mockReturnValueOnce(olderResponse.promise)
      .mockResolvedValueOnce({ accounts: [account("new")] });
    const olderLoad = store.getState().fetchAll();
    await store.getState().fetchAll();
    olderResponse.reject(new Error("old response failed"));
    await olderLoad;
    expect(store.getState().accounts.map((item) => item.id)).toEqual(["new"]);
    expect(store.getState().error).toBeNull();
    expect(store.getState().loading).toBe(false);
  });

  it("keeps accepting pending reads when deletion fails without changing storage", async () => {
    const readResponse = deferred<{ accounts: AccountMeta[] }>();
    api.getAccounts.mockReturnValueOnce(readResponse.promise);
    api.deleteAccount.mockRejectedValueOnce(new Error("busy"));
    const read = store.getState().reconcileAccounts();
    await expect(store.getState().deleteAccount("a")).rejects.toThrow("busy");
    readResponse.resolve({ accounts: [account("a", "latest note")] });
    await read;
    expect(store.getState().accounts[0].note).toBe("latest note");
  });
});

describe("CodeBuddy CLI status responses from polls and switch events", () => {
  it("does not let an old poll override fresh status from a switch event", async () => {
    const olderResponse = deferred<CodeBuddyCliStatus>();
    api.getCodebuddyCliStatus
      .mockReturnValueOnce(olderResponse.promise)
      .mockResolvedValueOnce(cliStatus("new"));
    const olderPoll = store.getState().refreshCodebuddyCliStatus();
    await store.getState().refreshCodebuddyCliStatus();
    // Other client probes also update this shared store while the poll waits.
    const ideStatus: CodeBuddyCnIdeStatus = {
      installed: true, running: false, dataDir: "test", dbPath: "test", dbExists: true,
      appPath: "test", activeAccountId: "ide-account", activeAccountName: "ide-account",
    };
    store.getState().setClientStatus({ codebuddyCnIde: ideStatus });
    olderResponse.resolve(cliStatus("old"));
    await olderPoll;
    expect(store.getState().clientStatus.codebuddyCli?.activeAccountId).toBe("new");
    expect(store.getState().clientStatus.codebuddyCnIde).toBe(ideStatus);
  });

  it("retains committed status on busy errors and rejects earlier pending replies", async () => {
    store.getState().setClientStatus({ codebuddyCli: cliStatus("committed") });
    const olderResponse = deferred<CodeBuddyCliStatus>();
    api.getCodebuddyCliStatus
      .mockReturnValueOnce(olderResponse.promise)
      .mockRejectedValueOnce(new Error("AUTH_STATE_BUSY"));
    const olderPoll = store.getState().refreshCodebuddyCliStatus();
    await store.getState().refreshCodebuddyCliStatus();
    olderResponse.resolve(cliStatus("old"));
    await olderPoll;
    expect(store.getState().clientStatus.codebuddyCli?.activeAccountId).toBe("committed");
    api.getCodebuddyCliStatus.mockResolvedValueOnce(cliStatus("recovered"));
    await store.getState().refreshCodebuddyCliStatus();
    expect(store.getState().clientStatus.codebuddyCli?.activeAccountId).toBe("recovered");
  });
});

describe("login warning recovery after credit queries", () => {
  it("removes a recovered warning without relying on desktop events", async () => {
    store.setState({ accounts: [{ ...account("a"), needsRelogin: true, needsReloginReason: "old failure" }] });
    api.getCreditExpiry.mockResolvedValue({ ok: true, totalRemaining: 1 });
    api.getAccounts.mockResolvedValue({ accounts: [account("a")] });
    await store.getState().refreshCredits(["a"]);
    expect(store.getState().accounts[0].needsRelogin).toBe(false);
    expect(store.getState().creditMap.a.ok).toBe(true);
    expect(api.getAccounts).toHaveBeenCalledTimes(1);
  });

  it("keeps the warning when access succeeds but renewal still fails", async () => {
    const flagged = { ...account("a"), needsRelogin: true, needsReloginReason: "renewal failed" };
    store.setState({ accounts: [flagged] });
    api.getCreditExpiry.mockResolvedValue({ ok: true });
    api.getAccounts.mockResolvedValue({ accounts: [flagged] });
    await store.getState().refreshCredits(["a"]);
    expect(store.getState().accounts[0].needsRelogin).toBe(true);
    expect(store.getState().creditMap.a.ok).toBe(true);
  });

  it("preserves query results when metadata reconciliation fails", async () => {
    store.setState({ accounts: [{ ...account("a"), needsRelogin: true }] });
    api.getCreditExpiry.mockResolvedValue({ ok: true });
    api.getAccounts.mockRejectedValue(new Error("busy"));
    await store.getState().refreshCredits(["a"]);
    expect(store.getState().creditMap.a.ok).toBe(true);
    expect(store.getState().refreshingCredits).toBe(false);
    expect(store.getState().accounts[0].needsRelogin).toBe(true);
  });
});
