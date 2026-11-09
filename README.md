# Kita
<<<<<<< HEAD

**Windows, as declared.**
**把你的 Windows 变成一份可执行、可复现、可回滚的配置。**

Kita 是一个用 Rust 编写的、面向 Windows 的声明式包管理器。它不是另一个“下载器 + PATH 修改器”，而是把一台 Windows 机器当成一个可以 `plan / apply / rollback` 的构建产物。

> 状态：**设计稿 v0.1 + 本地安装实现**（2026-11-09）。当前代码实现了「本地静态仓库 + portable zip」的完整闭环：`kita.toml` → 解析 → 锁文件 → CAS → generation → shim。HTTP 仓库、MSI、签名与 GC 仍未实现。本环境未安装 Rust 工具链，代码**未经编译验证**。

---

## 一分钟理解 Kita

```toml
# kita.toml —— 你提交进 dotfiles / 仓库的文件
schema = 1

[environment]
name = "dev"
scope = "user"

[packages]
ripgrep = "14.1"
fd = "10"
bat = "0.24"
node = "22"
```

```console
$ kita sync
  Resolve  ✓ 4 packages in 41 ms
  Plan     + ripgrep 14.1.1   + fd 10.2.0   + bat 0.24.0   + node 22.12.0
           + ripgrep will provide rg, ripgrep
  Fetch    ✓ 4 artifacts, 18.4 MB (cache 61%)
  Activate ✓ generation 42
  Done in 1.8 s

$ kita rollback
  Switched generation 42 → 41. 其余包未变化。
```

Kita 的核心循环只有五步：

```text
manifest ──resolve──▶ lockfile ──plan──▶ fetch ──verify──▶ activate generation
                                                                    │
                                                              rollback ◀┘
```

## Kita 的不同之处

| 能力 | winget | Scoop | Chocolatey | **Kita** |
| --- | --- | --- | --- | --- |
| 声明式环境 + 锁文件 | 部分（Export / DSC） | 清单，无锁文件 | 包脚本 | **原生 `kita.toml` + `kita.lock`** |
| 可复现安装 | 弱 | 弱 | 弱 | **强（版本 + 哈希 + 生成号）** |
| 原子切换 / 一键回滚 | 无 | 无 | 无 | **代际激活** |
| 默认免管理员 | 是 | 是 | 否 | **是** |
| 原生安装器（MSI/EXE） | 强 | 弱 | 强 | **MSI 为一等公民，EXE 需白名单配方** |
| 内容去重 | 无 | 无 | 无 | **内容寻址存储 + 硬链接** |
| 默认执行包脚本 | 否 | 是 | 是 | **否，脚本默认禁用** |
| 需要常驻后台服务 | 否 | 否 | 否 | **否** |

Kita 不试图取代 Windows Update，也不做图形应用商店。它解决的是一个更具体的问题：

> **让 Windows 开发环境、团队机器和 CI 机器，像代码一样被声明、审查、复现和回滚。**

## 设计原则

1. **声明，而不是修改。** 用户编辑 `kita.toml`；Kita 负责收敛，而不是让用户记住一串命令。
2. **先计划，后执行。** 每个会改变系统的操作都能 `plan`，计划可读、可 JSON 化、可审查。
3. **用户空间优先。** 默认无需管理员；提权是显式、窄范围、可审计的例外。
4. **一个 PATH 入口。** 不把几十个目录塞进 PATH；通过 shim 分发工具。
5. **可验证的信任。** 签名索引、哈希锁定、默认不执行脚本；每个字节都能追溯。
6. **Windows 不是 Unix。** 原生处理 MSI、ACL、UAC、长路径、junction、文件占用和 ARM64。
7. **快，来自减法。** 无守护进程、无后台更新器、无网络也能 `list` 和 `run`。
8. **错误是产品的一部分。** 版本冲突要说清“为什么”，而不只是“解不出来”。

## 文档地图

| 文档 | 内容 |
| --- | --- |
| [产品定位](docs/01-product.md) | 用户、场景、竞品、原则、非目标、成功指标 |
| [系统架构](docs/02-architecture.md) | 组件、数据流、crate 分层、目录布局、状态模型 |
| [包模型](docs/03-package-model.md) | `kita.toml`、`kita.lock`、包清单、版本与依赖语义 |
| [安装模型](docs/04-install-model.md) | CAS、硬链接、代际、shim、原生安装器、提权、回滚 |
| [仓库与安全](docs/05-repository-security.md) | 静态仓库协议、签名、威胁模型、供应链 |
| [CLI 与 UX](docs/06-cli-ux.md) | 命令面、输出、退出码、错误、JSON 事件 |
| [Rust 实现](docs/07-rust-implementation.md) | workspace、模块 API、依赖选型、测试、性能 |
| [路线图](docs/08-roadmap.md) | 里程碑、风险、开放问题 |
| [设计对齐](docs/09-design-alignment.md) | 与 pacman 风格示例日志的取舍（daemon、PATH、hooks、`.kta`） |

## 示例

- [`examples/kita.toml`](examples/kita.toml)：项目声明
- [`examples/kita.lock`](examples/kita.lock)：锁文件
- [`examples/packages/ripgrep.toml`](examples/packages/ripgrep.toml)：portable 包清单
- [`examples/packages/example-msi.toml`](examples/packages/example-msi.toml)：MSI 包清单
- [`examples/repository.toml`](examples/repository.toml)：仓库元数据
- [`examples/local-repo/`](examples/local-repo)：可直接安装的本地示例仓库（`kita/hello`，portable zip）

