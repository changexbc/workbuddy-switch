#!/usr/bin/env node
// workbuddy-switch npm 入口：spawn 平台二进制（postinstall 复制，见 scripts/install.js）。
const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

const binDir = path.join(__dirname, "..", "bin");

// 平台 → 二进制文件名（与 install.js 保持一致）
const FILE = {
  "darwin-arm64": "wb-switch-darwin-arm64",
  "darwin-x64": "wb-switch-darwin-x64",
  "win32-x64": "wb-switch-win32-x64.exe",
  "linux-x64": "wb-switch-linux-x64",
  "linux-arm64": "wb-switch-linux-arm64",
}[`${process.platform}-${process.arch}`];

if (!FILE) {
  console.error(`wb-switch: 不支持平台 ${process.platform}-${process.arch}`);
  process.exit(1);
}

const binPath = path.join(binDir, FILE);
if (!fs.existsSync(binPath)) {
  console.error(
    "workbuddy-switch: 未找到平台二进制，请重新安装（npm install -g workbuddy-switch）",
  );
  process.exit(1);
}

const child = spawn(binPath, process.argv.slice(2), { stdio: "inherit" });

// Keep the npm parent alive while the CLI finishes an authentication commit.
// On Windows console Ctrl+C/Ctrl+Break is already broadcast to the child;
// child.kill() would force-terminate it and bypass its graceful handler.
const forward = signal => {
  if (process.platform !== "win32" && child.exitCode === null) {
    child.kill(signal);
  }
};
process.on("SIGINT", () => forward("SIGINT"));
process.on("SIGTERM", () => forward("SIGTERM"));
if (process.platform === "win32") {
  process.on("SIGBREAK", () => forward("SIGBREAK"));
}
child.once("error", error => {
  console.error(`workbuddy-switch: 无法启动平台二进制 (${error.code || "unknown error"})`);
  process.exitCode = 1;
});
child.once("close", (code, signal) => {
  process.exitCode = code ?? (signal === "SIGINT" ? 130 : signal === "SIGTERM" ? 143 : 1);
});
