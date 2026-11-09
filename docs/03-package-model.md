# 03 · 包模型与文件格式

Kita 有三个核心文件，职责严格分离：

| 文件 | 谁写 | 作用 | 是否入库 |
| --- | --- | --- | --- |
| `kita.toml` | 人 | 声明想要的环境 | 是 |
| `kita.lock` | Kita | 固定解析结果与哈希 | 是 |
| `package.toml` | 包维护者 | 描述一个包的某个版本 | 仓库 |

原则：

- **清单表达意图，锁文件表达事实。**
- 锁文件不包含时间戳、绝对路径、用户名、机器名、凭据。
- 包清单不包含命令式安装脚本。
- 所有未知字段都报错，避免“写了但没生效”。

## 1. 项目清单 `kita.toml`

### 1.1 完整示例

```toml
schema = 1

[environment]
name = "dev"
scope = "user"                 # user | machine，默认 user
platform = "x86_64-pc-windows-msvc"

[packages]
ripgrep = "14.1"
fd = "10"
bat = "0.24"
node = "22"

# 带选项的写法
"kita/cmake" = { version = ">=3.28, <4", scope = "user" }
"example/contoso-agent" = { version = "2.4", scope = "machine" }

[env]
EDITOR = "hx"
PAGER = "bat"
```

### 1.2 顶层字段

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `schema` | integer | 是 | 当前为 `1`；不兼容变更时递增 |
| `environment` | table | 否 | 环境元数据 |
| `packages` | table | 是 | 包 ID → 版本需求或选项 |
| `env` | table | 否 | 注入到 shim / `kita shell` 的环境变量 |

未知顶层字段直接报 `KITA-1002`。不使用 `[hooks]`、`[scripts]`、`[install]` 这类项目级执行入口。

### 1.3 `[environment]`

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `name` | `"default"` | 人类可读标签；v1 不用于隔离状态 |
| `scope` | `"user"` | 默认安装 scope；可被单包覆盖 |
| `platform` | 当前主机 | 交叉计划用；必须匹配 `目标三元组` 格式 |
| `channel` | `"stable"` | 仓库通道；v1 官方仓库仅 stable/beta |

`platform` 主要用于 CI 生成/验证锁文件。实际安装时如果不等于主机目标，`sync` 拒绝执行，除非 `--plan-only`。

### 1.4 `[packages]`

值是以下两种之一：

```toml
ripgrep = "14.1"                                   # 版本需求字符串
"kita/cmake" = { version = ">=3.28, <4" }          # 选项表
```

选项表字段：

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `version` | 必填 | 版本需求；见 §3 |
| `scope` | 环境默认 | `user` 或 `machine`；`machine` 仅允许原生安装器或显式系统集成 |
| `source` | `"kita"` | 已信任的仓库 ID |

包 ID 规则：

- 规范形式：`<namespace>/<name>`；省略 namespace 时默认 `kita`。
- namespace 与 name 各自匹配 `[a-z0-9][a-z0-9._-]{0,63}`。
- 全部转为小写存储；拒绝非 ASCII，避免 Windows 大小写折叠陷阱。
- 显示名可以与规范 ID 不同（如 `Microsoft.VisualStudioCode` 映射为 `microsoft/visual-studio-code`），但锁文件只用规范 ID。

### 1.5 `[env]`

```toml
[env]
EDITOR = "hx"
KITA_LOG = "info"
```

规则：

- 值必须是字符串，不支持列表/数字的隐式转换。
- 不支持命令替换、`$()`、反引号。
- 用户级 `[env]` 只注入到 Kita 启动的进程（shim、`kita run`、`kita shell`），**不永久写用户环境变量**。
- 与系统已有变量同名时，Kita 的值优先；`kita run --inherit-env` 可反转。
- 以 `KITA_` 开头的变量名保留给 Kita，用户设置会报错。

### 1.6 项目清单的规范化

为了检测“清单改了但锁文件没更新”，Kita 对清单做**语义规范化**：

1. 解析为强类型结构；
2. 补默认值；
3. 包按规范 ID 排序，env 按 key 排序；
4. 序列化为 canonical TOML；
5. 计算 SHA-256，写入锁文件的 `manifest` 字段。

因此注释、空白、字段顺序不影响 digest；但任何语义变化都会触发 `--locked` 失败。

