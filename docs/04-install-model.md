# 04 · 安装模型

本文描述 Kita 如何把“解析结果”变成“可运行、可回滚的系统状态”。核心是四个机制：

1. **内容寻址存储（CAS）**：同一文件只存一份。
2. **不可变 generation**：每次变更生成完整快照，切换指针。
3. **shim-first 激活**：PATH 里只有一个目录，命令通过 launcher 分发。
4. **可逆性分级**：便携包完全可逆；MSI 由 Windows Installer 回滚；不假装 EXE 安装器是原子的。

## 1. 为什么不是“直接解压到安装目录”

传统包管理器：

```text
下载 → 解压到 C:\tools\rg → 改 PATH → 结束
```

问题：

- 升级覆盖旧文件，失败后处于半新半旧状态；
- 回滚需要重新下载、重新解压；
- 多个包往 PATH 追加目录，顺序不可控；
- 无法知道某个文件属于哪个包；
- 卸载可能删掉用户后来放进去的文件；
- 没有跨包去重。

Kita：

```text
下载 → 校验 → 文件进入 CAS → 构建新 generation（硬链接）
     → 写 READY → 原子替换 active.json → 旧 generation 保留
```

切换是 O(1) 的指针替换；回滚是再替换一次。

## 2. 存储布局与对象

### 2.1 文件对象

```text
store/objects/<sha256>          # 不可变、只读的普通文件
```

- 对象名是内容 SHA-256，不是包名。
- 写入流程：临时文件 → 边写边哈希 → 校验长度 → `FlushFileBuffers` → 原子 rename。
- 已存在的对象直接复用（先校验大小，再按需校验哈希）。
- 设置 `FILE_ATTRIBUTE_READONLY`，降低误写风险。
- 对象不包含“属于哪个包”的信息；引用关系在 tree 清单里。

### 2.2 树清单

```json
{
  "schema": 1,
  "digest": "sha256:...",
  "entries": [
    { "path": "rg.exe", "kind": "file", "size": 3045678, "sha256": "..." },
    { "path": "doc/rg.1", "kind": "file", "size": 12034, "sha256": "..." }
  ]
}
```

- 路径使用 `/` 作为分隔符，UTF-8 存储，安装时转成 Windows 路径。
- 不记录 mtime、ACL、owner；这些不属于可复现内容。
- 空目录可选记录；v1 不保留空目录。
- 树摘要算法：

```text
sha256(
  for each entry sorted by path:
    path_utf8 + "\0" + kind + "\0" + size_decimal + "\0" + sha256_hex + "\n"
)
```

- 树清单本身也以 digest 命名：`store/trees/<digest>.json`。

### 2.3 去重收益

- 同一个工具的不同版本共享未变化文件（文档、DLL、证书）。
- 不同包共享相同 VC++ runtime / 证书文件（如果内容完全一致）。
- 硬链接不复制数据，只增加目录项；`robocopy`、备份工具会看到多个文件，这是预期行为。

### 2.4 什么时候不硬链接

| 情况 | 行为 |
| --- | --- |
| store 与 generation 不在同一卷 | 回退为复制，并警告（正常布局强制同卷） |
| 包声明 `install.mutable = true` | 复制到 generation，允许包写自己的安装目录 |
| 文件被标记为 reparse point | 拒绝（v1 不支持包内 symlink/junction） |
| 硬链接数达到上限 | 复制并记录 |
| 杀毒软件/备份软件干扰 | 重试；仍失败则复制 |

> **可变包**：少数便携应用会在 exe 旁边写配置。Kita 提供 `mutable = true`，此时该包不使用硬链接，generation 内是可写副本；回滚会连同当时的文件一起恢复。代价是不能跨版本去重。需要长期保存的数据应使用 `${KITA_DATA}`。

## 3. Generation

### 3.1 目录

```text
envs/default/generations/42/
├─ generation.json
├─ READY
└─ tree/
   ├─ kita_ripgrep_14.1.1/
   │  ├─ rg.exe
   │  └─ ...
   └─ kita_fd_10.2.0/
      └─ ...
```

