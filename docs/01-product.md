# 01 · 产品定位

## 1. 一句话

Kita 是 Windows 的**声明式、事务型包管理器**：用一个可提交的清单描述机器状态，用锁文件固定解析结果，用代际切换保证每次变更都能回滚。

## 2. 问题

Windows 上的软件安装长期处在两种极端之间：

- **安装器编排器**（winget、Chocolatey）擅长处理原生安装包，但“装了什么、为什么装、能否重现”依赖导出文件和机器现状。升级通常不可逆，配置漂移难以发现。
- **便携包管理器**（Scoop）把文件放在用户目录，干净、免管理员，但缺少锁定、事务、原生安装器支持、供应链签名和原生 Windows 集成。
- **开发者的真实做法**往往是：一份 README + 若干脚本 + 手动下载 + 改 PATH。机器换了，环境就重来一遍。

同时，Windows 的安装现实并不会因为“声明式”而消失：

- 大量软件只提供 MSI/EXE 安装器，注册表、服务、COM、文件关联都是安装的一部分。
- 用户通常没有管理员权限，或者不希望每个工具都触发 UAC。
- 文件会被进程占用，更新不能粗暴覆盖。
- PATH、案例不敏感、长路径、保留文件名、junction 和 ACL 都是默认要处理的约束。

Kita 的切入点不是“再做一个包列表”，而是：

> **把可复现的声明式模型，和 Windows 原生安装现实，放进同一个事务里。**

## 3. 目标用户

### 3.1 开发者（首要）

- 用 dotfiles 或仓库管理多台 Windows 机器。
- 需要 node、python、rust、ripgrep、fd、bat、git、7zip 等工具的一致版本。
- 在 CI 或 Windows Sandbox 里需要可重复、无交互的安装。
- 希望 `kita.lock` 像 `Cargo.lock` 一样进入 code review。

### 3.2 团队 / 平台工程

- 新机器 onboarding 从半天变成一条 `kita sync --locked`。
- 统一工具版本，减少“我这里能跑”。
- 需要审计谁在什么时候装了什么、从哪个仓库、什么哈希。
- 需要离线包（air-gapped）与内部镜像。

### 3.3 高级用户

- 想用便携工具，但不想污染系统、不想手动维护 PATH。
- 想一键回滚一次失败的升级。
- 想看清楚一个安装器到底要改什么。

### 3.4 包维护者

- 用声明式清单描述包，而不是写命令式脚本。
- 在发布前本地验证哈希、架构、路径与依赖。
- 通过 Git / CI 审核包的变更。

## 4. 核心用户旅程

### 4.1 新机器

```console
$ winget install Kita.Kita        # 或从签名安装包安装
$ kita init
$ git clone https://github.com/me/dotfiles
$ cd dotfiles && kita sync --locked
```

结果：工具版本、shim、环境变量与锁文件完全一致；没有手工 PATH 编辑。

### 4.2 添加工具

```console
$ kita add ripgrep
```

Kita 展示计划（版本、大小、将暴露的命令、是否需要提权），确认后：

1. 解析依赖并更新 `kita.lock`；
2. 下载并校验；
3. 构建新 generation；
4. 原子切换；
5. 输出如何使用。

### 4.3 升级与回滚

```console
$ kita update
$ rg --version          # 发现行为异常
$ kita rollback         # 回到上一个 generation
$ kita pin ripgrep@14.1.1
```

回滚只切换指针；便携包不需要重新下载和安装。

### 4.4 团队复现

```console
$ kita sync --locked --offline    # 从本地 bundle/cache 安装
$ kita verify --all               # 校验每个文件对象
```

### 4.5 原生安装器

```console
$ kita add kita/7zip
```

Kita 显示：

```text
  Native installer (MSI)
  Scope        machine
  Publisher    7-Zip  ✓ Authenticode
  Will run     msiexec /i ... /qn /norestart
  Elevation    required (one UAC prompt)
  Reversible   installer (Windows Installer rollback / vendor uninstall)
```