## 2. 锁文件 `kita.lock`

### 2.1 完整示例

```toml
# 由 Kita 生成；请勿手工编辑。
schema = 1
resolver = "1"
platform = "x86_64-pc-windows-msvc"
manifest = "0000000000000000000000000000000000000000000000000000000000000000"

[[package]]
id = "kita/bat"
version = "0.24.0"
source = "kita"
scope = "user"
root = true
activation = "shim"
artifact = { target = "x86_64-pc-windows-msvc", sha256 = "1111111111111111111111111111111111111111111111111111111111111111", size = 3_112_345 }
tree = "2222222222222222222222222222222222222222222222222222222222222222"
manifest_digest = "3333333333333333333333333333333333333333333333333333333333333333"
bins = ["bat"]
dependencies = []

[[package]]
id = "kita/fd"
version = "10.2.0"
source = "kita"
scope = "user"
root = true
activation = "shim"
artifact = { target = "x86_64-pc-windows-msvc", sha256 = "4444444444444444444444444444444444444444444444444444444444444444", size = 2_987_654 }
tree = "5555555555555555555555555555555555555555555555555555555555555555"
manifest_digest = "6666666666666666666666666666666666666666666666666666666666666666"
bins = ["fd", "fdx"]
dependencies = []

[[package]]
id = "kita/ripgrep"
version = "14.1.1"
source = "kita"
scope = "user"
root = true
activation = "shim"
artifact = { target = "x86_64-pc-windows-msvc", sha256 = "7777777777777777777777777777777777777777777777777777777777777777", size = 3_045_678 }
tree = "8888888888888888888888888888888888888888888888888888888888888888"
manifest_digest = "9999999999999999999999999999999999999999999999999999999999999999"
bins = ["rg", "ripgrep"]
dependencies = []
```

### 2.2 字段语义

| 字段 | 说明 |
| --- | --- |
| `schema` | 锁文件 schema |
| `resolver` | 解析器语义版本；改变求解规则时递增 |
| `platform` | 锁定的目标三元组；跨平台锁不共享 |
| `manifest` | 项目清单的规范化 digest |
| `package.id` | 规范包 ID |
| `package.version` | 精确版本 |
| `package.source` | 仓库 ID；防止跨仓库混淆 |
| `package.scope` | `user` / `machine` |
| `package.root` | 是否由项目清单直接请求（依赖为 `false`） |
| `package.activation` | `shim` / `native` / `none` |
| `package.artifact` | 选中的 artifact：target / sha256 / size |
| `package.tree` | 解压后的树摘要；原生包为空 |
| `package.manifest_digest` | 仓库中包清单原始字节的 SHA-256 |
| `package.bins` | 暴露的命令名（排序） |
| `package.dependencies` | 精确依赖，格式 `id@version`，排序 |

规则：

- 包按 `id` 升序排列；依赖按字符串升序。
- 不记录下载 URL：URL 可能变化，哈希才是身份。
- 不记录 generation 号：generation 是本机状态，不属于可移植的锁。
- 不记录原生安装路径：路径由 receipt 记录，锁只记录包身份。
- 同一包 ID 在锁中最多出现一次（v1 不支持同包多版本并存）。
- 锁文件一旦生成，手改会被 `kita sync` 的 digest 校验发现。

### 2.3 Digest 表示

- TOML 中的所有 SHA-256 字段使用 **64 位小写十六进制**，不带 `sha256:` 前缀。
- 人类输出、错误信息与 JSON 展开时允许加 `sha256:` 前缀以增强可读性。
- 示例为简洁省略了部分字符；真实锁文件必须是完整 64 位。


### 2.4 锁文件的信任

锁文件本身不签名；它通过以下方式获得信任：

1. 进入 Git，接受 code review；
2. 每个包的 `manifest_digest` 能在一个已签名索引中找到；
3. 每个 artifact 的 `sha256` 被签名清单固定；
4. `kita sync --locked` 不重新解析，只按锁下载；
5. `kita verify --locked` 校验本地 tree 与锁一致。

如果锁中的包已被 yank/revoke：

- 已安装：保留，但 `kita verify` / `kita update` 告警。
- 新安装：默认拒绝，`--allow-yanked` 只对已锁定的精确版本生效。
- 被撤销（恶意/泄露）：默认拒绝，即使已锁定；需要 `--allow-revoked` 并显示强警告。

