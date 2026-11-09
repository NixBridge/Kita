# 02 · 系统架构

## 1. 总览

Kita 是一个单进程 CLI。没有守护进程、没有系统服务、没有后台更新器。所有持久状态都在文件系统和 SQLite 中，可被审计、备份和删除。

```text
┌──────────────────────────────────────────────────────────────────────┐
│                              kita.exe                                │
│                                                                      │
│  ┌──────────┐   ┌───────────┐   ┌──────────┐   ┌─────────────────┐  │
│  │ Manifest │──▶│ Resolver  │──▶│ Planner  │──▶│ Execution Engine│  │
│  │ Loader   │   │ (PubGrub) │   │ (diff)   │   │ (transaction)   │  │
│  └──────────┘   └─────┬─────┘   └────┬─────┘   └────────┬────────┘  │
│                       │              │                  │           │
│                 ┌─────▼─────┐  ┌─────▼──────┐    ┌──────▼────────┐  │
│                 │ Registry  │  │ Installed  │    │ Store / Fetch │  │
│                 │ + cache   │  │ state      │    │ / Verifier    │  │
│                 └─────┬─────┘  └────────────┘    └──────┬────────┘  │
│                       │                                 │           │
│                 ┌─────▼─────────────────────────────────▼────────┐  │
│                 │           Windows Platform Layer               │  │
│                 │  shims · hardlinks · MSI · UAC · ACL · RM       │  │
│                 └───────────────────────┬───────────────────────┘  │
└─────────────────────────────────────────┼──────────────────────────┘
                                          │
              ┌───────────────────────────┼───────────────────────────┐
              │                           │                           │
       ┌──────▼──────┐            ┌───────▼───────┐          ┌────────▼───────┐
       │ Static repo │            │  Local state  │          │ Native Windows │
       │ HTTPS + sig │            │  CAS + SQLite │          │ MSI / registry │
       └─────────────┘            └───────────────┘          └────────────────┘
```

### 1.1 设计约束

- **单进程**：命令结束，进程结束。没有需要 watchdog 的组件。
- **离线优先**：读取本地状态、运行 shim、列出已安装包不依赖网络。
- **状态可重建**：`state.db` 可以从磁盘上的 generation / active.json / store 重建；它不是唯一真相。
- **平台隔离**：不安全的 Win32 调用集中在 `kita-win`，其余 crate `#![forbid(unsafe_code)]`。
- **可嵌入**：核心逻辑不依赖 stdout；CLI 只是 `kita-app` 的一个前端。JSON 事件流是稳定接口。

## 2. 关键概念

| 概念 | 定义 |
| --- | --- |
| **Environment** | 一组包及其激活状态的逻辑集合。v1 每个 scope 只有一个活动环境（默认 `default`），内部模型预留多环境。 |
| **Package** | 仓库中由 `<namespace>/<name>` 唯一标识的软件。 |
| **Version** | 包的某个具体版本，可能包含多个平台的 artifact。 |
| **Artifact** | 某个目标平台的可下载文件（zip / tar.zst / MSI；可选 `.kta` 容器）。 |
| **Instance** | 环境中的一个已安装包版本。v1 每个包 ID 最多一个实例。 |
| **Object** | 内容寻址的不可变文件，键为 SHA-256。 |
| **Tree** | 一个包的目录清单：相对路径 → object hash + 元数据。 |
| **Generation** | 环境的一次完整、不可变快照：instance 集合、文件树、工具表、环境变量。 |
| **Active Pointer** | 指向当前 generation 的原子文件（`active.json`），是运行时真相。 |
| **Plan** | 从当前状态到目标状态的、可序列化的操作集合。 |
| **Receipt** | 原生安装器（MSI 等）的安装记录：产品码、版本、卸载命令、安装位置、外围改动。 |

## 3. 一次 `kita sync` 的数据流

```text
1. 读取 kita.toml
        │
2. 校验 schema、规范化包名/版本需求、计算 manifest digest
        │
3. 读取 kita.lock（若存在且 --locked）
        │
4. 加载仓库元数据（本地缓存 + ETag / 签名）
        │
5. 解析依赖（PubGrub）
        │
6. 生成目标图，与已安装 generation 做 diff
        │
7. 生成 Plan（纯数据，不产生副作用）
        │
8. 用户确认 / --yes / --dry-run
        │
9. 下载缺失 artifact（并行、断点、校验）
        │
10. 解压到临时树，计算 tree digest，校验路径安全
        │
11. 执行原生安装器步骤（如有），串行、必要时提权，记录 receipt
        │
12. 从 CAS 硬链接构建新 generation（同卷），纳入 native 工具路径，写 generation.json + READY
        │
13. 原子替换 active.json
        │
14. 写 SQLite 审计记录，清理旧 generation（按保留策略）
```

