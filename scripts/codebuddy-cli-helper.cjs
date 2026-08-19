#!/usr/bin/env node
// CodeBuddy CLI apiKeyHelper for wb-switch.
//
// 只把当前选中账号的 token（Bearer xxx）打印到 stdout，CodeBuddy CLI 会把它
// 作为 Authorization 头使用。当前选中账号由 wb-switch 写入
// ~/.codebuddy-rotate/state.json（activeAccountId / active 索引）。
//
// 用 Node 实现：CodeBuddy CLI 本身就是 Node 应用（npm 安装，shebang 同样是
// #!/usr/bin/env node），helper 由 CLI 拉起时 node 必然可用——用户不需要安装
// Python 等任何额外环境。
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");

const rotateDir = process.env.CODEBUDDY_ROTATE_DIR || path.join(os.homedir(), ".codebuddy-rotate");
const accountsFile =
  process.env.WB_SWITCH_ACCOUNTS_FILE || path.join(os.homedir(), ".wb-switch", "accounts.json");
const stateFile = path.join(rotateDir, "state.json");

function fail(message) {
  process.stderr.write(`wb-switch: ${message}\n`);
  process.exit(1);
}

function readJson(file, fallback) {
  try {
    const text = fs.readFileSync(file, "utf8").replace(/^\uFEFF/, "");
    return JSON.parse(text);
  } catch {
    return fallback;
  }
}

const accounts = readJson(accountsFile, null);
if (!Array.isArray(accounts) || accounts.length === 0) {
  fail("no accounts available");
}

let state = readJson(stateFile, {});
if (typeof state !== "object" || state === null || Array.isArray(state)) {
  state = {};
}

function resolveIndex(state, accounts) {
  let index = 0;
  const activeId = typeof state.activeAccountId === "string" ? state.activeAccountId : "";
  if (activeId) {
    const found = accounts.findIndex((account) => account && account.id === activeId);
    if (found !== -1) return found;
  }
  const legacy = Number.parseInt(state.active, 10);
  if (Number.isFinite(legacy)) index = legacy;
  return ((index % accounts.length) + accounts.length) % accounts.length;
}

const index = resolveIndex(state, accounts);
const account = accounts[index];
const token = account && typeof account.access_token === "string" ? account.access_token.trim() : "";
if (!token) {
  fail("selected account has no access token");
}
process.stdout.write(`Bearer ${token}\n`);