## 3. 版本与需求语义

### 3.1 为什么不用严格 SemVer

Windows 软件常见版本：

- `1.2.3.4`（四段）
- `2024.10`
- `6.3.9600.0`
- `1.0.0-beta.2`
- `1.2.3.4.5`（少见但存在）

严格 SemVer 会拒绝这些版本。Kita 定义 **Windows Version**：

```text
version   = numeric ( "." numeric ){0,7} [ "-" prerelease ]
numeric   = "0" | [1-9] digit*
prerelease= 1*( alphanum | "." | "-" )
```

排序规则：

1. 按数字段从左到右比较，缺失段视为 0（`1.2 == 1.2.0`）。
2. 有 prerelease 的版本小于同基线的正式版。
3. prerelease 段按点分隔；数字段按数值比较，字母段按 ASCII 比较；数字段小于字母段（SemVer 规则）。
4. 不允许前导零（避免 `01` 与 `1` 的歧义）。
5. 解析时去除末尾的零段（至少保留一段），因此 `1.2.0.0` 的规范形式是 `1.2`；锁文件与显示均使用规范形式。这样相等、排序与哈希保持一致。
6. v1 不支持 `+build`；出现即校验失败。Windows 版本几乎不使用 build metadata，拒绝它可避免“显示相同但排序/相等不同”的歧义。

### 3.2 需求语法

| 写法 | 含义 |
| --- | --- |
| `*` | 任意稳定版 |
| `latest` | 最新稳定版（解析时确定，锁定时固定） |
| `=1.2.3` | 精确等于 |
| `1.2.3` | **caret**：`>=1.2.3, <2.0.0`（若主版本为 0，则 `<0.3.0` 等按 Cargo 规则） |
| `^1.2` | 同上 |
| `~1.2` | `>=1.2.0, <1.3.0` |
| `1.2.*` | `>=1.2.0, <1.3.0` |
| `>=1.2, <2` | 交集 |
| `1.2 \|\| 2.0` | 并集 |
| `>=1.2.0-beta.1` | 允许该基线的 prerelease |

预发布版本默认排除，除非：

- 需求中显式包含同一基线的 prerelease；或
- 清单设置 `allow-prerelease = true`（v1 不开放，避免误用）；或
- 使用 `--pre` 且需求不是精确 pin。

`latest` 与 `*` 只在解析时生效；一旦锁文件存在，`sync --locked` 以锁为准，不会因为上游发布新版本而漂移。只有 `update` 或重新 lock 才会重新解释它们。

### 3.3 解析策略

PubGrub 产生一个满足所有约束的解。多个解时按以下优先级选择：

1. 已安装且满足约束的版本（减少无谓变更）；
2. 稳定版优先于 prerelease；
3. 版本高者优先；
4. 包 ID 字典序小者优先（保证确定性）；
5. 依赖更少者优先（tie-break）。

`kita update` 忽略第 1 条，但仍受清单约束。`kita update --major` 会**修改清单约束**并在计划中显式标出。

### 3.4 冲突解释

失败时必须给出可行动的 derivation，而不是“无法解析”：

```text
error[KITA-3007]: 无法同时满足以下版本要求

  kita/node 22.12.0 依赖 kita/npm ^10.2
  kita/pnpm 9.15.0 依赖 kita/npm >=11

  kita/npm 没有同时满足 ^10.2 和 >=11 的版本。

建议：
  - 将 pnpm 升级到 10.x（支持 npm 10）
  - 或运行 kita add pnpm@10 --dry-run 查看变化
  - 或使用独立环境（v1 暂不支持，见 roadmap）
```

## 4. 包清单 `package.toml`

包清单描述一个包的**一个版本**。它由仓库提供，被签名索引引用；安装时由哈希固定。仓库中通常命名为 `manifest.toml`；本文示例使用 `packages/ripgrep.toml` 只是为了可读性，文件名不参与身份。

### 4.1 Portable 包示例