任何一步失败：

- 第 9–10 步失败：删除临时树，现有 active 不变。
- 第 11 步失败：Windows Installer 回滚；已成功的 native 步骤按 journal 逆序 undo。
- 第 12 步失败：删除临时 generation；若本次已安装 native 包，卸载新 ProductCode / 重装旧 MSI。
- 第 13 步失败：active 仍是旧值，执行 native undo，清理未引用的 generation。
- 第 14 步失败：不影响运行状态；下次启动重建索引并告警。

## 4. 组件职责

### 4.1 Manifest Loader / Validator

- 解析 TOML，拒绝未知字段（`deny_unknown_fields`）。
- 语义校验：ID、版本、依赖图、路径、scope、平台。
- 计算 canonical manifest digest（规范化 TOML 的 SHA-256）。
- 不访问网络，不修改文件。

### 4.2 Registry Client

- 拉取并验证仓库元数据、包索引、manifest。
- 管理 ETag、zstd 缓存、离线模式、镜像回退。
- 对每个文件做签名与哈希校验。
- 不参与依赖求解。

### 4.3 Resolver

- 输入：manifest 需求、平台、scope、仓库快照。
- 使用 PubGrub 产出唯一解；返回完整的 derivation，用于解释冲突。
- 决策顺序确定：优先稳定版、优先精确 pin、优先少变更、最后按版本降序。
- 输出 `ResolvedGraph`，不下载 artifact。

### 4.4 Planner

- 对比 `ResolvedGraph` 与当前 generation。
- 产出 `Plan { steps, downloads, warnings, requires_elevation, reversible }`。
- 计划必须是纯函数：同样的输入和状态，得到同样的计划。
- 计划哈希用于提权代理的一次性校验。

### 4.5 Fetcher

- 有界并发（默认 8），按 host 限速。
- 支持 HTTP/2、zstd、ETag、Range 续传（可选）。
- 镜像失败自动切换；每个镜像下载后都必须通过同一哈希校验。
- 写入 `.part`，成功后 `MoveFileEx` 重命名。
- 尊重 `NO_PROXY` / 系统代理；企业证书通过平台证书存储验证。

### 4.6 Verifier

- artifact：大小 + SHA-256。
- manifest：仓库签名 + 哈希链。
- tree：逐文件 SHA-256 + 规范化树摘要。
- 可选：Authenticode（`WinVerifyTrust`）、provenance（in-toto/SLSA）、SBOM。
- 失败分类：网络截断 vs 哈希不匹配 vs 签名无效，给出不同修复建议。

### 4.7 Store

- 文件级 CAS：`store/objects/<sha256>`。
- 树清单：`store/trees/<digest>.json`。
- 硬链接农场：activation 时从 object 硬链接到 generation。
- 只读标记；`kita verify` 可全量校验。
- GC：以所有 generation + 当前 active + 保留的 rollback 为根做 mark-sweep。

### 4.8 Execution Engine

- 将 Plan 编译为有序 `Step`。
- 每个 Step 有 `precondition` / `execute` / `undo`。
- 维护 operation journal（SQLite + 临时 JSON），进程崩溃后可恢复。
- 支持 `--dry-run`、`--json`、取消（Ctrl+C）。
- 对不可逆步骤（原生安装器）在计划中显式标注。

### 4.9 Windows Platform Layer

