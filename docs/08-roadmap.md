# 08 · 路线图、风险与开放问题

> 当前阶段：**设计稿 v0.1**。本路线图假设 2–4 名熟悉 Rust 与 Windows 的工程师，按“先证明，再扩展”的顺序推进。时间是量级估计，不是承诺。

## 1. 路线总览

```text
Phase 0  Design spike        2–3 周   证明关键机制可行
Phase 1  Walking skeleton    4–6 周   便携包能装、能跑、能回滚
Phase 2  Reproducible env    4–6 周   清单/锁/远程签名仓库
Phase 3  Trust & ops         4–6 周   可日常使用、可诊断、可离线
Phase 4  Native Windows      6–8 周   MSI、UAC、机器级
Phase 5  Beta / hardening    6–8 周   真实包、性能、崩溃恢复
          v1.0
Phase 6  Post-1.0            —        EXE 配方、多环境、TUF、GUI
```

## 2. Phase 0 · Design spike（2–3 周）

**目标**：用最小代码证明最难的四件事，不追求产品完整。

### 交付

1. **CAS + 硬链接原型**
   - 写入对象、构建 generation、`active.json` 切换、回滚。
   - 在 NTFS、ReFS、Windows Defender 开启、OneDrive 同步目录下测试。
2. **shim 原型**
   - console + GUI 两个版本；参数保真、退出码、Ctrl+C、Job Object。
   - 测试 Electron/Node/Rust 工具链等会 spawn 子进程的程序。
3. **MSI 原型**
   - 枚举 ProductCode、静默安装、卸载、失败回滚。
   - 在非管理员与管理员两种 token 下测试。
4. **威胁模型评审**
   - 针对签名、提权、路径逃逸做一次红队桌面演练。
5. **包样本**
   - 选 20 个真实工具，验证 manifest schema 是否能表达：
     - 单 exe、带 DLL 的目录、多 bin、GUI、MSI、需要写配置、需要 VC++ runtime。

### 退出标准

- [ ] 同一份 artifact 能装进两个 generation，切换 < 50 ms。
- [ ] 在干净 Windows Sandbox 中，20 个样本至少 15 个能通过 portable 或 MSI 安装。
- [ ] 删除/损坏 active.json 后，doctor 能恢复到最后有效 generation。
- [ ] shim 运行 Node/Electron 应用无控制台异常、无参数损坏。
- [ ] MSI 安装失败时，系统回到安装前状态（ProductCode 不再存在）。
- [ ] 路径逃逸样本全部被拒绝。

如果 Phase 0 失败，**不要进入 Phase 1**；先调整模型（例如放弃硬链接，改用目录 junction / 复制）。

## 3. Phase 1 · Walking skeleton（4–6 周）

**目标**：`kita add ripgrep` 能工作，且状态机是可测试的。

### 范围

- Rust workspace、CI、错误码骨架。
- `kita.toml` / `kita.lock` 的解析与校验。
- 本地文件仓库（无网络） + fixture。
- portable zip/tar.zst 安装。
- CAS、tree digest、generation、active.json。
- shim 安装与 PATH 管理（用户级）。
- `init`、`add`、`remove`、`sync`、`list`、`plan`（基础）。
- 回滚（纯 portable）。
- 集成测试：temp root + 本地仓库 + 故障注入。

### 不做

- 远程仓库签名、依赖求解、MSI、提权、bundle、GC 完整策略。

### 退出标准

- [ ] 全新用户目录下 `init → add → run` 成功。
- [ ] `sync` 幂等；无变更不产生新 generation。
- [ ] 强制在每一步崩溃后，下一次启动能恢复。
- [ ] 卸载不删除用户数据；回滚恢复文件。
- [ ] 所有测试不依赖公网。

## 4. Phase 2 · Reproducible environment（4–6 周）

**目标**：一份锁文件能在另一台机器复现。

### 范围

- 远程静态仓库 + zstd + ETag + 镜像。
- minisign/Ed25519 签名与密钥固定。
- PubGrub 求解器 + 可解释冲突。
- `sync --locked`、`update`、`lock`、`why`、`search`、`info`。
- 计划 diff、JSON 输出、退出码稳定。
- artifact 下载并行、断点、缓存。
- 签名索引 + manifest 哈希 + artifact 哈希全链路。
- 基础 revocation/yank。

### 退出标准