```toml
schema = 1

id = "kita/ripgrep"
version = "14.1.1"
summary = "面向行的递归搜索工具"
description = "ripgrep 递归搜索目录中的正则表达式，尊重 .gitignore。"
license = "MIT OR Apache-2.0"
homepage = "https://github.com/BurntSushi/ripgrep"
upstream = "https://github.com/BurntSushi/ripgrep"

[platform]
os = ">=10.0.19041"
arch = ["x86_64", "aarch64"]

[install]
mode = "portable"

[capabilities]
elevation = "never"
network = false
registry = false
services = false

[[artifact]]
target = "x86_64-pc-windows-msvc"
url = "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-x86_64-pc-windows-msvc.zip"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
size = 3045678
format = "zip"
strip_components = 1

[[artifact]]
target = "aarch64-pc-windows-msvc"
url = "https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/ripgrep-14.1.1-aarch64-pc-windows-msvc.zip"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
size = 2987654
format = "zip"
strip_components = 1

[[bin]]
name = "rg"
path = "rg.exe"
kind = "console"
aliases = ["ripgrep"]

[[bin]]
name = "rga"
path = "rga.exe"
kind = "console"
```

> 示例中的哈希为占位值，仅用于展示格式；真实清单必须使用实际 SHA-256。

### 4.2 顶层字段

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `schema` | 是 | `1` |
| `id` | 是 | 规范包 ID |
| `version` | 是 | Windows Version |
| `summary` | 是 | ≤ 120 字符，搜索与列表展示 |
| `description` | 否 | 长描述，纯文本或受限 Markdown |
| `license` | 是 | SPDX 表达式 |
| `homepage` | 是 | HTTPS |
| `upstream` | 否 | 上游源码仓库，便于审计 |
| `platform` | 是 | OS/架构约束 |
| `install` | 是 | 安装方式 |
| `capabilities` | 是 | 声明的系统能力 |
| `artifact` | 是 | 至少一个 artifact |
| `bin` | 否 | 暴露的命令 |

禁止字段：`post_install`、`pre_install`、`scripts`、`powershell`、`cmd`、`registry`（作为任意写入）。出现即校验失败。

### 4.3 `[platform]`

```toml
[platform]
os = ">=10.0.19041"
arch = ["x86_64", "aarch64"]
```

- `os` 使用 Windows 版本比较；当前支持 `>=`、`<`、`=` 与交集。
- `arch` 必须与 artifact 的 `target` 一致；用于早期给出“没有可用构建”的清晰错误。
- 不支持 `x86`（v1 不提供 32 位 shim）。

### 4.4 `[install]`

#### Portable

```toml
[install]
mode = "portable"
strip_components = 1     # 可选，artifact 也可各自覆盖
mutable = false          # true：复制到 generation，允许写入安装目录
```

- 解压到 CAS，文件进入不可变树。
- 默认使用硬链接和只读对象；`mutable = true` 时改为复制，允许应用在安装目录写文件（代价是失去跨版本去重）。
- 不支持安装脚本。
- 包必须在解压后立即包含所有运行所需文件；长期数据应写到 `${KITA_DATA}`。

#### Native

```toml
[install]
mode = "native"
kind = "msi"
scope = "machine"
upgrade_code = "{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}"
properties = { ALLUSERS = "1", MSIFASTINSTALL = "7" }
```

| 字段 | 说明 |
| --- | --- |
| `kind` | v1 仅 `"msi"` |
| `scope` | `machine` / `user`；MSI 的 `ALLUSERS` 由此推导 |
| `upgrade_code` | MSI UpgradeCode，用于检测已安装版本；强烈建议提供 |
| `product_code` | 可选；固定 ProductCode 的 MSI 可直接精确检测 |
| `properties` | 传给 MSI 的公共属性；白名单校验，不允许任意命令行 |
| `silent_args` | 不开放；由 Kita 统一生成 `/qn /norestart` |