`generation.json`：

```json
{
  "schema": 1,
  "generation": 42,
  "created_at": "2026-11-09T08:31:22Z",
  "platform": "x86_64-pc-windows-msvc",
  "plan_digest": "sha256:...",
  "manifest": "sha256:...",
  "packages": [
    {
      "id": "kita/ripgrep",
      "version": "14.1.1",
      "instance": "kita_ripgrep_14.1.1",
      "tree": "sha256:...",
      "scope": "user",
      "activation": "shim"
    }
  ],
  "tools": {
    "rg": {
      "package": "kita/ripgrep",
      "version": "14.1.1",
      "origin": "managed",
      "root": "tree/kita_ripgrep_14.1.1",
      "path": "rg.exe",
      "kind": "console",
      "env": {}
    }
  },
  "env": {}
}
```

- 目录名使用版本 + 短哈希，避免大小写/特殊字符问题。
- `READY` 是最后写入的空文件；没有它说明 generation 未完成。
- generation 号单调递增，从不复用。
- 删除旧 generation 前先检查文件占用。

### 3.2 构建步骤

```text
1. 在 envs/default/tmp/gen-<uuid>/ 创建 staging
2. 为每个 package instance 创建 tree/<instance>/
3. 遍历 tree 清单：
   a. 确保 object 存在且哈希正确
   b. CreateHardLinkW(object, target)
   c. 失败则 CopyFileW
4. 写 generation.json
5. FlushFileBuffers(generation.json) + FlushFileBuffers(READY)
6. rename staging -> generations/<n>（同卷、原子）
7. 写 active.json 临时文件（同目录）
8. ReplaceFileW(active.json, temp, backup)
9. SQLite 记录（异步/失败不致命）
10. 旧 generation 按保留策略处理
```

注意顺序：**先 READY，后 active**。即使在第 7 步崩溃，active 仍指向旧 generation；staging 或新 generation 会在下次 `doctor/gc` 清理。

### 3.3 原子替换 `active.json`

- 使用 `ReplaceFileW`，它保留 ACL、属性，并产生一个备份。
- 如果目标不存在（首次），使用 `MoveFileExW(MOVEFILE_WRITE_THROUGH)`。
- 替换前 `FlushFileBuffers`。
- 备份文件 `active.json.bak` 至少保留到下一次成功激活。
- 不用“删除后重命名”，避免出现无 active 的窗口。

### 3.4 执行顺序

```text
1. 获取写锁，reconcile active.json / state
2. 下载缺失 artifact（并行）
3. 校验并解压 portable artifact 到 CAS（并行，有界）
4. 执行原生安装器（串行、必要时提权），拿到 receipt
5. 构建新 generation：portable 文件硬链接 + native 工具的 receipt 路径
6. 写 generation.json + READY
7. 原子替换 active.json
8. 提交 SQLite / 审计日志
9. 按保留策略 GC（best-effort）
```

为什么 native 安装放在构建 generation 之前：

- MSI 的实际安装目录与 ProductCode 只有安装完成后才能可靠获得；
- native shim 需要 receipt 中的 `${INSTALL_DIR}`；
- 若顺序反过来，会生成指向不存在路径的 shim。

代价是存在一个“MSI 已安装、active 尚未切换”的窗口。Journal 必须记录：

- 本次新安装的 ProductCode；
- 被升级/替换的旧 ProductCode 与旧 MSI 缓存路径；
- 若第 5–8 步（构建 generation / 激活 / 提交）失败，启动恢复时优先卸载新 MSI 并重装旧 MSI，然后清理；
- 若恢复也无法完成，进入 `kita repair`，不自动删除证据。

### 3.5 回滚

```console
$ kita rollback
```

- 读取上一个 `READY` 且 `generation.json` 校验通过的 generation。
- 只替换 `active.json`；不移动、不重新下载文件。
- 更新 SQLite 的 current 指针与审计日志。
- 如果目标 generation 含 native 包，提示：

