// 下载源：GitHub Releases（changexbc/wb-switch-rust），可用环境变量覆盖：
//   WB_SWITCH_BINARY=<本地二进制路径>  本地开发/离线安装（直接复制，不联网）
//   WB_SWITCH_DOWNLOAD_BASE=<URL 前缀>  自定义下载源（默认 GitHub latest release）
const fs = require("fs");
const path = require("path");
const os = require("os");
const https = require("https");

const OWNER = "changexbc"; // 发布二进制到 Release 的 GitHub 仓库
const REPO = "workbuddy-switch";

const FILE = {
  "darwin-arm64": "wb-switch-darwin-arm64",
  "darwin-x64": "wb-switch-darwin-x64",
  "win32-x64": "wb-switch-win32-x64.exe",
  "linux-x64": "wb-switch-linux-x64",
  "linux-arm64": "wb-switch-linux-arm64",
}[`${process.platform}-${process.arch}`];

if (!FILE) {
  console.warn(
    `wb-switch: 跳过平台 ${process.platform}-${process.arch}（当前不支持），` +
      `可手动下载二进制后放置到 bin/ 目录`,
  );
  process.exit(0);
}

const binDir = path.join(__dirname, "..", "bin");
const target = path.join(binDir, FILE);

function fail(msg) {
  console.error(`wb-switch install: ${msg}`);
  console.error("安装失败，可手动下载二进制放到 npm 包的 bin/ 目录。");
  process.exit(1);
}

function copyLocal(src) {
  fs.mkdirSync(binDir, { recursive: true });
  fs.copyFileSync(src, target);
  if (process.platform !== "win32") fs.chmodSync(target, 0o755);
  console.log(`wb-switch: 已从 ${src} 复制二进制`);
}

function download(url) {
  fs.mkdirSync(binDir, { recursive: true });
  console.log(`wb-switch: 正在下载 ${url}`);
  const file = fs.createWriteStream(target);
  https
    .get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        file.close();
        return download(res.headers.location);
      }
      if (res.statusCode !== 200) {
        file.close();
        fs.unlinkSync(target);
        return fail(`下载失败 (HTTP ${res.statusCode})`);
      }
      res.pipe(file);
      file.on("finish", () => {
        file.close();
        if (process.platform !== "win32") fs.chmodSync(target, 0o755);
        console.log(`wb-switch: 二进制就绪 → ${target}`);
      });
    })
    .on("error", (e) => {
      file.close();
      fs.unlinkSync(target);
      fail(`网络错误: ${e.message}`);
    });
}

async function main() {
  // 1) 本地二进制覆盖（开发/离线）
  if (process.env.WB_SWITCH_BINARY) {
    const local = path.resolve(process.env.WB_SWITCH_BINARY);
    if (fs.existsSync(local)) return copyLocal(local);
    return fail(`WB_SWITCH_BINARY 指向的文件不存在: ${local}`);
  }

  // 2) 已存在则跳过
  if (fs.existsSync(target)) {
    console.log(`wb-switch: 二进制已存在，跳过下载`);
    return;
  }

  // 3) 从 GitHub Releases 下载
  const base =
    process.env.WB_SWITCH_DOWNLOAD_BASE ||
    `https://github.com/${OWNER}/${REPO}/releases/latest/download`;
  download(`${base}/${FILE}`);
}

main();
