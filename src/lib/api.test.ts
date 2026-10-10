import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getCreditExpiry, getStatus } from "./api";

/**
 * webui（浏览器）模式下的 HTTP 请求地址构造。
 *
 * `httpCall` 内部以相对路径发起请求（由 fetch 按页面来源解析），因此这里
 * 断言的是「传给 fetch 的 URL 是相对路径」，而不是浏览器解析后的绝对地址；
 * 修复目标即禁止出现写死的 `http://127.0.0.1:57890`。
 */

/** 让 `isWebui()` 成立的最小 window stub；origin 模拟 SSH 转发等非默认端口。 */
const PAGE_ORIGIN = "http://127.0.0.1:6100";

beforeEach(() => {
  vi.stubGlobal("window", { location: { origin: PAGE_ORIGIN } });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function stubFetch(impl?: (url: string, init?: RequestInit) => Promise<Response>) {
  const fetchMock = vi.fn(
    impl ??
      ((_url: string, _init?: RequestInit) =>
        Promise.resolve(
          new Response(JSON.stringify({ ok: true }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        )),
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("webui httpCall URL 构造", () => {
  it("GET 请求走相对路径并携带查询串，不写死 127.0.0.1:57890", async () => {
    const fetchMock = stubFetch();

    await getStatus("ai");

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/status?variant=ai");
    expect(init.method).toBe("GET");
    expect(init.body).toBeUndefined();
  });

  it("GET 无参数时不带查询串", async () => {
    const fetchMock = stubFetch();

    await getStatus();

    const [url] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/status");
  });

  it("POST 请求同样走相对路径并序列化请求体", async () => {
    const fetchMock = stubFetch();

    await getCreditExpiry("account-1");

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/credits");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(JSON.stringify({ accountId: "account-1" }));
  });

  it("服务不可达时错误文案带页面当前地址而非空地址", async () => {
    stubFetch(() => Promise.reject(new TypeError("Failed to fetch")));

    await expect(getStatus()).rejects.toHaveProperty(
      "message",
      `无法连接 workbuddy-switch 服务（${PAGE_ORIGIN}），请先运行 \`workbuddy-switch\``,
    );
  });

  it("页面来源为空或缺失时错误文案不含空括号或 undefined", async () => {
    vi.stubGlobal("window", { location: {} });
    stubFetch(() => Promise.reject(new TypeError("Failed to fetch")));

    await expect(getStatus()).rejects.toHaveProperty(
      "message",
      "无法连接 workbuddy-switch 服务，请先运行 `workbuddy-switch`",
    );
  });
});
