// workbuddy-switch postinstall：从「平台包」复制本平台二进制。
//
// 平台分包（esbuild 模式）：二进制发布在独立 npm 包（workbuddy-switch-<platform>-<arch>），
// 主包声明为 optionalDependencies，安装时 npm 自动装好平台包，postinstall 只需复制——
// 不依赖 GitHub，国内镜像（npmmirror）也能稳定安装。
//
// 环境变量覆盖：
//   WB_SWITCH_BINARY=<本地二进制路径>  本地开发/离线安装（直接复制，不联网）
const fs = require("fs");
const path = require("path");

const FILE = {
  "darwin-arm64": "wb-switch-darwin-arm64",
  "darwin-x64": "wb-switch-darwin-x64",
  "win32-x64": "wb-switch-win32-x64.exe",
  "linux-x64": "wb-switch-linux-x64",
  "linux-arm64": "wb-switch-linux-arm64",
}[`${process.platform}-${process.arch}`];

const PLATFORM_PKG = `workbuddy-switch-${process.platform}-${process.arch}`;

if (!FILE) {
  console.warn(
    `workbuddy-switch: 跳过平台 ${process.platform}-${process.arch}（当前不支持），` +
      `可手动下载二进制后放置到 bin/ 目录`,
  );
  process.exit(0);
}

const binDir = path.join(__dirname, "..", "bin");
const target = path.join(binDir, FILE);

function fail(msg) {
  console.error(`workbuddy-switch install: ${msg}`);
  console.error(
    "安装失败。请确认安装了对应平台包（npm 会自动装），或设置 WB_SWITCH_BINARY 指向本地二进制。",
  );
  process.exit(1);
}

function copyFrom(src) {
  fs.mkdirSync(binDir, { recursive: true });
  fs.copyFileSync(src, target);
  if (process.platform !== "win32") fs.chmodSync(target, 0o755);
  const size = fs.statSync(target).size;
  if (size < 1 * 1024 * 1024) {
    fs.unlinkSync(target);
    return fail(`平台包二进制异常（仅 ${size} 字节）`);
  }
  console.log(
    `workbuddy-switch: 二进制就绪 → ${target} (${(size / 1048576).toFixed(1)}MB)`,
  );
}

async function main() {
  // 1) 本地二进制覆盖（开发/离线）
  if (process.env.WB_SWITCH_BINARY) {
    const local = path.resolve(process.env.WB_SWITCH_BINARY);
    if (fs.existsSync(local)) return copyFrom(local);
    return fail(`WB_SWITCH_BINARY 指向的文件不存在: ${local}`);
  }

  // 2) 从平台包复制（node_modules/workbuddy-switch-<platform>-<arch>/bin/<file>）
  try {
    const pkgRoot = path.dirname(require.resolve(`${PLATFORM_PKG}/package.json`));
    const src = path.join(pkgRoot, "bin", FILE);
    if (fs.existsSync(src)) return copyFrom(src);
    return fail(`平台包 ${PLATFORM_PKG} 中未找到 ${FILE}`);
  } catch (e) {
    // 平台包缺失：可能是 optionalDependencies 没装上（如手动安装/旧版 npm）
    if (fs.existsSync(target)) {
      console.log("workbuddy-switch: 二进制已存在，跳过");
      return;
    }
    return fail(
      `平台包 ${PLATFORM_PKG} 未安装（${e.message}）。请重新执行 npm install。`,
    );
  }
}

main();