```text
该 generation 包含 1 个原生安装器包：
  example/contoso-agent 2.4.0 (MSI)

回滚文件与 shim 是即时的；但系统安装状态需要重新运行安装器。
  kita rollback --files-only      仅回滚文件/shim（可能导致版本不一致）
  kita rollback --with-native     重新安装旧版本（需要 UAC，耗时较长）
```

- 保留最近 5 个 generation；`kita generation pin <n>` 可防止 GC。
- `kita generation list` 展示每次变更的摘要、时间、plan digest。

## 4. 激活与 shim

### 4.1 `active.json`

运行时唯一真相。shim 每次启动读取它（通常几 KB），不读 SQLite，不解析 TOML。

```json
{
  "schema": 1,
  "scope": "user",
  "generation": 42,
  "generation_path": "envs/default/generations/42",
  "tools": {
    "rg": {
      "package": "kita/ripgrep",
      "version": "14.1.1",
      "origin": "managed",
      "root": "tree/kita_ripgrep_14.1.1",
      "path": "rg.exe",
      "kind": "console",
      "env": {}
    }
  },
  "env": { "EDITOR": "hx" }
}
```

- `origin = "managed"`：`root` 相对 generation；`path` 相对 `root`。
- `origin = "native"`：`root` 是 MSI receipt 记录的绝对安装目录；`path` 相对该目录。
- env 在激活时已展开模板，shim 不需要理解 `${...}`。
- `tools` 在生成 generation 时构建完整，避免运行时合并多个来源。
- native 工具在 MSI 安装完成、拿到 receipt 后才加入 `tools`（见 §3.4 的顺序）。

### 4.2 PATH

只添加一个目录：

```text
%LOCALAPPDATA%\Kita\shims
```

- `kita init` 展示 PATH diff 并请求确认；默认插入到用户 PATH 前部。
- 不删除、不重排用户已有项；只确保目标目录存在且在 PATH 中。
- 如果检测到同名命令在更早位置，`kita doctor` 报告 shadowing，给出修复建议。
- 机器级 shim 目录 `%ProgramFiles%\Kita\bin` 写入系统 PATH，必须通过提权代理。
- 卸载 Kita 时只移除自己添加的项，并记录原始值。

### 4.3 shim 如何工作

```text
rg.exe (kita-shim)
  │
  ├─ 1. GetModuleFileNameW → 自身路径
  ├─ 2. 推导 root：用户级 shims\..\，机器级 bin\..\
  ├─ 3. 读取 root\active.json
  ├─ 4. 取文件名 stem "rg" → tools["rg"]
  ├─ 5. 校验：managed 目标在 generation 内；native 目标在 receipt 的安装目录内
  ├─ 6. 合并环境变量（继承 + active.env + tool.env）
  ├─ 7. CreateProcessW(target, args, CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT)
  ├─ 8. 创建 Job Object，KILL_ON_JOB_CLOSE，AssignProcessToJobObject
  ├─ 9. ResumeThread
  ├─ 10. WaitForSingleObject
  └─ 11. ExitProcess(child_exit_code)
```

关键点：

- **不是 .cmd / .ps1 包装器。** 没有 shell 转义问题，启动快，Ctrl+C 行为正常。
- **不自动提权。** 目标需要管理员时，由目标自己的 manifest 触发 UAC，Kita 不静默提升。
- **GUI 版本**：`kita-shimw.exe` 使用 `windows` subsystem，避免控制台闪烁；`[[bin]] kind = "gui"` 时复制这个版本。
- **参数保真**：解析原始命令行并重新构造，保留引号/反斜杠语义；有专门的属性测试。
- **退出码**：原样返回子进程退出码；被 Job 终止时返回 `0xC000013A` 或等价错误。
- **环境**：只注入该工具声明的变量；不修改系统环境。
- **防篡改**：managed 目标必须在当前 generation 的 tree 下；native 目标必须在 receipt 的安装目录下；`active.json` 损坏时拒绝执行并提示 `kita doctor`，不猜测路径。