- 路径规范化与长路径（`\\?\`）。
- 硬链接、junction、文件锁、Restart Manager。
- Shim 生成与启动。
- MSI 检测、安装、卸载、产品码解析。
- UAC 提权代理。
- PATH / 环境变量的读写（仅显式操作）。
- 所有 `unsafe` 集中在此 crate。

### 4.10 State

- SQLite（WAL）保存索引与审计数据。
- `active.json` 是运行时真相；SQLite 是查询缓存和审计日志。
- 启动时 reconcile：如果 active 指向不存在的 generation，降级到上一个有效 generation 并报错。
- 不对用户暴露 SQL；提供 `kita log` / `kita doctor`。

## 5. 目录布局

### 5.1 用户级（默认）

```text
%APPDATA%\Kita\config.toml              # 小型、可漫游的配置
%LOCALAPPDATA%\Kita\
├─ active.json                          # 原子指针 + 工具表 + 环境变量
├─ state.db                             # SQLite（WAL）
├─ state.lock                           # 排他锁（写操作）
├─ store\
│  ├─ objects\<sha256>                  # 不可变文件对象
│  └─ trees\<tree-sha256>.json          # 树清单
├─ envs\default\
│  ├─ generations\
│  │  ├─ 41\
│  │  │  ├─ generation.json
│  │  │  ├─ READY
│  │  │  └─ tree\<instance>\...          # 硬链接
│  │  └─ 42\...
│  └─ current                          # 内容为 "42" 的文本文件，调试用；运行时不依赖
├─ shims\
│  ├─ kita-shim.exe                     # 共享的 launcher 二进制
│  ├─ rg.exe                            # 同一二进制的硬链接（回退：复制）
│  └─ tools.json                        # 工具名 -> 目标（仅调试/审计）
├─ cache\
│  ├─ index\                            # 压缩索引 + ETag
│  └─ downloads\                        # artifact 缓存
├─ logs\
│  └─ operations-2026-11.jsonl
├─ tmp\                                 # 仅在安装期间存在
└─ trash\                               # 延迟删除，GC 清空
```

### 5.2 机器级（显式 `scope = "machine"`）

```text
%ProgramFiles%\Kita\                    # 机器级 root；ACL：用户读/执行，管理员写
├─ active.json
├─ state.db
├─ store\...
├─ envs\default\...
├─ bin\                                 # 机器级 shim
│  ├─ kita-shim.exe
│  ├─ rg.exe
│  └─ ...
└─ ...

%ProgramData%\Kita\
├─ config.toml                          # 机器配置
└─ data\<package-id>\                   # 可变包数据 / native 应用数据
```

- 机器级 shim 目录 `%ProgramFiles%\Kita\bin` 写入系统 PATH；用户级 shim 目录写入用户 PATH。
- shim 通过自身路径推导 root：用户级 `...\Kita\shims\rg.exe` → `...\Kita`；机器级 `...\Kita\bin\rg.exe` → `...\Kita`。
- 用户 PATH 通常排在系统 PATH 前面，因此同名工具由用户级优先。`kita doctor` 会报告冲突。
- 机器级 root 的 ACL：用户只读 + 执行，管理员/系统可写；提权代理是唯一写入者。
- 可变数据放在 `%ProgramData%\Kita\data`，避免普通用户对 `%ProgramFiles%` 的写入。

### 5.3 项目目录

```text
repo/
├─ kita.toml
├─ kita.lock
└─ .kita/                # 可选的本地状态（不入库）
   ├─ plan.json
   └─ logs/
```

Kita 不把项目依赖装进项目目录；项目只是声明，安装进用户/机器 scope。这样工具对所有目录可用。需要完全隔离时使用独立 `KITA_ROOT` 或 bundle。

## 6. 运行时真相与一致性

### 6.1 `active.json`

```json
{
  "schema": 1,
  "scope": "user",
  "generation": 42,
  "generation_path": "envs/default/generations/42",
  "platform": "x86_64-pc-windows-msvc",
  "activated_at": "2026-11-09T08:31:22Z",
  "tools": {
    "rg": {
      "package": "kita/ripgrep",
      "version": "14.1.1",
      "origin": "managed",
      "root": "tree/kita_ripgrep_14.1.1_a1b2",
      "path": "rg.exe",
      "env": {},
      "kind": "console"
    }
  },
  "env": {
    "KITA_ACTIVE": "42"
  }
}
```

- 写入方式：写临时文件 → `FlushFileBuffers` → `ReplaceFileW`（保留备份）。
- shim 启动时读取这个文件；不查 SQLite。
- `generation_path` 是相对 root 的路径，避免盘符变化导致失效。
- `tools` 在生成 generation 时完整构建；回滚只需替换 `active.json`。

### 6.2 SQLite

主要表：

```sql
repositories(id, url, kind, key_fingerprint, trusted_at, last_sync);
generations(id, env, generation, manifest_digest, status, created_at);
instances(env, generation, instance_key, package_id, version, activation, scope, tree_digest);
artifacts(sha256, size, media_type, source_url, verified_at);
receipts(instance_key, kind, product_code, install_dir, uninstall_json);
operations(id, command, plan_digest, state, started_at, finished_at, error_code);
shims(name, instance_key, kind, updated_at);
settings(key, value);
```

约束：

- 所有写操作在 `state.lock` 排他锁内完成；读操作可并发。
- 每个 generation 的 `instances` 一旦写入不可变；回滚不修改历史行。
- `operations` 用于崩溃恢复和审计；计划哈希必须与执行时一致。
- SQLite 损坏不致命：`kita doctor --rebuild-state` 扫描 `envs/` 重建。

### 6.3 锁与并发

- 两个 `kita sync` 不能同时写：第二个进程等待或在 `--no-wait` 时失败。
- shim 运行不获取写锁；它只读 `active.json`。
- 更新正在运行的工具：在 Windows 上，硬链接目标被占用时通常仍可切换（旧 generation 保留）；删除旧 generation 前用 Restart Manager 检查占用。
- `kita gc` 跳过被占用文件，延迟到 `trash`。

## 7. 分层与 crate

```text
apps/
├─ kita/                 # CLI 二进制
├─ kita-elevated/        # 提权代理（无 UI 复用；仅执行已校验计划）
└─ kita-shim/            # shim launcher（console + GUI 两种构建）