MSI 的校验、提权与回滚见 [安装模型](04-install-model.md#6-原生安装器msi)。

#### 为什么没有 EXE 安装器

EXE 安装器（Inno、NSIS、自定义）没有统一的静默/回滚/检测协议。v1 不执行它们。后续通过“白名单配方”支持：

```toml
# 未来格式，不属于 v1
[install]
mode = "native"
kind = "recipe"
recipe = "inno"
```

即使将来支持，也会标注 `reversible = best_effort`，并在计划中展示完整命令行。

#### 依赖

```toml
[dependencies]
"kita/7zip" = ">=23"

# 只作为运行库安装，不把它的命令放进 PATH
"kita/runtime" = { version = "^1", bins = false }
```

- 依赖默认也暴露自己的 `[[bin]]`；`bins = false` 可抑制。
- v1 每个包 ID 只能解析出一个版本；冲突时给出 derivation，不自动并存。
- 依赖环由求解器拒绝，并显示环路径。
- 依赖必须与根包来自同一仓库（除非包的 `source` 显式指定）。

### 4.5 `[capabilities]`

```toml
[capabilities]
elevation = "never"      # never | optional | required
network = false          # 安装/运行是否需要网络（信息性）
registry = false         # 是否写注册表（native 专用）
services = false         # 是否安装服务（native 专用）
```

规则：

- portable 包必须 `elevation = "never"`、`registry = false`、`services = false`。
- `network` 是信息性声明，用于计划提示；Kita 不据此断网。
- `elevation = "required"` 的原生包在用户 scope 下直接校验失败。
- 能力声明不是沙箱；它是**审查提示**，不是强制隔离。

### 4.6 `[[bin]]`

```toml
[[bin]]
name = "rg"
path = "rg.exe"
kind = "console"           # console | gui
aliases = ["ripgrep"]
env = { "RIPGREP_CONFIG_PATH" = "${KITA_DATA}/ripgreprc" }
```

| 字段 | 说明 |
| --- | --- |
| `name` | 暴露的命令名，必须匹配 `[a-z0-9][a-z0-9._-]{0,63}`，不含 `.exe` |
| `path` | 相对包根；portable 必须在树中存在；native 可含 `${INSTALL_DIR}` |
| `kind` | `console` 使用 `kita-shim.exe`；`gui` 使用 `kita-shimw.exe` |
| `aliases` | 额外命令名；冲突时整体失败 |
| `env` | 仅对该命令注入的变量；模板规则见 §4.7 / §4.8 |

### 4.7 `[env]`

```toml
[env]
"RIPGREP_CONFIG_PATH" = "${KITA_DATA}/ripgreprc"
```

- 包级 env 注入到该包的所有 shim。
- 模板变量只有：`${KITA_ROOT}`、`${KITA_DATA}`、`${KITA_PACKAGE}`、`${KITA_GENERATION}`、`${INSTALL_DIR}`（仅 native）。
- 不做命令替换；未知变量校验失败。
- `${KITA_DATA}` 指向 `%LOCALAPPDATA%\Kita\data\<package-id>`（用户级）或 `%ProgramData%\Kita\data\<package-id>`（机器级），首次安装时创建。
- 卸载时默认保留 data；`kita remove --purge` 才删除。

### 4.8 模板与路径

- 所有路径分隔符统一为 `\` 或 `/`；Kita 内部转换为 NT 路径。
- 不允许 `..` 逃逸包根；不允许绝对路径（除 native 的 `${INSTALL_DIR}` 前缀）。
- `path` 不区分大小写；校验重复。
- 不允许 ADS（`:`）、设备名（`CON`、`NUL`、`COM1`…）、结尾空格/点。
- 单文件路径长度上限 32,767 字符（Windows 长路径），但 Kita 对超过 260 字符的路径给出警告。

### 4.9 Artifact

```toml
[[artifact]]
target = "x86_64-pc-windows-msvc"
url = "https://..."
sha256 = "..."
size = 3045678
format = "zip"
strip_components = 1
```

| 字段 | 说明 |
| --- | --- |
| `target` | Rust 风格目标三元组；当前支持 `x86_64-pc-windows-msvc`、`aarch64-pc-windows-msvc` |
| `url` | HTTPS；允许重定向，但最终 URL 也必须是 HTTPS |
| `sha256` | 64 位小写 hex；必填 |
| `size` | 字节数；必填，用于预检查和截断检测 |
| `format` | portable：`zip`、`tar.zst`（v1），`7z`、`tar.gz`、`kta` 可选；native：`msi` |
| `strip_components` | 去顶层目录层数；默认 0 |
| `publisher` | 可选；期望的 Authenticode 主体，例如 `CN=Contoso Ltd, O=Contoso Ltd, C=US` |
| `require_signature` | 可选；为 `true` 时 Authenticode 验证失败即拒绝 |

约束：

- 同一 target 只能有一个 artifact。
- `format = "msi"` 只能与 `install.mode = "native"` 一起使用。
- `format = "kta"` 是 Kita 自描述容器（见 §4.10）；其中的 `manifest.toml` 必须与仓库清单一致。
- 所有 `[[bin]]` 的 `path` 必须在每个 portable artifact 中存在；安装前校验。
- 不支持无哈希的“最新版”下载。
- 不允许 `file://`（本地仓库测试除外，需显式 `--allow-local`）。
- 不支持需要交互式登录的 URL；私有仓库通过仓库级认证，不把 token 写进清单。

### 4.10 `.kta` 容器

`.kta` 是可选的单文件分发格式，用于离线包、内网镜像和“一个文件带走一个包”：

```text
ripgrep-14.1.1-win64.kta        # zip 容器
├─ manifest.toml                # 与该版本的仓库清单一致
├─ payload/
│  └─ rg.exe ...
└─ META-INF/
   └─ manifest.sha256           # payload 的树摘要（可选）
```

规则：

- 整个 `.kta` 文件仍有 artifact 级 SHA-256；下载后先校验整包哈希。
- 解包后校验 `manifest.toml` 与索引中的 manifest digest 一致；不一致拒绝。
- 容器内不允许脚本/hook；仍按 portable 的路径安全规则解压。
- `.kta` 不替代签名索引：它只是一个传输容器。第三方 `.kta` 必须通过仓库签名或被用户显式信任。
- v1 可以先不实现 `.kta` 写入，只读取；生成器在 v1.1 提供。

## 5. 仓库索引格式

### 5.1 包索引（按包）

```toml
schema = 1
id = "kita/ripgrep"

[[version]]
version = "14.1.1"
manifest = "packages/kita/ripgrep/14.1.1/manifest.toml"
manifest_sha256 = "..."
targets = ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]
dependencies = {}
yanked = false

[[version]]
version = "13.0.0"
manifest = "packages/kita/ripgrep/13.0.0/manifest.toml"
manifest_sha256 = "..."
targets = ["x86_64-pc-windows-msvc"]
dependencies = {}
yanked = true
yanked_reason = "上游 13.0.0 在 ARM64 上崩溃"
```

- 解析器可以用索引中的 `dependencies` 完成求解，不必下载全部 manifest。
- `manifest_sha256` 固定完整清单；解析完成后只拉取被选中版本的 manifest。
- `yanked` 版本默认不参与新解析，但锁定的安装仍可用。

### 5.2 Catalog（搜索）

```toml
schema = 1
generation = 2026110901
updated_at = "2026-11-09T00:00:00Z"

[[package]]
id = "kita/ripgrep"
summary = "面向行的递归搜索工具"
latest = "14.1.1"
keywords = ["grep", "search", "cli"]
```

Catalog 只用于搜索和展示；**不能**作为解析依据。解析必须读包索引与 manifest。

## 6. 生命周期状态

```text
available ──resolve──▶ selected ──download──▶ verified ──activate──▶ installed
     ▲                    │                    │                      │
     │                    └──── yanked ────────┘                      │
     └────────────────────── removed ◀───────── uninstall ◀───────────┘
```

- `yanked`：不再被新解析选中；已锁定可安装，警告。
- `revoked`：安全撤销；即使锁定也默认拒绝。
- `deprecated`：仍可安装，但提示替代包（索引字段）。
- `removed`：从索引消失；锁文件中的精确安装仍可从缓存完成，否则失败。

## 7. 校验清单

包发布前 `kita pkg check` 必须通过：

1. Schema 版本正确，无未知字段。
2. ID/版本/许可证/主页合法。
3. artifact 至少一个，且覆盖声明的所有 arch。
4. 每个 artifact 的 SHA-256 与大小可从 URL 复现（维护者本地校验）。
5. 每个 bin 路径存在于每个 portable artifact。
6. 路径安全校验通过（无逃逸、无 ADS、无设备名、无大小写冲突）。
7. 依赖存在且可解析（在临时仓库中跑一次 `kita plan`）。
8. 能力声明与安装方式一致。
9. 没有禁止字段/脚本。
10. 包清单可通过 `kita pkg diff` 与上一版本比较，审核变更。