### 4.4 命令冲突

- 两个包暴露同名命令：计划阶段直接失败，列出双方包名。
- 依赖包的 bin 默认也暴露；如果依赖只是运行库，可在包清单声明 `bins = false`（依赖关系级），不暴露其命令。
- v1 不支持同一命令的多版本切换；需要并存时使用独立的 `KITA_ROOT`。

### 4.5 shim 的更新

- shim 二进制只存一份（`kita-shim.exe` / `kita-shimw.exe`）；每个命令名通过**硬链接**指向它，失败时回退为复制。这样 100 个命令不会占用 100 份二进制。
- `kita self update` 或版本升级后，`kita doctor --fix` 重新铺设 shim。
- 每个 shim 的 Authenticode 签名与主程序一致；`kita verify --shims` 校验哈希。
- 用户手动重命名 shim 会得到“未知工具”错误；这是有意的防误用。

## 5. 便携包安装与卸载

### 5.1 安装

1. 下载 artifact 到 `cache/downloads/<sha256>.part`。
2. 校验大小、SHA-256；成功后 rename 为正式缓存文件。
3. 解压到 `tmp/extract-<uuid>/`：
   - 拒绝绝对路径、`..`、盘符、UNC、ADS、设备名、结尾空格/点；
   - 拒绝 symlink/junction/reparse point；
   - 检测大小写折叠冲突；
   - 限制文件数、总大小、压缩比，防 zip bomb；
   - 流式计算每个文件哈希，写入 CAS。
4. 生成 tree 清单与 digest。
5. 构建新 generation；冲突的命令名在此阶段报错。
6. 激活。

### 5.2 卸载

1. 解析目标包，生成不含它的新 generation。
2. 切换 active；shim 立即不再暴露。
3. 旧 generation 保留在回滚窗口内。
4. `--purge` 删除 `${KITA_DATA}/<package>`。
5. GC 回收无引用对象。

卸载不删除用户数据、不删除日志、不删除缓存，除非显式 `--purge` / `kita cache clean`。

### 5.3 更新

- 新版本进入新 generation；旧版本目录保留。
- 正在运行的程序可能继续使用旧文件（Windows 文件句柄），这通常没问题；删除旧 generation 前用 Restart Manager 检查。
- 如果新版本启动失败，`kita rollback` 立即恢复旧版本。

## 6. 原生安装器（MSI）

### 6.1 为什么单独处理

MSI 的安装状态不在 Kita 的 CAS 里：文件、注册表、服务、COM 由 Windows Installer 管理。Kita 不能把 MSI 安装“硬链接”进 generation。因此：

- 文件/shim 的激活仍然事务化；
- MSI 本身使用 Windows Installer 的事务/回滚；
- Kita 记录 receipt，并在计划中诚实标注可逆性。

### 6.2 检测

```text
MsiEnumRelatedProductsW(upgrade_code)
  → 得到 ProductCode
  → MsiGetProductInfoW(ProductCode, INSTALLPROPERTY_VERSIONSTRING)
  → MsiGetProductInfoW(ProductCode, INSTALLPROPERTY_INSTALLLOCATION)
```

- 优先使用 `upgrade_code`；没有则用 `product_code`。
- 检测结果缓存到 receipt，`kita doctor` 可重新扫描。
- 检测不到但文件存在：不猜测，报告不一致。

### 6.3 安装

```text
1. 下载/校验 MSI（SHA-256 + 大小）
2. 可选：WinVerifyTrust 校验 Authenticode
3. 构建计划：MsiInstall { path, properties, scope }
4. 用户确认（machine scope 触发 UAC）
5. CreateProcessW("msiexec.exe",
     /i "<path>" /qn /norestart /l*v "<log>")
6. 等待；解析退出码
7. 成功：读取 ProductCode / InstallLocation，写 receipt
8. 失败：Windows Installer 自动回滚；Kita 清理 receipt
```