Kita 不会假装 MSI 安装是原子的；它明确标注可逆性等级，并对可管理的部分（shim、PATH、receipt）做事务。

### 4.6 离线 / 内网

```console
$ kita bundle create --output team-bundle
$ kita bundle install team-bundle --root D:\Kita
```

Bundle 包含锁文件、解析所需的元数据、所有 artifact、校验和与签名，可在无网络机器上完整复现。

## 5. 产品原则（展开）

### P1 · 声明式，但允许逃生舱

主路径是编辑 `kita.toml`。也允许 `kita add` 这种便捷命令，但它必须**同时修改清单并安装**，不能制造“已安装但清单里没有”的漂移。对于确实需要命令式的场景，提供显式的 `kita run` / `kita exec`，而不是隐藏 hook。

### P2 · 计划是一等公民

任何变更都先产生 `Plan`：

- 人类可读的 diff；
- 稳定的 JSON；
- 明确标注 `reversible`、`requires_elevation`、`network_bytes`；
- 计划与执行使用同一个内存对象，避免“展示一套、执行另一套”。

### P3 · 默认用户空间

默认根目录在 `%LOCALAPPDATA%\Kita`，默认 scope 是 `user`。这带来：

- 无 UAC；
- 不污染系统 PATH；
- 多用户互不干扰；
- 卸载/回滚简单。

只有 `scope = "machine"` 的原生包或显式系统集成才走提权代理。

### P4 · 一个 PATH 入口，而不是 N 个

Kita 只把 `%LOCALAPPDATA%\Kita\shims` 放进用户 PATH。每个命令是一个签名的小型 launcher，它读取当前 generation 并启动真实可执行文件。这样：

- 安装/升级/回滚不需要重写 PATH；
- 不同版本的工具不会同时出现在 PATH；
- 目录名变化不会破坏调用方；
- 可以统一注入环境变量、工作目录和退出码。

### P5 · 事务的边界要诚实

- 便携包：文件系统级事务，失败自动回滚，成功可一键回滚。
- MSI：使用 Windows Installer 自身的事务与回滚；Kita 负责 receipt 和外围集成。
- EXE 安装器：默认不执行；只允许审核过的配方，并标注 `reversible = best_effort`。

不把“能卸载”宣传成“可原子回滚”。

### P6 · 供应链可验证

- 仓库索引签名；
- 每个 artifact 固定 SHA-256 与大小；
- 锁文件固定 manifest digest 和 tree digest；
- 可选校验 Authenticode、SLSA provenance、SBOM；
- 包脚本默认禁用；
- 提权计划有一次性 nonce，代理只执行计划中的操作。

### P7 · Windows 原生

Kita 使用 Win32 API，而不是用 Unix 心智模型硬套：

- 用 MSI API 检测/卸载，而不是猜注册表；
- 用 Restart Manager 检测文件占用；
- 用 Job Object 管理子进程；
- 处理 UAC、ACL、长路径、junction、案例不敏感、保留设备名；
- 同时支持 x64 与 ARM64。

### P8 · 快，来自减法

- 不做常驻服务；
- 不做后台自动更新；
- 不做隐式网络请求；
- 冷启动不加载全量索引；
- `list`、`run`、`env` 在离线时可用；
- 下载并行，解压流式，硬链接激活。

### P9 · 可互操作

- 能导入 winget / Scoop 清单（后续里程碑），但不依赖它们的运行时；
- 能管理 MSI，而不是要求上游重打包；
- 仓库是静态文件，能放在 GitHub Pages、S3、Nginx、内部制品库；
- 锁文件和清单是纯文本，能被其他工具读取。

## 6. 非目标（v1）

明确不做，避免产品失焦：

