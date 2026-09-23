import { Component, type ErrorInfo, type ReactNode } from "react";
import { ChevronDown, Copy, RotateCw } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Toaster } from "@/components/ui/sonner";
import { canPersistErrorLog, reportError } from "@/lib/error-report";

interface AppErrorBoundaryProps {
  children: ReactNode;
}

interface AppErrorBoundaryState {
  error: Error | null;
  /** React 给出的组件栈；只在 componentDidCatch 里拿到，仅用于展示与复制。 */
  componentStack: string;
}

/**
 * 整棵组件树的最后防线：渲染期崩溃时展示可复制、可重新加载的错误页，而不是整页白屏。
 *
 * 覆盖范围是渲染 / 生命周期 / 构造函数中的异常；事件回调与异步链路的异常由
 * `lib/error-report.ts` 的全局捕获负责（toast + 落盘，不接管界面）。
 * 错误页自带 `Toaster`：App 已经卸载，全局那份不再存在，否则「复制详情」的反馈看不到。
 */
export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { error: null, componentStack: "" };

  static getDerivedStateFromError(error: Error): Partial<AppErrorBoundaryState> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // 组件栈只能在 componentDidCatch 里取到，写入 state 供错误页展示与复制。
    this.setState({ componentStack: info.componentStack ?? "" });
    reportError("frontend_crash", error.message || error.name || "未知错误", {
      detail: [error.stack ?? "", info.componentStack ? `组件栈:${info.componentStack}` : ""]
        .filter(Boolean)
        .join("\n\n"),
      source: "react-error-boundary",
    });
  }

  /** 「复制详情」的内容：出错时间、页面位置、错误摘要与完整栈。 */
  private detailsText(): string {
    const { error, componentStack } = this.state;
    return [
      `时间: ${new Date().toISOString()}`,
      `页面: ${window.location.href}`,
      `错误: ${error?.name ?? "Error"}: ${error?.message ?? "未知错误"}`,
      "",
      error?.stack ?? "(无堆栈)",
      componentStack ? `\n组件栈:${componentStack}` : "",
    ].join("\n");
  }

  private copyDetails = async (): Promise<void> => {
    try {
      await navigator.clipboard.writeText(this.detailsText());
      toast.success("错误详情已复制");
    } catch {
      toast.error("复制失败", { description: "剪贴板不可用，请展开详情后手动选中复制。" });
    }
  };

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="flex min-h-screen items-center justify-center bg-background p-6">
        <Card className="w-full max-w-2xl min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">
          <CardContent className="space-y-3 p-5">
            <div className="space-y-1">
              <h1 className="text-[13px] font-medium leading-5">应用出现错误</h1>
              <p className="break-all text-xs leading-5 text-muted-foreground">
                {error.message || "未知错误"}
              </p>
            </div>

            <Collapsible>
              <CollapsibleTrigger asChild>
                <Button variant="ghost" size="sm" className="-ml-2.5">
                  <ChevronDown />
                  查看错误详情
                </Button>
              </CollapsibleTrigger>
              <CollapsibleContent>
                <pre className="mt-2 max-h-64 overflow-auto rounded-lg bg-foreground/[0.04] p-3 font-mono text-[11px] leading-5 break-all whitespace-pre-wrap text-muted-foreground">
                  {this.detailsText()}
                </pre>
              </CollapsibleContent>
            </Collapsible>

            <div className="flex flex-wrap items-center gap-2">
              <Button size="sm" onClick={() => window.location.reload()}>
                <RotateCw />
                重新加载
              </Button>
              <Button size="sm" variant="outline" onClick={() => void this.copyDetails()}>
                <Copy />
                复制详情
              </Button>
            </div>

            <p className="text-xs leading-5 text-muted-foreground">
              {canPersistErrorLog()
                ? "错误已记录到本机错误日志，可在设置页「错误日志」查看。若界面一直无法恢复，可从托盘菜单「检查更新」升级到最新版本。"
                : "可以先点「重新加载」再试一次。若一直失败，请前往 GitHub 下载最新版本。"}
            </p>
          </CardContent>
        </Card>
        <Toaster />
      </div>
    );
  }
}