crates/
├─ kita-core/            # 领域类型、Plan、错误、纯逻辑
├─ kita-manifest/        # TOML schema、规范化、校验
├─ kita-registry/        # 仓库客户端、索引、签名验证
├─ kita-resolver/        # PubGrub provider 与版本求解
├─ kita-store/           # CAS、树、GC、硬链接
├─ kita-app/             # use case 编排，不依赖 CLI
├─ kita-install/         # 执行引擎、journal、portable/MSI
├─ kita-win/             # 所有 Win32/unsafe 封装
├─ kita-bundle/          # 离线 bundle
└─ kita-testkit/         # 测试仓库、fixture、属性测试
```

依赖方向：

```text
kita (bin)
 └─ kita-app
     ├─ kita-install ──┬─ kita-store
     │                 ├─ kita-registry
     │                 ├─ kita-resolver
     │                 ├─ kita-manifest
     │                 └─ kita-win
     ├─ kita-bundle
     └─ kita-core (all)
```

规则：

- `kita-core` 不依赖 `tokio`、`reqwest`、`windows`。
- `kita-manifest` 只依赖 `serde` / `toml` / `kita-core`。
- `kita-win` 不包含产品策略；只提供安全的、明确的平台操作。
- `kita-install` 不直接调用 Win32，通过 `kita-win` 的 trait 注入，便于测试。
- 禁止循环依赖；用 `cargo deny` 检查。

## 8. 平台抽象

```rust
pub trait Platform: Send + Sync {
    fn root(&self) -> &Path;
    fn same_volume(&self, a: &Path, b: &Path) -> bool;
    fn hard_link(&self, from: &Path, to: &Path) -> Result<()>;
    fn replace_file(&self, src: &Path, dst: &Path) -> Result<()>;
    fn is_file_in_use(&self, path: &Path) -> Result<bool>;
    fn close_holders(&self, paths: &[PathBuf], ui: &dyn Ui) -> Result<HolderAction>;
    fn verify_authenticode(&self, path: &Path) -> Result<SignatureInfo>;
    fn elevate(&self, plan: &ElevationRequest) -> Result<ElevationResult>;
    fn read_user_path(&self) -> Result<Vec<PathBuf>>;
    fn write_user_path(&self, value: &[PathBuf]) -> Result<()>;
}
```

测试时使用 `FakePlatform`（内存文件系统或 tempdir）验证事务逻辑；真正的 Win32 测试在 Windows CI / VM 中运行。

## 9. 进程模型

### 9.1 普通命令

```text
kita.exe
  ├─ 读取配置/清单
  ├─ 获取 state.lock（写命令）
  ├─ 解析/计划/执行
  └─ 退出
```

### 9.2 shim

```text
rg.exe (kita-shim)
  ├─ GetModuleFileNameW → 得到 "rg"
  ├─ 从自身路径推导 root（../）
  ├─ 读 active.json
  ├─ 查 tools["rg"] → 目标路径
  ├─ 构造环境变量（继承 + shim env）
  ├─ CreateProcessW（挂起）→ Job Object → ResumeThread
  ├─ 等待，转发退出码
  └─ Ctrl+C：共享控制台，终止 Job
```

- shim 不做网络、不做更新、不解析 TOML。
- shim 二进制由 Authenticode 签名；不依赖 .NET / PowerShell / cmd。
- GUI 应用使用 `kita-shimw.exe`（`windows` subsystem），避免弹出控制台窗口。

### 9.3 提权代理

```text
kita.exe (user)
  ├─ 构建 Plan，序列化为 plan.json，计算 plan_digest
  ├─ 将 plan.json 写入受 ACL 保护的临时目录
  ├─ ShellExecuteEx("runas", kita-elevated.exe --plan ... --nonce ...)
  └─ 等待命名管道返回结果

