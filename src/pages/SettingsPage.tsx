import { useEffect, useState } from "react";
import { ArrowLeftRight, ArrowUpCircle, CalendarCheck, ExternalLink, Loader2, RefreshCw, Rocket, Save } from "lucide-react";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import type {
  AutoRotateConfig,
  CheckinConfig,
  CheckinLog,
  GithubConfig,
  RotateLog,
  RotateStatus,
  UpdateInfo,
} from "@/lib/types";
import { GITHUB_RELEASE_URL, GITHUB_REPOSITORY_URL, openReleaseUrl } from "@/lib/update";
import { UpdateInstallDialog } from "@/components/update-install-dialog";
import { useAccountsStore } from "@/stores/accounts";

function formatTime(ts: number): string {
  try {
    return new Date(ts).toLocaleString("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return String(ts);
  }
}

function logLabel(result: string): { text: string; tone: "success" | "warning" | "error" } {
  switch (result) {
    case "success":
      return { text: "签到成功", tone: "success" };
    case "already":
      return { text: "已签到", tone: "warning" };
    default:
      return { text: "失败", tone: "error" };
  }
}

/** 自动签到配置 + 一键签到 + 日志。 */
function AutoCheckinCard() {
  const [cfg, setCfg] = useState<CheckinConfig | null>(null);
  const [logs, setLogs] = useState<CheckinLog[]>([]);
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    void load();
  }, []);

  async function load() {
    try {
      const [c, l] = await Promise.all([api.getAutoCheckinConfig(), api.getCheckinLogs()]);
      setCfg(c);
      setLogs(l.logs);
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    }
  }

  async function save() {
    if (!cfg) return;
    setSaving(true);
    setMsg(null);
    try {
      const saved = await api.saveAutoCheckinConfig(cfg);
      setCfg(saved);
      setMsg({ type: "ok", text: "配置已保存" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setSaving(false);
    }
  }

  async function checkinAllNow() {
    setBusy(true);
    setMsg(null);
    try {
      const res = await api.checkinAll();
      if (res.status === "skipped" && res.reason === "already_running") {
        setMsg({ type: "err", text: "签到任务正在进行，请稍后再试" });
        return;
      }
      const ok = res.accounts.filter((a) => a.result === "success").length;
      const already = res.accounts.filter((a) => a.result === "already").length;
      const err = res.accounts.filter((a) => a.result === "error").length;
      const detail = res.accounts
        .filter((a) => a.result === "error")
        .map((a) => `${a.email}（${a.error}）`)
        .join("；");
      setMsg({
        type: err > 0 ? "err" : "ok",
        text: `签到完成：成功 ${ok}，已签 ${already}，失败 ${err}${detail ? `。${detail}` : ""}`,
      });
      void load();
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  function setNum(key: keyof CheckinConfig, value: string) {
    if (!cfg) return;
    setCfg({ ...cfg, [key]: Number(value) });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <CalendarCheck className="size-4" />
          自动签到
        </CardTitle>
        <CardDescription>
          开启后，启动时立即检查全部账号，运行期间每 30 分钟自动补签；并每天自动保活 token。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {cfg ? (
          <>
            <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
              <div>
                <div className="text-sm font-medium">启用自动签到</div>
                <div className="text-xs text-muted-foreground">
                  启动时立即核验服务端状态，未签到账号会自动补签
                </div>
              </div>
              <Switch
                checked={cfg.enabled}
                onCheckedChange={(v) => setCfg({ ...cfg, enabled: v })}
              />
            </div>

            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label htmlFor="ac-keep">保活阈值（天，0=每天无条件刷新）</Label>
                <Input
                  id="ac-keep"
                  type="number"
                  min={0}
                  max={90}
                  value={cfg.keepalive_days}
                  onChange={(e) => setNum("keepalive_days", e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="ac-lazy">惰性刷新（小时）</Label>
                <Input
                  id="ac-lazy"
                  type="number"
                  min={1}
                  max={72}
                  value={cfg.lazy_refresh_hours}
                  onChange={(e) => setNum("lazy_refresh_hours", e.target.value)}
                />
              </div>
            </div>

            <div className="flex flex-wrap gap-2">
              <Button onClick={save} disabled={saving}>
                {saving ? <Loader2 className="animate-spin" /> : <Save />}
                保存配置
              </Button>
              <Button variant="outline" onClick={checkinAllNow} disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <CalendarCheck />}
                全部立即签到
              </Button>
            </div>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">加载配置中…</p>
        )}

        {msg && (
          <Alert variant={msg.type === "err" ? "destructive" : "default"}>
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}

        <div>
          <p className="mb-2 text-sm font-medium">签到日志（最近 30 天）</p>
          {logs.length === 0 ? (
            <p className="py-3 text-center text-sm text-muted-foreground">暂无签到记录</p>
          ) : (
            <div className="max-h-64 space-y-1 overflow-y-auto pr-1">
              {logs.map((l, i) => {
                const tone = logLabel(l.result);
                return (
                  <div
                    key={i}
                    className="flex items-center justify-between rounded-md border px-3 py-1.5 text-xs"
                  >
                    <div className="min-w-0 flex-1 truncate">
                      <span className="font-medium">{l.email}</span>
                      {l.error && <span className="text-destructive">（{l.error}）</span>}
                    </div>
                    <div className="ml-2 flex shrink-0 items-center gap-2">
                      <span
                        className={
                          tone.tone === "error"
                            ? "text-destructive"
                            : tone.tone === "warning"
                              ? "text-amber-600"
                              : "text-emerald-600"
                        }
                      >
                        {tone.text}
                      </span>
                      <span className="text-muted-foreground">{formatTime(l.ts)}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

/** 自动轮换配置（CodeBuddy CLI）+ 手动检查 + 日志。 */
function AutoRotateCard() {
  const [cfg, setCfg] = useState<AutoRotateConfig | null>(null);
  const [status, setStatus] = useState<RotateStatus | null>(null);
  const [logs, setLogs] = useState<RotateLog[]>([]);
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    void load();
  }, []);

  async function load() {
    try {
      const [c, s, l] = await Promise.all([
        api.getAutoRotateConfig(),
        api.getRotateStatus(),
        api.getRotateLogs(),
      ]);
      setCfg(c);
      setStatus(s);
      setLogs(l.logs);
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    }
  }

  async function save() {
    if (!cfg) return;
    setSaving(true);
    setMsg(null);
    try {
      const saved = await api.saveAutoRotateConfig(cfg);
      setCfg(saved);
      setMsg({ type: "ok", text: "配置已保存" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setSaving(false);
    }
  }

  async function runNow() {
    setBusy(true);
    setMsg(null);
    try {
      const res = await api.runRotate();
      setMsg({
        type: res.status === "error" ? "err" : "ok",
        text:
          res.status === "switched"
            ? `已切换到 ${res.to ?? "目标账号"}`
            : res.status === "disabled"
              ? "自动轮换未启用（请在下方开启后重试）"
              : (res.reason ?? `检查完成：${res.status}`),
      });
      void load();
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  function setNum(key: keyof AutoRotateConfig, value: string) {
    if (!cfg) return;
    setCfg({ ...cfg, [key]: Number(value) });
  }

  function actionLabel(action: string): { text: string; tone: "success" | "warning" | "error" } {
    switch (action) {
      case "switched":
        return { text: "已切换", tone: "success" };
      case "skipped":
        return { text: "未切换", tone: "warning" };
      case "disabled":
        return { text: "未启用", tone: "warning" };
      case "error":
        return { text: "出错", tone: "error" };
      default:
        return { text: action, tone: "warning" };
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <ArrowLeftRight className="size-4" />
          CodeBuddy CLI 自动轮换
        </CardTitle>
        <CardDescription>
          定期把 CodeBuddy CLI 切到积分最紧迫（最早到期）的账号，防止积分过期浪费；正在使用时不切，所有账号到期还早时也不切。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {status && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-md border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
            <span>
              当前 CLI 账号：
              <b className="text-foreground">{status.activeAccountName ?? "未配置"}</b>
            </span>
            {status.lastCheckAt && <span>上次检查 {formatTime(status.lastCheckAt)}</span>}
            {status.lastSwitchAt && <span>上次切换 {formatTime(status.lastSwitchAt)}</span>}
            {!status.cliConfigured && (
              <span className="text-destructive">未接入 CodeBuddy CLI（请先到账号页安装 helper）</span>
            )}
          </div>
        )}

        {cfg ? (
          <>
            <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
              <div>
                <div className="text-sm font-medium">启用自动轮换</div>
                <div className="text-xs text-muted-foreground">
                  开启后按下方间隔自动检查并切换 CodeBuddy CLI 账号
                </div>
              </div>
              <Switch
                checked={cfg.enabled}
                onCheckedChange={(v) => setCfg({ ...cfg, enabled: v })}
              />
            </div>

            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
              <div className="space-y-1.5">
                <Label htmlFor="ar-interval">检查间隔（分钟）</Label>
                <Input
                  id="ar-interval"
                  type="number"
                  min={1}
                  max={1440}
                  value={cfg.check_interval_minutes}
                  onChange={(e) => setNum("check_interval_minutes", e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="ar-cooldown">切换冷却（分钟）</Label>
                <Input
                  id="ar-cooldown"
                  type="number"
                  min={1}
                  max={1440}
                  value={cfg.cooldown_minutes}
                  onChange={(e) => setNum("cooldown_minutes", e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="ar-gap">到期差异阈值（小时）</Label>
                <Input
                  id="ar-gap"
                  type="number"
                  min={0}
                  max={720}
                  value={cfg.min_gap_hours}
                  onChange={(e) => setNum("min_gap_hours", e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="ar-urgency">到期紧迫阈值（小时）</Label>
                <Input
                  id="ar-urgency"
                  type="number"
                  min={0}
                  max={720}
                  value={cfg.min_urgency_hours}
                  onChange={(e) => setNum("min_urgency_hours", e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="ar-guard">活跃保护（分钟）</Label>
                <Input
                  id="ar-guard"
                  type="number"
                  min={0}
                  max={1440}
                  value={cfg.active_guard_minutes}
                  onChange={(e) => setNum("active_guard_minutes", e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="ar-min">最小剩余积分</Label>
                <Input
                  id="ar-min"
                  type="number"
                  min={0}
                  value={cfg.min_remaining_credits}
                  onChange={(e) => setNum("min_remaining_credits", e.target.value)}
                />
              </div>
            </div>
            <p className="text-xs text-muted-foreground">
              切换时机：目标账号剩余到期时间少于「紧迫阈值」且比当前账号早超过「差异阈值」，且最近「活跃保护」分钟内 CLI 无对话、目标剩余积分不低于「最小剩余积分」。
            </p>

            <div className="flex flex-wrap gap-2">
              <Button onClick={save} disabled={saving}>
                {saving ? <Loader2 className="animate-spin" /> : <Save />}
                保存配置
              </Button>
              <Button variant="outline" onClick={runNow} disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <RefreshCw />}
                立即检查一次
              </Button>
            </div>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">加载配置中…</p>
        )}

        {msg && (
          <Alert variant={msg.type === "err" ? "destructive" : "default"}>
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}

        <div>
          <p className="mb-2 text-sm font-medium">轮换日志（最近 200 条）</p>
          {logs.length === 0 ? (
            <p className="py-3 text-center text-sm text-muted-foreground">暂无轮换记录</p>
          ) : (
            <div className="max-h-64 space-y-1 overflow-y-auto pr-1">
              {logs.map((l, i) => {
                const tone = actionLabel(l.action);
                return (
                  <div
                    key={i}
                    className="flex items-center justify-between rounded-md border px-3 py-1.5 text-xs"
                  >
                    <div className="min-w-0 flex-1 truncate">
                      {l.action === "switched" && l.from && l.to && (
                        <span className="font-medium">
                          {l.from.name ?? l.from.id} → {l.to.name ?? l.to.id}
                        </span>
                      )}
                      {l.reason && <span className="text-muted-foreground">（{l.reason}）</span>}
                    </div>
                    <div className="ml-2 flex shrink-0 items-center gap-2">
                      <span
                        className={
                          tone.tone === "error"
                            ? "text-destructive"
                            : tone.tone === "success"
                              ? "text-emerald-600"
                              : "text-amber-600"
                        }
                      >
                        {tone.text}
                      </span>
                      <span className="text-muted-foreground">{formatTime(l.ts)}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

/** 权限检测卡片：确认本 App 是否有权写入 WorkBuddy 认证文件。 */
function PermissionCheckCard() {
  const authFile = useAuthFile();
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<null | { ok: boolean; text: string }>(null);

  async function runCheck() {
    setChecking(true);
    setResult(null);
    try {
      const res = await api.checkAuthPermission();
      setResult({
        ok: res.ok,
        text: res.ok
          ? res.message ?? "认证目录可写，权限正常"
          : `${res.error}（${res.dir ?? ""}）`,
      });
    } catch (e) {
      setResult({ ok: false, text: api.asError(e) });
    } finally {
      setChecking(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>权限检测</CardTitle>
        <CardDescription>
          切换账号需要写入 WorkBuddy 认证文件，macOS 要求授权。此处可随时检测权限是否生效。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="break-all rounded-md bg-muted/60 px-3 py-2 font-mono text-xs text-muted-foreground">
          {authFile || "认证文件路径未获取"}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button onClick={runCheck} disabled={checking}>
            {checking ? "检测中…" : "检测权限"}
          </Button>
          <Button
            variant="outline"
            onClick={() => void api.openPermissionSettings("all_files")}
          >
            打开完全磁盘访问
          </Button>
          <Button
            variant="outline"
            onClick={() => void api.openPermissionSettings("app_management")}
          >
            打开 App 管理
          </Button>
          <Button variant="outline" onClick={() => void api.revealAppInFinder()}>
            在 Finder 中显示
          </Button>
        </div>

        {result && (
          <Alert variant={result.ok ? "default" : "destructive"}>
            <AlertDescription>{result.text}</AlertDescription>
          </Alert>
        )}
        {result && !result.ok && (
          <div className="rounded-md border bg-muted/60 p-3 text-xs text-muted-foreground">
            <p className="mb-1 font-medium text-foreground">如何授权（拖拽方式）：</p>
            <ol className="list-decimal space-y-1 pl-4">
              <li>点上方「打开完全磁盘访问」</li>
              <li>再点「在 Finder 中显示」打开 workbuddy-switch 所在位置</li>
              <li>
                把 <b>workbuddy-switch.app</b> 从 Finder <b>直接拖进</b>完全磁盘访问的列表区域
                （即使没有提示框，拖入即生效），然后打开它的开关
              </li>
              <li>回到本页点「检测权限」，或直接重试切换</li>
            </ol>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function useAuthFile(): string | undefined {
  return useAccountsStore((s) => s.status?.authFile);
}

/** 自动更新：检查公开 GitHub Releases 源 + 安装签名更新。 */
function UpdateCard() {
  const version = useAccountsStore((s) => s.status?.version);
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  const [installOpen, setInstallOpen] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);
  const [githubConfig, setGithubConfig] = useState<GithubConfig>({});
  const [proxyUrl, setProxyUrl] = useState("");
  const [proxySaving, setProxySaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void api
      .getGithubConfig()
      .then((config) => {
        if (cancelled) return;
        setGithubConfig(config);
        setProxyUrl(config.proxy ?? "");
      })
      .catch((e) => {
        if (!cancelled) setMsg({ type: "err", text: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function check() {
    setChecking(true);
    setMsg(null);
    try {
      const r = await api.checkUpdate(proxyUrl, true);
      setInfo(r);
      if (!r.ok) {
        setMsg({ type: "err", text: r.message || r.error || "检查失败" });
      }
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setChecking(false);
    }
  }

  async function saveProxy() {
    const value = proxyUrl.trim();
    if (value) {
      try {
        const parsed = new URL(value);
        if (!parsed.hostname || !["http:", "https:"].includes(parsed.protocol)) {
          throw new Error("unsupported proxy protocol");
        }
      } catch {
        setMsg({ type: "err", text: "代理地址格式不正确，请填写 HTTP/HTTPS 地址，例如 http://127.0.0.1:7897" });
        return;
      }
    }

    setProxySaving(true);
    setMsg(null);
    try {
      const saved = await api.saveGithubConfig({ ...githubConfig, proxy: value });
      setGithubConfig(saved);
      setProxyUrl(saved.proxy ?? "");
      setMsg({ type: "ok", text: value ? "更新代理已保存" : "已关闭更新代理" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setProxySaving(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <RefreshCw className="size-4" />
          自动更新
        </CardTitle>
        <CardDescription>
          检查公开 GitHub Releases 新版本，整包更新经签名校验后自动安装。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="text-sm">
          当前版本：<span className="font-mono">v{version || "?"}</span>
        </div>

        <div className="flex items-center justify-between gap-3 rounded-md border bg-muted/30 px-3 py-2.5 text-sm">
          <div className="min-w-0">
            <div className="font-medium">公开更新源</div>
            <div className="truncate text-xs text-muted-foreground">{GITHUB_REPOSITORY_URL}</div>
          </div>
          <Button
            variant="ghost"
            size="icon"
            title="打开 GitHub Release"
            onClick={() => void openReleaseUrl(GITHUB_RELEASE_URL)}
          >
            <ExternalLink />
          </Button>
        </div>

        <div className="space-y-2 rounded-md border bg-muted/30 px-3 py-3">
          <div>
            <Label htmlFor="update-proxy">更新代理地址</Label>
            <p className="mt-1 text-xs text-muted-foreground">
              仅用于 GitHub 更新检查和安装包下载；留空表示关闭显式代理。
            </p>
          </div>
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input
              id="update-proxy"
              value={proxyUrl}
              onChange={(event) => setProxyUrl(event.target.value)}
              placeholder="例如 http://127.0.0.1:7897"
              spellCheck={false}
              autoComplete="off"
            />
            <Button variant="outline" onClick={() => void saveProxy()} disabled={proxySaving}>
              {proxySaving ? <Loader2 className="animate-spin" /> : <Save />}
              保存代理
            </Button>
          </div>
        </div>

        <div className="flex flex-wrap gap-2">
          <Button variant="outline" onClick={check} disabled={checking}>
            {checking ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            检查更新
          </Button>
        </div>

        {info?.ok && (
          <Alert variant="default">
            <AlertDescription className="space-y-2">
              <div>
                {info.hasUpdate
                  ? `发现新版本 v${info.latest}（当前 v${info.current}）`
                  : `已是最新版本 v${info.current}`}
                {info.releaseName && <span className="text-muted-foreground"> · {info.releaseName}</span>}
              </div>
              {info.hasUpdate && (
                <Button size="sm" onClick={() => setInstallOpen(true)}>
                  <ArrowUpCircle />
                  立即升级
                </Button>
              )}
              {info.releaseUrl && (
                <Button
                  variant="link"
                  size="sm"
                  className="h-auto p-0"
                  onClick={() => void openReleaseUrl(info.releaseUrl)}
                >
                  打开 GitHub Release
                </Button>
              )}
            </AlertDescription>
          </Alert>
        )}
        {msg && (
          <Alert variant={msg.type === "err" ? "destructive" : "default"}>
          <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}
        <UpdateInstallDialog
          open={installOpen}
          onOpenChange={setInstallOpen}
          update={info}
        />
      </CardContent>
    </Card>
  );
}

/** 开机自启（仅桌面端渲染）：开关直接反映系统自启注册状态，切换立即生效。 */
function StartupCard() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    void api
      .getLaunchAtLoginEnabled()
      .then((value) => {
        if (!cancelled) setEnabled(value);
      })
      .catch((e) => {
        if (!cancelled) setMsg({ type: "err", text: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onToggle(value: boolean) {
    if (busy || enabled === null) return;
    const previous = enabled;
    setBusy(true);
    setMsg(null);
    try {
      // 后端回读 OS 权威状态；即使与请求一致，也以回读值显示。
      const authoritative = await api.setLaunchAtLoginEnabled(value);
      setEnabled(authoritative);
      setMsg({ type: "ok", text: authoritative ? "已开启开机自启" : "已关闭开机自启" });
    } catch (e) {
      // 失败时恢复到最后一次确认的状态，并显示可读错误。
      setEnabled(previous);
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Rocket className="size-4" />
          启动设置
        </CardTitle>
        <CardDescription>
          开启后，登录系统时自动启动并静默进入托盘：主窗口和 Dock / 任务栏入口不出现，签到等后台任务继续运行；手动启动仍正常显示主窗口。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
          <div>
            <div className="text-sm font-medium">开机时静默启动到托盘</div>
            <div className="text-xs text-muted-foreground">
              开关直接反映系统登录项状态；之后可从托盘「打开主界面」恢复
            </div>
          </div>
          <Switch
            checked={enabled ?? false}
            disabled={busy || enabled === null}
            onCheckedChange={(v) => void onToggle(v)}
            aria-label="开机时静默启动到托盘"
          />
        </div>

        {msg && (
          <Alert variant={msg.type === "err" ? "destructive" : "default"}>
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}
      </CardContent>
    </Card>
  );
}

/** 设置页：自动签到配置 / 权限检测 / 更新配置。 */
export default function SettingsPage() {
  return (
    <div className="mx-auto max-w-3xl px-6 py-6">
      <header className="mb-6">
        <h1 className="text-xl font-semibold">设置</h1>
        <p className="mt-1 text-sm text-muted-foreground">自动签到、权限检测与自动更新配置。</p>
      </header>

      <div className="space-y-4">
        <PermissionCheckCard />
        <AutoCheckinCard />
        <AutoRotateCard />
        {api.isDesktop() ? <StartupCard /> : null}
        {api.isWebui() ? null : <UpdateCard />}
      </div>
    </div>
  );
}