## 已实现：本地包安装

当前实现覆盖 `docs/08-roadmap.md` 中 Phase 0/1 的本地部分：

- Rust workspace：`kita-core`（库）+ `kita`（CLI）+ `kita-shim` / `kita-shimw`（shim）
- 版本、版本需求（caret/tilde/通配/比较符/`||`）、包 ID、路径安全校验
- `kita.toml` 规范化与 canonical digest、`kita.lock` 读写
- 本地仓库：`repository.toml` + `packages/<ns>/<name>/index.toml` + 清单 + artifact
- 朴素解析器：按 ID 顺序选择最高匹配版本，检测命令冲突；不做回溯
- CAS：`store/objects/<sha256>` 与 `store/trees/<digest>.json`
- generation：`envs/<env>/generations/<n>/`，`generation.json` + `READY` + 硬链接树
- `active.json` 原子写入，shim 每次启动读取；`kita rollback` 只切换指针
- CLI：`init` / `add` / `remove` / `sync` / `plan` / `list` / `rollback` / `generation list` / `run` / `env` / `verify`

### 构建与试用

```powershell
cargo build
cargo test

# 使用仓库内的示例本地仓库
cargo run -p kita-cli -- init --repository examples/local-repo
cargo run -p kita-cli -- add kita/hello
cargo run -p kita-cli -- list
cargo run -p kita-cli -- run hello -- kita

# 将优先级最高的 shims 目录加入用户 PATH 后，可直接运行 hello
# 默认位置：%LOCALAPPDATA%\Kita\shims
```

本地仓库布局：

```text
<repo>/
├─ repository.toml                      # schema = 1, kind = "local"
├─ packages/kita/hello/index.toml
├─ packages/kita/hello/1.0.0/manifest.toml
└─ blobs/hello-1.0.0.zip
```

artifact 的 `url` 可以是仓库相对路径，也可以是 `file://` 绝对 URL；HTTP/HTTPS 会明确报“不支持”。

### 命令速览

| 命令 | 作用 |
| --- | --- |
| `kita install [SPEC...]` | 无参数时等同 `sync`；包名等同 `add`；`.kta` 文件直接安装该容器 |
| `kita lock [--check]` | 生成锁文件并补齐 CAS；`--check` 只校验不写入 |
| `kita update [--dry-run]` | 忽略现有锁文件，按 `kita.toml` 重新解析并同步 |
| `kita search <QUERY>` | 在本地仓库中按包 ID / 摘要搜索 |
| `kita info <PACKAGE>` | 显示包摘要、许可、命令、依赖与版本列表 |
| `kita why <PACKAGE\|COMMAND>` | 解释包或命令为何出现在当前环境，并给出依赖链 |
| `kita diff` | 对比 `kita.toml`、`kita.lock` 与活动 generation |
| `kita exec <PROGRAM> [ARGS...]` | 在注入活动环境变量的进程中运行命令 |
| `kita shell [--powershell]` | 打开注入了活动环境的 cmd / PowerShell |
| `kita doctor [--fix]` | 检查根目录、仓库、活动代际、CAS 与 shim；`--fix` 重建 shim |
| `kita gc [--dry-run]` | 删除 CAS 中未被 active/previous/pin/锁文件引用的对象 |
| `kita cache list\|clean [--all]` | 查看或清理下载缓存；默认保留锁文件引用的 artifact |
| `kita generation pin\|unpin <N>` | 固定或取消固定某个 generation |
| `kita generation gc` | 删除未激活、非上一个、未固定的 generation |

所有变更类命令（`install`、`add`、`remove`、`sync`、`update`）都会先输出 pacman 风格的事务摘要并请求确认；`--noconfirm` 可跳过确认。`--json` 输出始终是单一 JSON 文档，不包含任何装饰。

`kita install <文件.kta>` 直接读取容器根目录的 `manifest.toml`，把 `payload/` 解包进 CAS 并激活，不需要先放入仓库；这类本地包不写入 `kita.toml`，属于一次性安装，后续 `kita sync` / `kita update` 会按清单将其移除。`.kta` 参数不能与仓库包名混用。

### 当前限制

- 只支持 Windows portable zip 与 `.kta` 容器；MSI/native 安装未实现。
- 依赖解析是确定性的贪心算法，**不回溯**；复杂冲突会报 `KITA-3004`。
- 没有 HTTP 仓库、签名/索引验证、bundle 与 `self update`。
- `gc` 需要存在活动环境；它保留 active、previous、`generation pin` 以及锁文件引用的内容。
- `cache clean` 默认按 artifact 的 sha256 保留锁文件引用项，`--all` 才全部清理。
- `init` 不会修改 PATH，只打印需要手动加入的 shim 目录。
- 锁文件中的树摘要与命令列表来自本地安装结果，不校验仓库签名。
- 代码按设计文档编写，但当前环境没有 Rust 工具链，**未编译、未运行测试**。

## 名字

Kita 读作 `/ˈkiːtə/`。取 “kit”（工具组合）与 “a”（一个起点）的合成感：一套属于你的 Windows 工具，而不是一堆散落的安装包。

---

设计稿采用 [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) 授权；未来的代码计划采用 MIT OR Apache-2.0 双许可。
=======
A package manager running on Windows
>>>>>>> 72007fbacd405f55b6e52caa3a3ddaa9f4f06e2e
