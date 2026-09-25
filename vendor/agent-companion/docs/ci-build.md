# GitHub Actions 构建与发布

`.github/workflows/build.yml` 支持手动构建和版本标签构建。在 GitHub 仓库的 **Actions → Build desktop app → Run workflow** 中，可选择全部平台或单个平台；安装包保存在该次运行的 Artifacts 中，不会创建 Release。

| 平台 | runner | 安装包 | 应用内更新包 |
| --- | --- | --- | --- |
| macOS Apple Silicon | `macos-15` | 包含 `.app` 的 zip | `Agent-Companion_macos_aarch64.app.tar.gz` |
| macOS Intel | `macos-15-intel` | 包含 `.app` 的 zip | `Agent-Companion_macos_x86_64.app.tar.gz` |
| Windows x64 | `windows-2025` | NSIS `Agent-Companion_windows_x86_64-setup.exe` | 同一个安装包 |
| Linux x64 | `ubuntu-24.04` | `.deb`、`Agent-Companion_linux_x86_64.AppImage` | 同一个 AppImage |

推送 `vX.Y.Z` 标签时，工作流会先检查标签版本与 `package.json`、`src-tauri/tauri.conf.json` 的 `version` 一致，再构建全部平台。全部成功后，安装包、更新包、`.sig` 与合并出的 `latest.json` 会进入同名**草稿 Release**，供安装验证后发布。标签构建无需另设发布密钥；Release 使用工作流的 `GITHUB_TOKEN`。

每个平台都在本架构 runner 上执行 Tauri 的 `beforeBuildCommand`，编译并嵌入对应架构的 `agent-studio-runtime`。macOS 包当前没有开发者签名和公证；Windows / Linux 的安装与运行尚未验证。Actions 构建成功只表示产物已生成，不代表目标系统上的 Hooks / Webhook、托盘和透明窗口均已通过验收。

## 质量门禁

在此之前测试结果只有开发机知道：`build.yml` 只做打包、不跑任何检查，前端用例与 crate 测试全红也能发出正式 Release。现在分成两道——**信号**（push 后立刻知道坏了）和**闸门**（坏了就发不出去）：

| 位置 | job | 跑什么 | 触发 |
| --- | --- | --- | --- |
| `.github/workflows/test.yml` | `web checks` | `npm run ci:web`：typecheck → lint → 全部用例 → `vite build` → bundle 检查 | push `main`、手动 |
| `.github/workflows/test.yml` | `rust tests (ubuntu-24.04)`、`rust tests (windows-latest)` | `cargo test -p agent-studio-core -p agent-studio-runtime --locked` | 同上 |
| `.github/workflows/test.yml` | `rust/node parity` | `node scripts/qa-rust-parity.mjs`（Rust 与 Node 快照一致性） | 同上 |
| `.github/workflows/build.yml` | `quality gate` | 同一条 `npm run ci:web` + 同一条 cargo test（单平台求快） | `v*` 标签、手动 |

两道刻意分成两条 workflow：`test.yml` 与发布流程完全解耦，`build.yml` 里的 `quality` 是发布路径上的闸门——`build` 矩阵 `needs: [select-platforms, quality]`，`release` 继续只依赖 `build`，所以 `quality` 失败时四平台产物与草稿 Release 都不会产出。GitHub 的 `needs` 不能跨 workflow 指向「另一个 workflow 最近一次成功」，要复用只能上 `workflow_call`（把触发条件耦合起来）或 `workflow_run`（结果订阅，易漏），所以闸门在发布路径上重复跑一遍检查，换来「谁挡住了发布」一眼可见、可单独重跑。

web 侧的命令清单只有一份来源：`package.json` 的 `ci:web` script，两条 workflow 都调用它，避免两份 YAML 各写一遍 `typecheck && lint && ...` 后慢慢漂移。Rust 侧那条 `cargo test` 在 `test.yml` 的 `rust` job 与 `build.yml` 的 `quality` job 里各写一次，接受这点重复——包成 npm script 的话，`rust` 矩阵的 ubuntu 与 windows 两个 job（现在不装 Node）都得再装 Node，不划算。