- 不使用 shell；参数逐个构造。
- 固定 `/qn /norestart`；需要重启时返回明确错误，不自动重启。
- 解析 Windows Installer 返回码：`0` 成功；`3010` 安装成功但需要重启（继续激活并显示 `reboot required` 警告，不把安装判为失败）；`1641` 安装器已请求重启（本应被 `/norestart` 抑制，视为错误）；`1603` 致命错误；`1618` 另一个安装正在进行；`1602` 用户取消。除 `0` / `3010` 外都不得激活新 generation。
- 需要重启时写入 operation 记录；`kita doctor` 在重启前提示，不在后台重启。
- MSI 日志保存在 `logs/msi/<operation>-<timestamp>.log`，失败时提示路径。
- 属性白名单：`ALLUSERS`、`MSIFASTINSTALL`、`INSTALLDIR` 等；禁止任意属性注入命令行。
- Authenticode：默认记录发布者；官方仓库可标记 `require_signature = true`，不满足则拒绝。

### 6.4 升级

- 有 `upgrade_code` 时，MSI 自己处理 major upgrade；Kita 只负责调用新 MSI。
- 安装前记录旧 ProductCode 和版本，写入 journal。
- 升级失败：Windows Installer 回滚到旧版本；Kita 校验旧 ProductCode 仍存在。
- 升级成功：旧 ProductCode 通常已被移除；更新 receipt。

### 6.5 卸载

```text
msiexec /x <ProductCode> /qn /norestart
```

- 只卸载 Kita receipt 中记录的 ProductCode，不按名字猜测。
- 用户手动安装的同名软件不会被误删。
- 顺序：先构建并激活“不含该包”的 generation（shim 立即消失），再执行 `msiexec /x`；若卸载失败，包仍在系统中但不再被 Kita 管理，进入 `kita repair`，不自动回滚到旧 generation（否则 shim 指向已部分删除的文件）。
- 卸载后清理 shim、PATH 项（如果由 Kita 添加）、receipt。

### 6.6 回滚的真实语义

| 场景 | 文件/shim | 系统安装状态 | 结论 |
| --- | --- | --- | --- |
| 纯 portable | 指针切换 | 无 | 秒级、完全回滚 |
| MSI 安装失败 | 不激活 | Windows Installer 回滚 | 安全 |
| 已安装 MSI 后回滚上一代 | 指针切换 | 需要重装旧 MSI | 默认拒绝自动回滚，要求 `--with-native` |
| EXE 安装器 | 不支持 v1 | 不可控 | 不执行 |

Kita 不把“能卸载”说成“可回滚”。计划中显示：

```text
reversible: full | installer | best_effort | none
```

## 7. 文件占用与更新

Windows 上更新正在运行的程序很常见。策略：

1. 构建新 generation 不需要写旧文件，所以通常不受占用影响。
2. GC 删除旧 generation 前，用 Restart Manager 注册即将删除的路径。
3. 如果发现占用：
   - 交互式：列出进程名/PID，询问是否关闭并重启；
   - 非交互：默认跳过该 generation 的删除（延迟 GC），不阻塞更新；
   - `--close-apps`：显式允许 Kita 关闭这些进程。
4. 关闭后可选 `RmRestart` 恢复被关闭的应用；默认不自动重启。
5. 永不强制结束未确认的进程。

## 8. 提权模型

### 8.1 原则

- 默认不提权。
- 提权只发生在 machine scope 的原生安装或系统集成。
- 提权进程只执行**计划中已列出**的操作，不接受任意命令。
- 一次提权只服务一个计划；完成后退出。
- 内部二进制是 `kita-elevated.exe`；为熟悉 pacman 的用户提供 `k-sudo.exe` 别名，但**它不是通用 sudo**，只接受签名的计划文件，不执行任意命令行。

### 8.2 流程