kita-elevated.exe (admin)
  ├─ 校验 plan 来源 ACL、nonce、plan_digest
  ├─ 重新验证计划只包含允许的机器级操作
  ├─ 执行；写 journal
  └─ 返回结构化结果
```

详见 [安装模型](04-install-model.md#8-提权模型)。

## 10. 失败与恢复

| 故障 | 行为 |
| --- | --- |
| 下载中断 | `.part` 保留；重试/续传；active 不变 |
| 哈希不匹配 | 删除坏文件；尝试镜像；全部失败则停止 |
| 解压失败 | 清理临时树；active 不变 |
| 磁盘满 | 预检查 + 明确错误；清理临时文件 |
| 文件被占用 | 提示进程列表；可选择关闭（Restart Manager）或取消 |
| 进程崩溃在 activation 中 | 启动时发现无 READY 的 generation，清理；active 仍指向旧代 |
| 进程崩溃在 active 替换中 | `ReplaceFileW` 的备份可恢复；启动时 reconcile |
| MSI 失败 | 依赖 Windows Installer 回滚；Kita 恢复 receipt 和 active |
| MSI undo 失败 | 保留 journal，`kita repair` 提供手工步骤 |
| 磁盘上的 active.json 损坏 | 扫描最近有效 generation，恢复到只读安全状态并报告 |

### 10.1 Journal

- 临时 journal 写在 `tmp/op-<uuid>.json`，每步 `fsync`。
- 步骤粒度：`Download`、`Extract`、`Link`、`MsiInstall`、`WritePath`、`Activate`。
- 每个可逆步骤记录 inverse；不可逆步骤记录 `irreversible_reason`。
- 成功完成后，journal 归档到 `logs/` 并从 tmp 删除。
- 启动时若发现未完成 journal：
  - 未到 `Activate`：自动 undo 并清理；
  - 已到 `Activate`：完成审计并清理；
  - 原生安装器不确定：进入 `kita repair` 指引，不自动删除任何文件。

## 11. 性能架构

- **冷启动**：不加载完整仓库索引；CLI 只读配置与本地状态。
- **索引**：按包懒加载；catalog 只用于搜索。
- **下载**：8 路并发，每路独立重试；大文件可选 Range。
- **解压**：流式写入临时对象，边解压边哈希；CPU 工作放到 blocking pool。
- **激活**：硬链接 O(files)，通常 < 100 ms；不复制数据。
- **diff**：按 tree digest / instance key 比较，不做全盘扫描。
- **搜索**：catalog 本地二分 + 前缀匹配；不启动模糊搜索引擎。
- **数据库**：WAL，索引覆盖常用查询；不在事务中做网络 I/O。

## 12. 可观测性

默认不收集遥测。用户显式开启后只发送：

- 命令名、耗时、是否成功、错误码；
- Kita 版本、Windows 版本、架构；
- 不发送包名、路径、用户名、仓库 URL。

本地日志：

- `logs/operations-YYYY-MM.jsonl`：每次变更一行，含 plan digest、步骤、结果。
- `kita log --json` 可导出给支持团队。
- 日志默认保留 30 天；`kita log prune` 手动清理。

## 13. 架构决策记录（ADR）

| 编号 | 决策 | 理由 |
| --- | --- | --- |
| ADR-001 | 静态 HTTPS 仓库，无必需服务端 | CDN 友好、可镜像、可离线；把复杂度放在客户端 |
| ADR-002 | TOML 作为清单/锁文件格式 | 人类可读、注释友好、Rust 生态成熟 |
| ADR-003 | SHA-256 而非自创哈希 | 广泛工具支持、硬件加速、供应链可验证 |
| ADR-004 | 文件级 CAS + 硬链接 | 跨版本去重；失败可回滚；不复制大文件 |
| ADR-005 | `active.json` 作为运行时真相 | shim 读取快；回滚是单文件原子替换 |
| ADR-006 | 不执行任意安装脚本 | 供应链风险不可接受；MSI/便携包覆盖大多数场景 |
| ADR-007 | Shim 而非 PATH 目录 | PATH 稳定、版本切换无感、便于注入环境 |
| ADR-008 | 用户级默认 | 降低权限面；符合开发者机器现实 |
| ADR-009 | MSI 一等公民，EXE 白名单 | 兼顾覆盖率和可逆性 |
| ADR-010 | 单环境 v1 | shim 语义简单；多环境留到 v1.x |