两个刻意的偏离：

- **不放 macOS**：这两个 crate 的平台分支只有 `#[cfg(unix)]` 与 `#[cfg(windows)]` 两种，macOS 与 ubuntu 落在同一分支、不带来额外覆盖，而 macOS runner 按 10× 计费且开发机就是 macOS。测试矩阵因而只有 ubuntu + windows。
- **不装 webkit 等系统依赖**：`agent-studio-core` / `agent-studio-runtime` 零 tauri 依赖。`agent-studio-desktop`、`src-tauri` 的测试与需要 `tauri build` 的原生 QA 仍留在本地。

测试 runner 与打包 runner 不完全同一批（打包用 `macos-15` / `macos-15-intel` / `windows-2025` / `ubuntu-24.04`），测试通过不等于打包环境通过，两者别混为一谈。

### 本地复跑

```sh
npm run ci:web                                                    # 与 web checks 同一组命令；quality gate 的前半段
cargo test -p agent-studio-core -p agent-studio-runtime --locked   # 与 rust tests、以及 quality gate 的 crate 测试同一条
node scripts/qa-rust-parity.mjs                                   # 只对应 rust/node parity，不在 quality gate 里
```

### 额度

> 仓库已于 2026-09-25 转公开，标准 runner 免费，下面的额度测算只作为私有阶段的取舍记录保留。

私有 Free 每月 2000 分钟，ubuntu 按 1× 计费、Windows 2×、macOS 10×。本方案每次 push 约 13 计费分钟（ubuntu ~7 + windows ~6），约合每月 150 次 push；若把 macOS 加进矩阵会变成每次约 43 分钟、每月只剩 40 多次——这是 `rust` 矩阵只有两个平台、以及手动验证时优先选 `platform=linux-x64` 的直接原因。转公开后标准 runner 免费，这个约束消失。

### 转公开后的三件后续（2026-09-25 已转公开）

1. ✅ `test.yml` 已增加 `pull_request` 触发；标准 runner 免费后，PR 触发不再消耗额度。
2. ⏳ 评估把 `macos-14` 加回 `rust` 矩阵：只有出现 macOS 专属分支（如 `#[cfg(target_os = "macos")]`）或打包环境相关改动时才值得。macOS 打包本身用 `workflow_dispatch platform=mac-arm64` 单独验证，不必进测试矩阵。
3. ✅ 分支保护已配置推送路径的三个 required status check：`web checks`、`rust tests (ubuntu-24.04)`、`rust tests (windows-latest)`（`strict` 开启，不要求 PR，直接推 `main` 不受影响）。发布路径的 `quality gate` 只在 tag / 手动构建里出现，配成 required 会让普通 PR 永久停在「Expected」，因此不配——它已经由 `build` 矩阵的 `needs: [select-platforms, quality]` 挡住。

## 更新签名密钥

应用内更新只信任**构建时**写入的 minisign 公钥，签名私钥只存在于 CI 机密与你自己备份的离线副本中，绝不进入仓库、日志或产物。

首次配置：

1. 在可信机器上生成一对本应用专用密钥（不要复用 wb-switch 或其他应用的密钥）：
   ```sh
   npx tauri signer generate -w ~/.tauri/agent-companion.key
   ```