```text
父进程（用户 token）
  1. 解析、生成 Plan
  2. 将 Plan 写入 %LOCALAPPDATA%\Kita\tmp\elev\<uuid>.json
  3. ACL：仅当前用户、SYSTEM、Administrators
  4. 计算 plan_digest，生成一次性 nonce
  5. 创建命名管道（ACL 限制）
  6. ShellExecuteExW("runas", kita-elevated.exe, --plan <path> --pipe <name> --nonce <hex>)
  7. 等待管道结果，显示 UAC 由系统处理

子进程（管理员 token）
  8. 验证调用者、文件 ACL、文件 owner
  9. 重新读取 Plan，校验 digest/nonce/schema
 10. 拒绝任何不在白名单的操作/路径
 11. 重新检查当前状态是否仍与计划前置条件一致
 12. 执行，写 journal，通过管道返回结构化结果
 13. 退出；删除临时 Plan
```

### 8.3 白名单与计划来源

提权代理只允许：

- 在 `%ProgramFiles%\Kita`（machine root）与 `%ProgramData%\Kita\data` 下创建/替换文件；
- 修改系统 PATH（仅 Kita shim 目录 `%ProgramFiles%\Kita\bin`）；
- 调用 `msiexec` 安装/卸载计划中的 MSI；
- 写入 Kita 的 machine state；
- 明确的注册表事务（未来，仅 MSI 之外的系统集成）。

代理不只检查“路径是否在白名单内”，还必须验证计划来源：

1. 计划文件由当前用户创建、ACL 未被放宽、owner 正确；
2. `plan_digest` 与内容一致，nonce 未使用过；
3. 计划中的每个 MSI 都有 `sha256`，且缓存文件的实际哈希匹配；
4. 该 MSI 的 manifest digest 能追溯到本地缓存的、已签名且未过期的仓库索引；
5. 计划不包含任何“从网络下载后执行”的步骤；
6. 在代理自己的窗口中展示包 ID、发布者、MSI 哈希和将执行的操作，要求用户再次确认；UAC 本身只证明“谁在请求”，不证明“用户同意这些具体操作”。

禁止：

- 任意命令行；
- 任意路径写入；
- PowerShell / cmd / WScript；
- 下载（下载在用户态完成并校验，提权进程只读本地文件）；
- 修改不属于 Kita 的注册表键；
- 创建服务（v1 不支持）。

### 8.4 UAC 被拒绝

- 返回 `KITA-6001`，计划不执行，active 不变。
- 已完成的用户态步骤（下载、staging）保留或清理，不影响系统。
- 不循环弹 UAC。

## 9. 崩溃恢复

### 9.1 恢复矩阵

| 崩溃点 | 磁盘状态 | 恢复动作 |
| --- | --- | --- |
| 下载中 | `.part` | 删除/续传；active 不变 |
| 解压中 | `tmp/extract-*` | 删除；active 不变 |
| MSI 安装中 | Windows Installer 回滚 | 读 journal，校验 receipt |
| MSI 成功、generation 未完成 | 系统已安装新版本，active 仍旧 | 按 journal 卸载新 ProductCode / 重装旧 MSI；清理 staging |
| 构建 generation 中 | 无 READY 的 staging | 删除 staging；若本次有 native 安装，执行 native undo |
| READY 后、active 前 | 新 generation 完整但未引用 | 保留或 GC；若含 native 变更，按 journal 决定是否 undo |
| active 替换中 | `active.json.bak` + 新文件 | 校验新文件；失败则用 bak，并 undo native |
| active 后、SQLite 前 | active 新，DB 旧 | 启动时 reconcile，以 active 为准 |
| GC 中 | trash 中残留 | 下次启动清空 trash |

### 9.2 启动 reconcile

每次写命令启动时：

1. 获取 state lock。
2. 读取 `active.json`；若损坏，尝试 `.bak`。
3. 校验 generation 路径存在、有 READY、generation.json 可解析。
4. 校验 shim 目标存在。
5. 不匹配时：
   - 能修复：重建 tools 表/索引；
   - 不能修复：回退到最近有效 generation，进入只读安全模式并提示 `kita doctor`。
