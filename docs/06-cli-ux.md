# 06 · CLI 与用户体验

Kita 的 CLI 遵循三条规则：

1. **每个变更都能先看计划。**
2. **默认不交互的脚本场景必须显式 `--yes`，不会被默认确认坑。**
3. **人类输出好看，机器输出稳定；两者是同一份数据。**

## 1. 命令面

```text
kita <command> [options]

环境
  init              创建 kita.toml，配置 shell 集成
  install           有参数：添加并安装；无参数：sync
  add               添加包（修改清单 + 安装）
  remove            移除包（修改清单 + 收敛）
  sync              将机器收敛到清单/锁文件
  lock              只解析并生成 kita.lock，不安装
  update            在约束内更新依赖并收敛
  plan              展示将要发生的变更，不执行
  list              列出已安装包
  search            搜索仓库
  info              查看包详情
  why               解释为什么安装了某个包
  diff              比较清单、锁、已安装状态

运行
  run               使用 Kita 环境运行某个工具
  exec              使用 Kita 环境运行任意命令
  shell             启动注入了 Kita 环境的子 shell
  env               输出环境变量（人类/JSON）

状态
  rollback          切换到上一个 generation
  generation        列出/固定/清理 generation
  verify            校验文件、锁、shim
  doctor            诊断并修复本地状态
  gc                回收无引用对象
  cache             查看/清理缓存
  log               查看操作日志

仓库
  repo              添加/移除/信任/刷新仓库
  auth              登录私有仓库

分发
  bundle            创建/安装离线 bundle
  self              更新 Kita 自身
  completions       生成 shell 补全
  version           版本信息
```

`install` 有两种形态：有包参数时等价于 `add`（修改清单 + 安装）；无参数时等价于 `sync`（严格按清单/锁收敛）。参数是 `.kta` 文件路径时，直接读取容器内的 `manifest.toml` 与 `payload/` 激活该包，不修改 `kita.toml`，属于一次性安装；`.kta` 参数不能与包名混用。`uninstall` 是 `remove` 的别名。它们不引入第二套状态语义。

## 2. 全局选项

```text
--root <PATH>          覆盖 Kita root（CI/测试）
--scope <user|machine> 覆盖默认 scope
--json                 输出单个 JSON 文档（隐含 --no-progress）
--jsonl                输出 JSONL 事件流（隐含 --no-progress）
--no-progress          不显示进度条
--color <auto|always|never>
--no-unicode           使用 ASCII 符号
--quiet                只输出错误和最终结果
--verbose, -v          增加诊断（可重复）
--offline              禁止网络；只使用缓存/bundle
--yes, -y              对非交互确认回答 yes
--dry-run              只计划，不执行（所有变更命令）
--no-wait              状态锁被占用时立即失败
--config <PATH>        覆盖用户配置路径
```

安全相关的例外：

- `--yes` **不能**跳过第三方仓库的首次指纹确认。
- `--yes` **不能**绕过 critical revocation。
- `--yes` 不能把 `scope=machine` 变成静默提权；UAC 仍然出现。
- `--dry-run` 不获取 state 写锁，不下载，不执行 MSI。

## 3. 核心流程

### 3.1 `kita init`

```console
$ kita init
Created kita.toml
Created kita.lock

Kita 需要把 shim 目录加入用户 PATH：
  + %LOCALAPPDATA%\Kita\shims

当前 PATH 的前几项：
  C:\Windows\System32
  C:\Program Files\Git\cmd
  ...

插入到最前面？ [Y/n] y
✓ PATH 已更新（仅用户级；需要新开终端）

Next:
  kita add ripgrep
  kita sync
```

- 检测已有 `kita.toml`，不覆盖，除非 `--force`。
- 不自动写 PowerShell profile；只提示可选的 `kita shell` 用法。
- `--no-path` 跳过 PATH 修改，打印手动命令。
- 不创建全局配置文件，除非用户运行 `config set`。

### 3.2 `kita add`

```console
$ kita add ripgrep fd bat
```

规格语法：

```text
<id>[@<requirement>]
```

示例：

```text
ripgrep                  # 未指定版本：写入 "*"，锁到当时最新稳定版
ripgrep@14.1             # caret：>=14.1, <15
kita/ripgrep@=14.1.1     # 精确
kita/cmake@">=3.28,<4"   # 范围（需要引号避免 shell 解释）
```