2. 在 GitHub 仓库 **Settings → Secrets and variables → Actions** 中配置：
   - **Variables**：`TAURI_UPDATER_PUBKEY` = 公钥内容（`~/.tauri/agent-companion.pub`）
   - **Secrets**：`TAURI_SIGNING_PRIVATE_KEY` = 私钥内容（`~/.tauri/agent-companion.key`）
   - **Secrets**（仅当私钥有密码）：`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
3. 离线备份私钥并记录口令。私钥丢失后只能用一对新密钥重签，旧版本不会信任新密钥：用户需要手动安装一次新版本，或继续用旧密钥签名一个过渡版本。

CI 行为：

- 标签构建带 `--require-signing`：公钥缺失会立即失败，不会创建不可更新的 Release；私钥缺失时 Tauri 打包也会失败（`A public key has been found, but no private key`）。
- 手动构建带 `--optional-signing`：未配置公钥时把 `bundle.createUpdaterArtifacts` 改为 `false`，只产出用于试装的安装包。
- `scripts/configure-updater.mjs` 只把公钥写进构建检出中的 `src-tauri/tauri.conf.json`，并校验 `plugins.updater.endpoints` 指向本仓库的 `latest.json`；私钥从不经过该脚本，也不打印。

仓库内的 `src-tauri/tauri.conf.json` 刻意保留 `"pubkey": ""`：本地 `npm run desktop:build` 通过 `--config` 关闭更新产物（不需要任何密钥），开发构建因此只显示「当前构建未配置更新签名公钥」，不会误报可更新。发布构建由 CI 注入公钥。

## 更新清单与发布门禁

每个矩阵任务在打包后由 `scripts/update-manifest.mjs collect` 断言该平台**恰好一个**签名更新包及其 `.sig`，把更新包改名为固定的发布资产名，并写出 `update-fragment-<平台>.json`。Release 任务运行 `node scripts/update-manifest.mjs merge`，只有全部条件成立才会生成 `latest.json`：

- 四个平台片段齐全，且版本与标签一致；
- 平台键恰好为 `darwin-aarch64`、`darwin-x86_64`、`windows-x86_64-nsis`、`windows-x86_64`、`linux-x86_64`；
- 每个地址都是本 Release 资产（`https://github.com/changexbc/agent-companion/releases/latest/download/<资产名>`），且资产确实存在于产物目录；
- 每个更新包都有非空签名，不同更新包的签名不重复。

`latest.json` 与更新包一起进入**草稿** Release。`/releases/latest/download/latest.json` 指向最近一个已发布版本，所以草稿期间线上客户端看到的仍是上一个稳定版；发布后才会切换到新版本。

发布前需要人工验证（每个平台都要做）：

1. 在干净系统或干净用户目录安装本次草稿中的安装包；
2. 把上一稳定版升级到本次草稿版本：打开设置 → 更新，确认状态从「发现新版本」走到下载完成，点击「重启并安装」后版本号更新；
3. 确认升级失败、断网、代理无效等情况下界面显示错误且可重试，且不会自动安装或重启；
4. 全部通过后再发布草稿。任一平台失败：修复并重建同一候选版本，或保留草稿不发布。

> 可选加固（本轮不启用）：首次签名发布时评估 Tauri updater 2.12+ 的 `requireSignedVersion`——它要求清单声明的版本号与更新包内已签名版本一致，防止 `latest.json` 被换成「更高版本号 + 旧的已签名包」来阻断升级。评估通过前不启用，也不修改配置。

## 平台注意事项

- **macOS**：当前 `.app` 未做开发者签名与公证，Gatekeeper 首次启动的提示见 [README](../README.md#macos-注意事项)。更新签名只保证更新包来源可信，不替代系统代码签名/公证；升级后的新版本仍会走同样的 Gatekeeper 流程。升级需要应用有写权限（`/Applications` 下由用户确认授权）。
- **Windows**：更新包就是 NSIS 安装器。安装器运行时应用会退出，属于预期行为；卸载或管理员安装模式请按 NSIS 的提示处理。Windows 尚未做过实机验收。
- **Linux**：只有 AppImage 支持应用内更新，且必须从 AppImage 启动（`$APPIMAGE` 指向自身）；`.deb` 安装的版本请手动下载新包升级。Linux 尚未做过实机验收。

## 本地验证

```sh
npm test                                                  # 含更新契约与发布清单用例
cargo test --manifest-path src-tauri/Cargo.toml           # 更新状态机、代理校验、配置读写
node scripts/update-manifest.mjs merge --dir <产物目录> --version <版本>   # 用真实产物复核清单
```

没有签名私钥时无法在本地产出可被信任的更新包；请在草稿 Release 的安装验证中完成签名链路验收。
