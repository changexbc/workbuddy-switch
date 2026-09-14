import { Component, type ErrorInfo, type ReactNode } from "react";

import { Button } from "@/components/ui/button";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/**
 * 全局错误边界：任何渲染异常只显示可读错误页，
 * 避免整窗塌成纯背景色（看起来就是「全黑、没内容」）。
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[ErrorBoundary]", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="flex h-screen flex-col items-center justify-center gap-4 bg-background p-8 text-foreground">
        <div className="max-w-md space-y-2 text-center">
          <p className="text-lg font-semibold">界面出现异常</p>
          <p className="break-all text-sm text-muted-foreground">
            {error.message || String(error)}
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" onClick={() => this.setState({ error: null })}>
            尝试恢复
          </Button>
          <Button onClick={() => window.location.reload()}>重新加载界面</Button>
        </div>
      </div>
    );
  }
}