流程：

1. 读取当前清单与锁。
2. 解析新图。
3. 生成计划并展示。
4. 确认后：写清单 → 写锁 → 下载 → 激活。
5. 如果所有步骤失败，清单也不应留下半截状态：先写临时清单，成功激活后原子替换。

输出：

```console
$ kita add ripgrep

Plan: add 1 package to generation 41
  + kita/ripgrep 14.1.1
      provides   rg, ripgrep
      download   3.0 MB (zip)
      scope      user
      activation shim

  PATH 会新增 2 个 shim：rg, ripgrep
  No elevation required.

Continue? [Y/n] y

  Resolve    ✓ 38 ms
  Download   ✓ 1/1 (3.0 MB, 0.4 s)
  Verify     ✓ sha256 3d7f...a91c
  Activate   ✓ generation 42
  Done.

Try: rg --version
```

选项：

| 选项 | 说明 |
| --- | --- |
| `--version <req>` | 显式版本需求，避免 `@` 解析歧义 |
| `--scope <user\|machine>` | 单包 scope；machine 仅原生包 |
| `--source <repo>` | 指定已信任仓库 |
| `--pre` | 允许选择 prerelease（仍受版本需求约束） |
| `--no-sync` | 只改清单/锁，不安装 |
| `--dry-run` | 展示计划，不写任何文件 |
| `--yes` | 跳过确认（不跳过信任/UAC） |

### 3.2.1 多包事务摘要

`kita install git nvim fastfetch` 会先展示一张事务表，再进入执行阶段：

```console
$ kita install git nvim fastfetch

Repository  kita/stable
Platform    x86_64-pc-windows-msvc

Transaction
  Requested
    kita/git           2.43.0    win64    45 MB
    kita/nvim          0.10.0    win64    32 MB
    kita/fastfetch     2.8.4     win64     8 MB
  Dependencies
    kita/libcurl       8.5.0     win64    12 MB
    kita/luajit        2.1.0     win64     1 MB
    kita/libuv         1.48.0    win64     2 MB
    kita/ripgrep       14.1.0    win64     3 MB

  Total              7 packages
  Download           103 MB (cache 0%)
  Installed          245 MB
  Keyring            ✓ 7/7
  File conflicts     ✓ none
  Elevation          none (all user scope)

Proceed? [Y/n] y

  Resolve    ✓ 52 ms
  Download   ✓ 7/7
  Verify     ✓ sha256 / signatures
  Activate   ✓ generation 42
  Integrate  ✓ shims: git, nvim, fastfetch, rg
  Done in 4.1 s
```

说明：

- “Requested”与“Dependencies”来自解析图，不靠安装器事后猜测。
- Keyring 检查是仓库签名/密钥有效性检查，不是系统钥匙串。
- File conflicts 在计划阶段完成：命令名、shim 名、包内路径大小写冲突。
- **没有 daemon。** 认证、提权和事务都由单次 CLI 进程 + 一次性 broker 完成；用户级安装不会启动常驻服务。
- 这张表是 `kita plan --json` 的人类渲染，字段与 JSON 一一对应。

### 3.3 `kita sync`

```console
$ kita sync
```

- 有锁文件：默认按锁安装（只下载缺失/损坏的），但仍校验清单 digest。
- 锁与清单不一致：
  - 交互式：提示“清单已变化，是否重新解析？”；
  - `--locked`：失败，退出码 7；
  - `--yes`：重新解析。
- `--locked` 保证不改变锁文件，适合 CI。
- `--frozen` 等价于 `--locked --offline`，适合 air-gap。
- 无变化：输出 `Already up to date (generation 42).`，退出 0，不创建新 generation。

`kita lock` 只做解析并写入 `kita.lock`，不下载、不激活，适合在 PR 中先审查解析结果。`kita lock --check` 只校验现有锁是否与清单一致，不写文件。

### 3.4 `kita plan`

```console
$ kita plan
```

输出的是**人类可读的计划**，不执行任何副作用：

