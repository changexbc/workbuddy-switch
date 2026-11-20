// 构建后为 dist 内文本资源生成 .gz 预压缩副本。
// axum 侧（crates/wb-switch-server/src/api.rs static_handler）会按 Accept-Encoding
// 自动选用同名 .gz 返回 Content-Encoding: gzip；1.2MB 的主 JS 包可压到约 1/4，
// 慢链路（Tailscale DERP 中继等）下避免「应用启动失败」兜底被传输时间误触发。
// 已压缩格式（png/woff2/ico）不处理；压缩后不比原文件小的（罕见）直接丢弃副本。
import { gzipSync } from "node:zlib";
import { readdirSync, statSync, readFileSync, writeFileSync, unlinkSync } from "node:fs";
import { join, extname } from "node:path";
import { fileURLToPath } from "node:url";

const GZIP_EXTENSIONS = new Set([".js", ".mjs", ".css", ".html", ".svg", ".json", ".txt"]);
const MIN_BYTES = 1024; // 太小的文件压缩收益低,反而多占一份嵌入体积

const distDir = fileURLToPath(new URL("../dist/", import.meta.url));

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else out.push(full);
  }
  return out;
}

let totalOriginal = 0;
let totalGzipped = 0;
let count = 0;

for (const file of walk(distDir)) {
  if (file.endsWith(".gz")) {
    unlinkSync(file); // 旧的预压缩副本一律重建,避免与最新产物错配
    continue;
  }
  if (!GZIP_EXTENSIONS.has(extname(file))) continue;
  const raw = readFileSync(file);
  if (raw.length < MIN_BYTES) continue;
  const gz = gzipSync(raw, { level: 9 });
  if (gz.length >= raw.length) continue;
  writeFileSync(`${file}.gz`, gz);
  count += 1;
  totalOriginal += raw.length;
  totalGzipped += gz.length;
}

if (count === 0) {
  console.log("[gzip-dist] 没有可压缩的文本资源");
} else {
  const kb = (n) => `${(n / 1024).toFixed(0)}KB`;
  console.log(
    `[gzip-dist] 已生成 ${count} 个 .gz 预压缩副本: ${kb(totalOriginal)} -> ${kb(totalGzipped)} (${((totalGzipped / totalOriginal) * 100).toFixed(0)}%)`,
  );
}