- [ ] 机器 A 生成锁，机器 B `sync --locked` 得到完全相同的 tree digest。
- [ ] 篡改索引/manifest/artifact 的测试全部在安装前失败。
- [ ] 版本冲突输出可读的 derivation，且有回归测试。
- [ ] 100 个包的计划在 1 秒内完成（热缓存）。
- [ ] 离线模式下，已锁定的环境可安装；新解析明确失败。

## 5. Phase 3 · Trust & operations（4–6 周）

**目标**：可以给早期用户日常使用，出问题能诊断。

### 范围

- `doctor`、`verify`、`gc`、`cache`、`log`、`generation pin`。
- 完整崩溃恢复与 journal 归档。
- Restart Manager 文件占用处理。
- 第三方仓库信任 UI、私有仓库认证（Credential Manager）。
- 离线 bundle（创建/安装/校验）。
- 崩溃点注入测试矩阵。
- 性能基准与预算。
- 安装包 / 自更新签名。

### 退出标准

- [ ] 所有关键路径有故障注入测试。
- [ ] `doctor --fix` 能修复 90% 的本地状态问题（以故障样本计）。
- [ ] bundle 在无网络 Windows Sandbox 中可完整安装。
- [ ] shim 启动开销 p95 < 15 ms。
- [ ] 安装 20 个工具 < 2 分钟（100 Mbps，冷缓存）。

## 6. Phase 4 · Native Windows（6–8 周）

**目标**：支持 MSI 和机器级安装，同时保持“可逆性诚实”。

### 范围

- MSI 检测、安装、升级、卸载、receipt。
- UAC 提权代理 + 计划校验 + 审计。
- machine scope、机器级 PATH、机器级 shim 目录 ACL。
- `rollback --files-only` / `--with-native`。
- MSI 失败恢复与 `repair` 指引。
- Authenticode 验证与发布者展示。
- 非管理员/管理员 token 的测试矩阵。

### 退出标准

- [ ] 标准 MSI 在用户级与机器级都能安装/卸载。
- [ ] UAC 拒绝时无副作用。
- [ ] 提权代理无法执行计划外的命令/路径（渗透测试）。
- [ ] MSI 失败后 ProductCode 不残留。
- [ ] 机器级 shim 目录不可被普通用户写入。

## 7. Phase 5 · Beta / hardening（6–8 周）

**目标**：真实世界可用，发布 v1.0。

### 范围

- 官方仓库 500+ 开发者常用包。
- winget / Scoop 清单导入（只生成 manifest，不依赖其运行时）。
- 性能优化：索引懒加载、解压流水线、启动。
- 文档、错误码页面、迁移指南。
- 安全审计、依赖审计、fuzzing。
- 遥测（可选、默认关闭）、崩溃报告（显式）。
- 灰度发布与回滚演练。
- ARM64 完整支持。

### v1.0 退出标准

- [ ] 核心命令面冻结，JSON schema 稳定。
- [ ] 锁文件格式冻结，提供迁移工具。
- [ ] Windows 10 22H2 / 11 24H2 x64 + ARM64 通过测试矩阵。
- [ ] 20 个真实团队场景（Web、Rust、Node、Python、嵌入式）验证。
- [ ] 无 P0/P1 安全问题；外部审计完成。
- [ ] 升级失败自动回滚成功率 > 99.9%（故障注入 + 真实样本）。
- [ ] 文档覆盖所有错误码。
- [ ] 发布二进制全部签名，提供 SBOM 与校验和。

## 8. Post-1.0

按优先级：

1. **EXE 安装器白名单配方**（Inno/NSIS），明确 `best_effort`。
2. **多环境 / 同包多版本**（`kita env`、项目隔离、shim 选择）。
3. **TUF 快照/时间戳与阈值签名**，替换当前的 TUF-lite。
4. **Provenance 验证**（Sigstore/in-toto）与 SBOM 漏洞扫描。
5. **GUI / 托盘**：只读状态 + 更新提示，不做常驻服务。
6. **导入/导出**：winget、Scoop、Chocolatey 的迁移工具。
7. **企业策略**：强制仓库、禁用提权、离线镜像、审计导出。
8. **增量更新**：内容定义分块（CDC），减少大包升级流量。

## 9. 风险登记