```text
Environment  user / default
Generation   42 → 43

Packages
  + kita/bat        0.24.0    portable   user
  ~ kita/ripgrep    14.1.1 → 14.1.2  portable  user
  - kita/fd         10.2.0    portable   user

Downloads
  2 artifacts, 6.1 MB (cache hit: 3.0 MB)

Native installers
  (none)

Elevation
  (none)

Reversibility
  full — portable packages only

Warnings
  ! kita/ripgrep 14.1.2 需要 Windows 10 19041+
```

`--json` 输出完整 Plan：

```json
{
  "schema": 1,
  "command": "plan",
  "environment": "default",
  "scope": "user",
  "generation": { "from": 42, "to": 43 },
  "actions": [
    { "op": "add", "id": "kita/bat", "version": "0.24.0", "scope": "user" },
    { "op": "update", "id": "kita/ripgrep", "from": "14.1.1", "to": "14.1.2" },
    { "op": "remove", "id": "kita/fd", "from": "10.2.0" }
  ],
  "downloads": { "count": 2, "bytes": 6100000, "cached_bytes": 3000000 },
  "elevation": { "required": false, "operations": [] },
  "reversibility": "full",
  "warnings": []
}
```

### 3.5 `kita update`

```console
$ kita update
$ kita update ripgrep fd
$ kita update --major
```

- 默认：在清单约束内更新所有根包及其依赖。
- 不修改 `kita.toml`。
- `--major`：允许跨主版本，并**同时修改清单中的 caret 约束**，计划里显示旧/新约束；需要确认。
- `--pre`：允许 prerelease，仅对明确允许的包生效。
- `--dry-run`：只展示。
- `--locked` 不适用于 update（矛盾时直接报错）。
- 更新失败自动回到原 generation。

### 3.6 `kita remove`

```console
$ kita remove fd
```

- 修改清单并重新解析；如果 fd 仍是其他包的依赖，会保留为依赖并提示。
- 默认保留数据目录。
- `--purge` 删除 `${KITA_DATA}/fd`。
- 不直接删除文件；生成新 generation，保留回滚窗口。

### 3.7 日常查看

```console
$ kita list
NAME        VERSION   SCOPE  SOURCE  STATUS
bat         0.24.0    user   kita    active
fd          10.2.0    user   kita    active
ripgrep     14.1.1    user   kita    active
node        22.12.0   user   kita    active

$ kita list --outdated
NAME        CURRENT   LATEST   CONSTRAINT
ripgrep     14.1.1    14.1.2   ^14.1

$ kita search grep
kita/ripgrep   14.1.1   面向行的递归搜索工具        [grep, search, cli]
kita/ast-grep  0.30.1   基于 AST 的代码搜索          [grep, ast, code]

$ kita info kita/ripgrep
```

`kita info` 展示：

- ID、版本、summary、license、homepage、source；
- 发布者、签名状态、provenance；
- 可用平台、下载大小；
- 暴露的命令；
- 依赖；
- capabilities；
- yank/revocation 状态；
- 安装状态与 generation。

### 3.8 `kita why`

```console
$ kita why ripgrep
kita/ripgrep 14.1.1
  ← 直接依赖（kita.toml）
  ← 没有其他包依赖它
```

或：

```console
$ kita why npm
kita/npm 10.9.0
  ← kita/node 22.12.0
  ← 直接依赖（kita.toml）
```

### 3.9 `kita diff`

比较三者：清单 / 锁 / 已安装。

```console
$ kita diff
manifest ≠ lock
  + kita/bat  (manifest)
  - kita/fd   (lock)

lock = active
  (no changes)
```

### 3.10 运行工具

```console
$ kita run rg -- --version
$ kita run --list
$ kita exec -- pwsh -NoProfile
$ kita shell
```

- `kita run <tool>` 从当前工具表解析，不依赖 shim，适合 CI。
- `kita exec -- <cmd>` 注入 Kita 环境，但不改变 PATH 之外的系统状态。
- `kita shell` 启动 `pwsh`（可配置），注入环境；退出后不留痕。
- `kita env --json` 输出可供其他工具消费的环境：

```json
{
  "schema": 1,
  "root": "C:\\Users\\me\\AppData\\Local\\Kita",
  "generation": 42,
  "path_prepend": ["C:\\Users\\me\\AppData\\Local\\Kita\\shims"],
  "env": { "EDITOR": "hx", "KITA_ACTIVE_GENERATION": "42" }
}
```

