# 09 · 设计对齐与取舍

工作区里有一份示例安装日志 `example·.log`，展示了一种更接近 Arch/pacman 的 Kita：`kita install git nvim fastfetch`、`.kta` 包、keyring、依赖表、`kita-daemon`、`k-sudo`、按应用注册 PATH，以及 Plasma/Dolphin/Konsole 的 post-transaction hooks。

它是重要的 UX 输入。本文记录哪些采纳、哪些刻意不采纳，避免实现时出现两套互相冲突的语义。

> 说明：`example·.log` 是工作区中的既有示例文件，不是本设计稿的一部分，也不代表已经实现的代码。

## 1. 采纳

| 日志中的行为 | 本设计 |
| --- | --- |
| `kita install <pkg...>` | 采纳。`install` 有参数时等价于 `add`；无参数时等价于 `sync`。 |
| 先展示依赖与事务摘要，再执行 | 采纳。`Plan` 区分 Requested / Dependencies，渲染为事务表。 |
| keyring / 签名检查 | 采纳为仓库签名验证与密钥固定；“keyring”是本设计中的仓库密钥环，不是系统钥匙串。 |
| 文件冲突检查 | 采纳。命令名、shim 名、包内路径大小写冲突在计划阶段失败。 |
| `.kta` 包 | 采纳为可选的单文件分发容器（见包模型 §4.10）；不替代签名索引。 |
| 用户级安装跳过提权 | 采纳。默认 user scope，不触发 UAC。 |
| 多包事务一次完成 | 采纳。一个计划、一个 generation、一次激活。 |

## 2. 刻意不采纳

### 2.1 `kita-daemon`

日志里出现：

```text
[Kita] Authenticating via kita-daemon...
```

本设计**不引入常驻 daemon**：

- 没有需要长期持有的会话；
- 认证用 Windows Credential Manager；
- 提权用一次性 `kita-elevated.exe`（用户可见别名 `k-sudo.exe`）；
- 事务在单次 CLI 进程内完成，崩溃恢复靠 journal，而不是后台进程；
- 减少攻击面、安装复杂度、企业部署阻力和“为什么有个后台服务”的疑问。

如果未来需要跨进程协调，优先使用命名管道 + 一次性 broker，而不是常驻 daemon。

### 2.2 按应用注册 PATH

日志里：

```text
Registering PATH variables: C:\Kita\apps\git\bin, C:\Kita\apps\nvim\bin, ...
```

本设计改为：

- 只有一个 shim 目录进入 PATH；
- 每个命令是一个硬链接的 launcher；
- 版本切换只改 `active.json`，不改 PATH；
- 避免 PATH 顺序、重复项、卸载残留和长度限制。

代价是 shim 启动开销（预算 < 15 ms）和需要对 GUI/console 分别处理。这是有意的取舍。

### 2.3 任意 post-transaction hook

日志里的 5 个 hook（PATH、快捷方式、Dolphin 菜单、默认终端、二进制缓存）如果做成任意脚本，会破坏可复现性和供应链安全。

本设计的替代：

- **v1**：shim + `kita env`；不写全局关联。
- **v1.x**：声明式 `[[integration]]`，由引擎执行、进入计划、可逆、可审计：
  - `shortcut`（开始菜单/桌面）
  - `context-menu`（资源管理器）
  - `file-association`（受限）
  - `terminal-profile`
  - `path-entry`（默认禁止，需显式 capability）
- 每个 integration 都是数据，不是命令；安装器不执行任意 PowerShell。
- KDE/Plasma/Dolphin/Konsole 集成作为可选的 shell integration 插件，而不是核心依赖。

### 2.4 `C:\Kita\apps` 作为安装根

本设计的默认根：

```text
%LOCALAPPDATA%\Kita        # 用户级
%ProgramFiles%\Kita        # 机器级
```

原因：

- 用户级无需管理员；
- 与 Windows 的 LocalAppData / Program Files 约定一致；
- 机器级 ACL 可以收紧；
- 避免在系统盘根目录创建可写目录（`C:\Kita` 的 ACL 往往过宽）。

如果用户需要固定路径，可用 `KITA_ROOT` 覆盖；CI 用临时 root。

## 3. 与日志输出风格的映射

日志中的 pacman 风格输出可以保留为**人类可读事务摘要**，但底层仍使用 Kita 的声明式模型：

| 日志阶段 | Kita 对应 |
| --- | --- |
| Synchronizing package databases | 刷新仓库元数据 + 签名验证 |
| Resolving primary packages | Resolver + Plan（Requested） |
| Missing dependencies detected | Resolver + Plan（Dependencies） |
| Downloading dependencies | Fetcher（并行、哈希校验） |
| Preparing transaction | Plan（无副作用） |
| Checking keys / file conflicts | 签名链验证 + shim/路径冲突检查 |
| Processing package changes | CAS 写入 + generation 构建 + 激活 |
| Running post-transaction hooks | 声明式 integrations（未来） |
| Complete! | generation 激活 + 审计记录 |

## 4. 术语

| 日志用词 | 本设计用词 | 说明 |
| --- | --- | --- |
| primary packages | Requested | 用户直接请求的根包 |
| dependencies | Dependencies | 解析出的依赖 |
| `.kta` | `.kta` container | 可选单文件容器 |
| keyring | repository keyring | 仓库签名密钥环 |
| kita-daemon | — | 不实现 |
| k-sudo | `kita-elevated.exe` 的别名 | 一次性提权代理 |
| hook | integration | 声明式、可逆的系统集成 |

## 5. 结论

这份设计保留了示例日志中最好的部分：**一次事务、清晰的依赖表、多包安装、可审计的签名检查**。它去掉了与“声明式、可回滚、无守护进程”冲突的部分：**常驻 daemon、任意 hook、按包改 PATH**。

如果未来产品决定走“pacman 风格 + KDE 深度集成”的路线，应该把它作为**另一个前端/发行版**，而不是把两套模型混进同一个核心。