| # | 风险 | 概率 | 影响 | 缓解 |
| --- | --- | --- | --- | --- |
| R1 | Windows 软件太杂，manifest 表达不了 | 高 | 高 | Phase 0 用 20 个真实包验证；保留 native/MSI 逃生舱；EXE 配方后置 |
| R2 | 硬链接被 AV/备份/同步软件破坏 | 中 | 中 | 只读对象；verify；复制回退；避免在 OneDrive 根目录默认安装 |
| R3 | shim 被 AV 误报 | 中 | 高 | 代码签名；提交白名单；最小二进制；不使用可疑 API 组合；提供 `.cmd` 逃生模式（可选） |
| R4 | MSI 行为差异大，卸载不干净 | 高 | 中 | 只支持标准 MSI；receipt + Windows Installer 日志；失败时 repair 指引；不承诺完全回滚 |
| R5 | UAC 代理成为提权漏洞 | 低 | 极高 | 计划白名单、ACL、nonce、重检前置条件、外部审计、最小 API |
| R6 | 签名密钥管理失误 | 低 | 极高 | 根密钥离线；repo key HSM；轮换演练；撤销流程；TUF 路线 |
| R7 | 生态冷启动 | 高 | 高 | 先覆盖开发者 top 100；winget 导入；团队场景驱动；不自建商店 |
| R8 | 求解器输出难懂 | 中 | 中 | PubGrub derivation 渲染；大量冲突 fixture；`kita why` |
| R9 | 性能不达标（启动/解压） | 中 | 中 | 预算 + CI 基准；懒加载；硬链接；流式解压 |
| R10 | 长路径/ACL/ARM64 边界 | 中 | 中 | 专用测试矩阵；`kita-win` 集中处理；Windows Sandbox |
| R11 | 范围膨胀（GUI、容器、跨平台） | 高 | 中 | 明确的非目标；v1 只做 CLI；路线图评审 |
| R12 | 单环境模型限制团队工作流 | 中 | 中 | v1 用独立 root/CI 规避；v1.x 设计多环境 |

## 10. 关键开放问题

这些问题不影响开始 Phase 0，但需要在 v1.0 前决定：

1. **官方仓库的治理与托管**
   - 谁来审包？命名空间政策？CDN 成本？是否接受企业镜像？
2. **依赖模型**
   - Windows 软件大多是自包含的；依赖主要用于工具链组件。是否限制依赖深度？是否允许虚拟包/`provides`？
3. **同包多版本**
   - v1 不支持；开发者常需要 Node 18/20/22 并存。是 v1.1 的最高优先级吗？
4. **原生包的 bin 发现**
   - MSI 安装路径不固定。是要求 manifest 声明 `${INSTALL_DIR}`，还是允许安装后扫描？
5. **EXE 安装器政策**
   - 是否永远只支持白名单配方？认证标准是什么？谁维护配方？
6. **TUF 时间点**
   - 当前签名模型能覆盖 v1，但快照/时间戳/阈值签名何时补上？
7. **Windows 10 支持期限**
   - 是否跟随微软 EOL？影响测试矩阵与 API 选择。
8. **遥测与隐私**
   - 是否永远默认关闭？企业是否需要强制审计导出？
9. **许可证**
   - 代码 MIT/Apache-2.0；文档 CC BY 4.0；名称/Logo 商标策略。
10. **品牌**
    - “Kita” 在多语言中的含义与商标可用性需要检索；CLI 前缀 `kita` 是否与现有工具冲突。

## 11. 前 10 个工程任务（建议顺序）

1. 初始化 workspace、CI、错误码骨架、`kita-core` 类型。
2. 实现 `RelPath` / `Version` / `Requirement` + property tests。
3. 实现 CAS 对象写入 + tree digest + 临时目录原子 rename。
4. 实现 generation builder + `active.json` + FakePlatform 测试。
5. 实现 console shim + 参数/退出码/Job Object 测试。
6. 用本地 fixture 仓库跑通 `init → add → run`。
7. 加入计划 diff、`sync` 幂等、回滚。
8. 接入 HTTP 仓库 + 签名 + 哈希校验。
9. 接入 PubGrub + 锁文件 + 冲突解释。
10. Windows CI + Windows Sandbox + 故障注入。

## 12. 决策检查点

在每个 Phase 结束做一次 go/no-go：

- **Phase 0 → 1**：难机制是否被证明？如果硬链接不可靠，是否愿意切复制模式？
- **Phase 1 → 2**：状态机是否稳定？崩溃恢复是否可测试？
- **Phase 2 → 3**：签名链是否完整？锁文件是否真正可复现？
- **Phase 3 → 4**：用户是否愿意日常使用 portable？MSI 是否是必须的？
- **Phase 4 → 5**：提权面是否可接受？是否通过了安全评审？
- **Phase 5 → v1.0**：是否有 20 个真实团队场景？是否愿意冻结格式？

任何检查点失败，优先缩减范围，而不是降低安全/可逆性标准。