## 4. 回滚与状态命令

### 4.1 `kita rollback`

```console
$ kita rollback
$ kita rollback 2        # 回退两个 generation
$ kita rollback --files-only
$ kita rollback --with-native
```

- 默认回退一个 generation。
- 纯便携包：秒级切换。
- 含 native 包：默认拒绝自动切换，提示选择 `--files-only` 或 `--with-native`。
- `--with-native` 需要 UAC，可能较慢；计划中显示将重装的 MSI。
- 回滚本身写审计日志。

### 4.2 `kita generation`

```console
$ kita generation list
GEN  CREATED              PACKAGES  REVERSIBILITY  PINNED
42   2026-11-09 16:31     12        full           no
41   2026-11-08 10:02     11        full           no
40   2026-11-07 09:12     11        installer      yes

$ kita generation pin 40
$ kita generation unpin 40
```

### 4.3 `kita verify`

```console
$ kita verify
✓ active.json schema
✓ generation 42 READY
✓ 1,284 files match store objects
✓ 12 shims match
✓ lock matches active generation

$ kita verify --all
$ kita verify --shims
$ kita verify --locked
```

失败时退出码 5，并列出第一个坏对象与修复建议。

### 4.4 `kita doctor`

```console
$ kita doctor
Kita 0.1.0 · Windows 11 26100 · x86_64

✓ state lock available
✓ root ACL
✓ active.json valid
✓ shim directory on PATH
! shim shadowed: rg (C:\Program Files\Git\usr\bin\rg.exe appears earlier)
! 2 stale staging directories (12 MB)

修复建议：
  kita doctor --fix        # 清理 staging，重建 shim
  kita doctor --path       # 调整 PATH 顺序
```

- `--fix` 只做安全、可逆的修复。
- 不自动删除数据、不自动重装 MSI、不自动改 PATH 顺序（除非 `--path` 且确认）。
- `--json` 输出机器可读的诊断项。

### 4.5 `kita gc` / `cache`

```console
$ kita gc --dry-run
Would free 412 MB:
  store objects   388 MB (1,204 files)
  old generations  24 MB (2 generations)

$ kita cache clean --older-than 30d
```

## 5. 仓库与认证

```console
$ kita repo add internal https://packages.example.com/kita/v1
$ kita repo list
$ kita repo refresh
$ kita repo remove internal

$ kita auth login internal
Token: ********
✓ stored in Windows Credential Manager
```

`repo remove`：

- 如果有锁文件引用该仓库的包，拒绝，除非 `--force`。
- 不删除已安装包与缓存；只移除信任和元数据。

## 6. 离线 bundle

```console
$ kita bundle create --output team-bundle
  Resolve    ✓
  Fetch      ✓ 12 artifacts (48 MB)
  Sign       ✓ minisign (optional)
  Wrote      team-bundle\
              bundle.toml
              bundle.toml.minisig
              blobs\sha256\...
              metadata\...

$ kita bundle install .\team-bundle --root D:\Kita --yes
```

- bundle 是自包含的：锁文件、manifest、索引、artifact、签名。
- 默认包含锁文件对应的平台；`--platform` 可指定。
- 安装时仍做完整哈希/签名校验。
- 可加密？v1 不做；用 7z/BitLocker 包装。

## 7. 输出与退出码

### 7.1 人类输出

- 阶段化：`Resolve → Plan → Fetch → Verify → Activate`。
- 进度条只在 TTY 显示；非 TTY 自动关闭。
- 颜色只用于强调，不承载唯一信息。
- 符号在 `--no-unicode` 下退化为 ASCII：`+ ~ - ! x`.
- 错误信息结构：

```text
error[KITA-3007]: 无法满足版本约束
  kita/node 22.12.0 需要 kita/npm ^10.2
  kita/pnpm 9.15.0 需要 kita/npm >=11

help: 升级 pnpm 到 10.x，或使用 `kita add pnpm@10 --dry-run` 查看影响
docs: https://kita.dev/errors/KITA-3007
```