6. 清理无 READY 的 staging（超过 24 小时）。
7. 归档未完成 operation。

## 10. GC 与保留策略

默认：

- 保留最近 5 个 generation；
- 保留所有被 `kita generation pin` 的 generation；
- 保留最近 14 天的下载缓存（可配置）；
- native receipt 引用的 MSI 不按天数删除：只要对应 generation 仍在保留窗口内，就保留其缓存（否则 `rollback --with-native` 无法离线完成）；
- 保留 30 天操作日志；
- 数据目录永不自动删除。

GC 流程：

1. 获取写锁。
2. 标记：所有保留 generation 的 tree 引用。
3. 标记：保留 generation 中 native receipt 引用的 MSI artifact。
4. 标记：active、pending、bundle 构建中的临时引用。
5. 清扫：无引用 tree 清单、object 和过期的 MSI 缓存。
6. 删除只读的 generation 链接时使用 `FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE`（Windows 10 1709+），避免为了删除一个链接而清除共享 object 的只读属性；不支持时回退为“清除属性 → 删除 → 重新设置 object 只读”。
7. 对可能被占用的文件，移动到 `trash/<uuid>/` 后尝试删除；失败留待下次。
8. 更新 SQLite。
9. `--dry-run` 展示可回收空间。

## 11. 完整性与修复

`kita verify` 检查：

- `active.json` schema、generation 路径、READY；
- 每个 immutable generation 的 tree 文件存在、链接目标正确、SHA-256 与 object 一致；
- `mutable = true` 的包只校验文件存在与可执行入口，不把用户修改视为损坏；报告“已修改（允许）”；
- shim 的哈希/签名；
- lock 与 active 的包版本一致（`--locked` 时）；
- 原生 receipt 的 ProductCode 仍存在；
- PATH 中包含 shim 目录且无更高优先级冲突。

`kita doctor --fix` 可以：

- 重建 `active.json` 的 tools 表；
- 重新创建缺失的 shim；
- 从 CAS 重新硬链接损坏的 immutable generation 文件；
- 清理 staging/trash；
- 重建 SQLite 索引；
- 重新添加 PATH 项（需确认）。

不能自动修复：

- 原生安装器状态（需要重装/卸载）；
- 被用户修改的 mutable 包内容（这是用户数据，不是损坏）；
- 已撤销包的信任问题。

## 12. 性能预算

| 操作 | 目标（p95） |
| --- | --- |
| shim 启动额外开销 | < 15 ms |
| 激活 20 个包 / 2000 个文件 | < 150 ms（硬链接） |
| 回滚 | < 50 ms |
| 无变更 `sync` | < 400 ms（热缓存、无网络） |
| 解压 50 MB zip | < 1.5 s |
| `kita list` | < 80 ms |
| 冷启动 `kita --version` | < 50 ms |

这些预算会进入 CI 基准；超过预算的 PR 需要说明。

## 13. 设计取舍

### 为什么用硬链接而不是 junction

- junction 只能指向目录，无法做到文件级去重；
- junction 目标的写操作会穿透，容易破坏 store；
- 硬链接的文件属性共享，正好可以用只读属性保护；
- 文件级硬链接允许不同 generation 共享未变化文件。

代价：同卷限制、备份工具看到多份、极少数应用对 hardlink 敏感。通过 `mutable` 和复制回退解决。

### 为什么 `active.json` 而不是数据库指针

- shim 需要极快、无依赖地读取；
- 单文件原子替换比 SQLite 事务更适合“切换生效”；
- 文件可读、可审计、可手工恢复；
- SQLite 损坏不影响运行。

### 为什么不在安装时执行脚本

- 无法可靠回滚；
- 供应链攻击面最大；
- 与“声明式、可复现”冲突；
- 大多数脚本只是复制/改 PATH/写注册表，这些应由引擎声明式完成。

代价：部分包需要维护者做额外适配；这是有意的门槛。
