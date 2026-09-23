import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/bricolage-grotesque";
import App from "./App";
import "./index.css";
import { AppErrorBoundary } from "./components/app-error-boundary";
import { installGlobalErrorHandlers } from "./lib/error-report";
import { installNotificationArchive } from "./lib/notify";
import { applyTheme, getThemePreference, watchSystemTheme } from "./lib/theme";

applyTheme(getThemePreference());
const stopWatchingSystemTheme = watchSystemTheme();
if (import.meta.hot) import.meta.hot.dispose(stopWatchingSystemTheme);

// 提示存档必须在首个 toast 之前装好（包装 sonner 的四类提示）。
installNotificationArchive();

// 全局错误捕获要在挂载前装好：事件回调与异步链路的异常归它兜底（渲染期另有 ErrorBoundary）。
installGlobalErrorHandlers();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <AppErrorBoundary>
      <App />
    </AppErrorBoundary>
  </React.StrictMode>,
);

// 入口 JS 已接管首屏：index.html 的静态兜底脚本据此不再替换占位（否则 10 秒后会把
// 可用的界面或错误页换成「启动失败」提示）。
window.__WB_MOUNTED__ = true;