- **不是 Windows Update / 驱动管理器。** 不管理 OS 补丁、驱动、字体。
- **不是 GUI 应用商店。** v1 只有 CLI 和稳定的 JSON 接口；GUI 可以建立在 JSON 之上。
- **不做源码构建。** 不替代 vcpkg / Conan / cargo；只安装预构建产物。
- **不管理 Microsoft Store / MSIX 应用。** v1 的 native 模式只覆盖 MSI；Store 应用有自己的更新通道。
- **不做通用沙箱。** 不承诺把任意安装器关进容器；脚本执行默认禁用。
- **不做跨平台。** 先成为最好的 Windows 包管理器，再考虑其他系统。
- **不做常驻守护进程。** 不后台扫描、不自动更新、不上传包列表。
- **不追求“一个包支持所有架构”。** 没有对应 artifact 就明确失败，不猜测兼容。

## 7. 竞品与差异

| 维度 | winget | Scoop | Chocolatey | Kita |
| --- | --- | --- | --- | --- |
| 核心抽象 | 安装器目录 | 便携包 + bucket | 包 + PowerShell 脚本 | **环境 + 锁 + 代际** |
| 清单 | YAML（多文件） | JSON | nuspec + 脚本 | **TOML，单文件，严格 schema** |
| 锁定 | Export / DSC | 无 | 部分 | **`kita.lock` 原生** |
| 依赖解析 | 有限 | 有限 | 有限 | **PubGrub，可解释冲突** |
| 升级 | 逐包 | 逐包 | 逐包 | **计划式批量收敛** |
| 回滚 | 依赖安装器 | 重装 | 重装 | **代际指针，秒级** |
| 去重 | 否 | 否 | 否 | **CAS + 硬链接** |
| 脚本 | 否 | 是 | 是 | **默认禁用** |
| 原生安装器 | 强 | 弱 | 强 | **MSI 一等，EXE 白名单** |
| 用户态默认 | 是 | 是 | 否 | **是** |
| 签名/供应链 | 部分 | 哈希 | 校验和 | **签名索引 + 哈希 + 可选 provenance** |
| 离线 | 部分 | 部分 | 部分 | **bundle 原生** |

Kita 不假设自己会在“包的数量”上赢过 winget。它要在**可复现性、可审计性、回滚体验和开发者工作流**上赢。

## 8. 成功指标

### 8.1 产品指标

- 新机器 20 个常用工具，100 Mbps 网络下 `sync` < 2 分钟。
- 90% 的日常操作不需要管理员权限。
- `sync` 幂等：无变更时退出码 0，且不产生新 generation。
- 升级失败自动回滚成功率 > 99.9%。
- `kita plan` 对已安装状态的判断准确率 100%（以测试矩阵为准）。
- 冷启动 p95 < 50 ms；`list` p95 < 80 ms。
- 所有安装 artifact 100% 经过哈希校验；签名索引覆盖率 100%。

### 8.2 生态指标

- 官方仓库首日覆盖 500 个开发者常用包。
- 包清单平均审核时间 < 48 小时。
- 第三方仓库可通过一条命令接入，且默认要求显式信任指纹。
- 因包质量问题导致的安装失败率 < 1%。

### 8.3 负向指标（必须保持为零）

- 未经用户确认的提权。
- 未经签名验证的索引被用于解析。
- 安装脚本在默认配置下被执行。
- 回滚后仍残留旧的 PATH 项 / shim 指向不存在的目标。

## 9. 命名与语气

- **Kita**：短、可读、无强绑定含义；适合作为命令前缀。
- 语气：克制、直接、可操作。错误信息先给事实，再给建议。
- 不使用“魔法”“一键起飞”等营销词；用“计划”“校验”“代际”“回滚”这些能被验证的词。

## 10. 反模式清单

设计评审时用来否决提案：

1. 为了“方便”在安装时执行任意 PowerShell。
2. 把包列表上传到远端作为默认行为。
3. 用修改全局 PATH 的方式支持每个包。
4. 在 UI 上展示一个版本、实际安装另一个版本。
5. 用“卸载脚本”冒充事务回滚。
6. 对没有哈希的 artifact 放行。
7. 在没有 UAC 明确确认的情况下修改机器级状态。
8. 为了支持某个安装器而引入常驻服务。
9. 让锁文件包含时间戳、绝对路径或本机状态。
10. 把 Windows 的文件名规则当成 Unix 的超集。