### 7.2 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 成功（包括“无变更”） |
| `1` | 一般错误 |
| `2` | 用法/参数错误 |
| `3` | 解析冲突 |
| `4` | 网络/仓库不可达 |
| `5` | 校验失败（哈希/签名/状态） |
| `6` | 权限/提权失败 |
| `7` | 锁文件与清单不一致 / frozen |
| `8` | 原生安装器失败或部分完成 |
| `9` | 平台/架构不支持 |
| `10` | 本地状态损坏，需要 doctor |

CI 只应依赖这些码，不应解析人类文本。

### 7.3 `--json`

- 单个 JSON 文档写到 stdout；日志/诊断到 stderr。
- 错误也输出 JSON 文档（带 `error` 字段），退出码非零。
- schema 字段随版本演进；破坏性变更递增。
- 示例错误：

```json
{
  "schema": 1,
  "command": "sync",
  "error": {
    "code": "KITA-5003",
    "message": "artifact 哈希不匹配",
    "package": "kita/ripgrep",
    "expected": "sha256:...",
    "actual": "sha256:...",
    "hints": ["删除缓存后重试：kita cache clean --package kita/ripgrep"]
  }
}
```

### 7.4 `--jsonl`

用于长任务：

```json
{"schema":1,"event":"phase","phase":"resolve","status":"started"}
{"schema":1,"event":"progress","phase":"download","done":1,"total":3,"bytes":3045678}
{"schema":1,"event":"warning","code":"KITA-4102","message":"镜像回退"}
{"schema":1,"event":"phase","phase":"activate","status":"completed"}
{"schema":1,"event":"result","ok":true,"generation":42}
```

事件只追加，不修改；最后一行为 `result` 或 `error`。

## 8. 配置

### 8.1 文件位置

| 范围 | 路径 |
| --- | --- |
| 用户配置 | `%APPDATA%\Kita\config.toml` |
| 用户数据 | `%LOCALAPPDATA%\Kita` |
| 机器配置 | `%ProgramData%\Kita\config.toml`（只读） |
| 项目清单 | `./kita.toml` |
| 环境覆盖 | `KITA_ROOT`、`KITA_CONFIG`、`KITA_NO_COLOR`、`KITA_TOKEN_<REPO>` |

### 8.2 示例

```toml
schema = 1

[general]
default_scope = "user"
confirm = true
color = "auto"

[network]
concurrency = 8
timeout_seconds = 30
retries = 3

[storage]
keep_generations = 5
cache_days = 14
gc_threshold_mb = 2048

[security]
require_signatures = true          # 不允许关闭
allow_http = false                 # 仅测试环境可改，启动时警告
repo_fingerprints = ["sha256:..."]

[telemetry]
enabled = false

[self_update]
enabled = true
channel = "stable"
```

### 8.3 优先级

```text
CLI 参数 > 环境变量 > 项目清单 > 用户配置 > 机器配置 > 默认值
```

项目清单只能影响包与环境变量，**不能**关闭签名校验、修改仓库信任、开启脚本执行或允许 HTTP。

## 9. Shell 集成

- `kita completions pwsh|bash|zsh|fish|elvish`。
- `kita shell` 注入：
  - `PATH`（shim 目录在前）；
  - `KITA_ROOT`；
  - `KITA_ACTIVE_GENERATION`；
  - 清单 `[env]` 与包 env。
- 不自动修改 PowerShell `$PROFILE`。
- `kita env` 可被 direnv 类工具调用；输出稳定 JSON。
- 在 CI 中推荐 `kita exec -- <command>`，避免依赖 shell 配置。

## 10. 可访问性与国际化

- 所有状态同时有文字，不仅靠颜色/符号。
- 尊重 `NO_COLOR`、`CLICOLOR=0`、`TERM=dumb`。
- 终端宽度 < 60 时切换为单列布局。
- 日期使用本地时区显示，机器输出用 RFC 3339 UTC。
- v1 界面中文/英文；错误码稳定，文案可翻译。
- 不假设终端是 UTF-8：`--no-unicode` 时全部 ASCII。

## 11. 反模式

- 不在 `kita add` 中静默安装依赖的 MSI。
- 不在 `sync` 中自动更新清单约束。
- 不在非 TTY 环境中等待输入。
- 不用随机成功文案掩盖部分失败。
- 不在 JSON 输出中混入进度条。
- 不把 UAC 当作普通确认。
- 不自动重启被关闭的应用。
- 不在命令失败后留下“清单已改、锁未改”的半状态。
