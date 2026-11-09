# 05 · 仓库协议与安全模型

Kita 的仓库是**静态 HTTPS 文件树**：没有必需的服务端、没有数据库连接、没有私有 API。任何能托管静态文件的 CDN / S3 / GitHub Pages / Nginx 都能成为仓库。

安全目标不是“相信仓库”，而是**让客户端能独立验证每一个字节，并且只在明确信任的范围内解析**。

## 1. 仓库布局

```text
/v1/
├─ repository.toml                 # 仓库元数据、签名密钥、代际、过期时间
├─ repository.toml.minisig
├─ catalog.toml.zst                # 搜索目录
├─ catalog.toml.zst.minisig
├─ revocations.toml                # 撤销列表
├─ revocations.toml.minisig
├─ packages/
│  └─ kita/
│     └─ ripgrep/
│        ├─ index.toml             # 版本列表、依赖、manifest 哈希
│        ├─ index.toml.minisig
│        └─ 14.1.1/
│           └─ manifest.toml       # 完整包清单（哈希已固定在 index）
└─ blobs/
   └─ sha256/<hash>                # 可选：仓库镜像 artifact
```

- 所有元数据是 UTF-8 TOML，可 zstd 压缩（`.zst`）。
- 签名是 Ed25519 的 detached minisign 格式，便于用现成工具审计。
- manifest 本身可以不再单独签名：它的 SHA-256 被签名过的 `index.toml` 固定。
- artifact 可以托管在任意 HTTPS 位置：它的 SHA-256 与大小被签名过的 manifest 固定。
- 路径不可变：同一个 URL 的内容不得改变；新版本使用新路径。CDN 可以永久缓存。

## 2. `repository.toml`

```toml
schema = 1
id = "kita"
name = "Kita Official"
description = "Kita 官方公共仓库"
base_url = "https://repo.kita.dev/v1"
generation = 2026110901
issued_at = "2026-11-09T00:00:00Z"
expires_at = "2026-12-09T00:00:00Z"

[[key]]
id = "kita-repo-2026"
algorithm = "ed25519"
public_key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3"
roles = ["package-index", "catalog", "revocations"]
not_before = "2026-01-01T00:00:00Z"
not_after = "2027-01-01T00:00:00Z"

[[mirror]]
url = "https://mirror.example.net/kita/v1"
priority = 10

[limits]
max_catalog_bytes = 33_554_432
max_index_bytes = 8_388_608
max_manifest_bytes = 1_048_576
```

字段：

| 字段 | 说明 |
| --- | --- |
| `id` | 仓库稳定 ID；锁文件与配置引用它 |
| `generation` | 单调递增的元数据代际；用于防回滚 |
| `expires_at` | 元数据过期时间；过期后仍可离线使用，但新解析告警/拒绝 |
| `key` | 受信任的公钥集合；每把钥有角色与有效期 |
| `mirror` | 可选镜像；客户端也可本地追加，但必须能通过签名校验 |
| `limits` | 客户端资源上限；防止恶意超大文件 |

`repository.toml` 由**仓库根密钥**签名。根公钥编译进 Kita（官方仓库）或由用户显式信任（第三方仓库）。

### 2.1 密钥分层

- **Root key（离线）**：签名 `repository.toml` 与密钥轮换；不直接签包。
- **Repo key（在线/HSM）**：签名 `index.toml`、`catalog`、`revocations`。
- **Package maintainer**：通过 PR / CI 提交清单；不持有在线签名密钥。

轮换流程：

1. 用旧 root key 签新的 `repository.toml`，其中包含新 repo key，旧 key 标记 `not_after`。
2. 客户端若已信任旧 root key，可平滑接受新 repo key。
3. 若 root key 本身需要轮换，发布新版 Kita（内嵌新 root key），并保留旧 key 验证过渡期元数据。
4. 私钥泄露：发布撤销 + 新 root key + Kita 安全公告；客户端拒绝旧 generation 之后签出的元数据，直到更新。

v1 使用单根密钥 + 明确的有效期；多签阈值/TUF 快照是 v2 目标（见 roadmap）。

## 3. 包索引 `index.toml`

```toml
schema = 1
id = "kita/ripgrep"
generation = 2026110901
expires_at = "2026-12-09T00:00:00Z"

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
yanked_reason = "上游在 ARM64 上崩溃"
```

- 索引包含**求解所需的最小信息**：版本、目标平台、依赖、yank 状态、manifest 哈希。
- 解析器不需要下载所有 manifest；选中版本后才拉完整清单。
- `dependencies` 使用与 manifest 相同的需求语法；两者不一致时以 manifest 为准并报错。
- 每个文件有 `generation` 和 `expires_at`；客户端拒绝更低的 generation（防回滚）。

## 4. Catalog

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

- 只用于 `kita search` 和信息展示，不参与解析。
- 客户端按 ID 排序存储，支持前缀匹配和关键词过滤。
- 可按首字母分片（`catalog/a.toml.zst`）以支持超大仓库；v1 单文件即可。

## 5. 签名与验证

### 5.1 验证链

```text
Kita 内置 root public key
        │ 验证
        ▼
repository.toml (repo keys, generation, expiry)
        │ 用 repo key 验证
        ▼
packages/<id>/index.toml  → manifest_sha256
        │ 验证哈希
        ▼
package manifest (artifact urls + sha256 + size)
        │ 验证哈希
        ▼
artifact bytes → tree → generation
```

任何一环失败都停止，不“跳过继续”。

### 5.2 最小验证步骤

拉取仓库元数据：

1. HTTPS 成功（不允许降级到 HTTP）。
2. 下载内容有大小上限。
3. `repository.toml.minisig` 能用内置/已信任的 root key 验证。
4. `schema == 1`，`id` 与配置一致。
5. `generation` 不低于本地记录（否则拒绝，防回滚）。
6. 未过期（过期只阻止新解析；锁定的离线安装仍可用，见 §5.5）。
7. 选中的 repo key 在有效期内且具备所需角色。

拉取包索引：

1. 签名可由 `repository.toml` 中的 repo key 验证。
2. `id` 与路径一致。
3. `generation` 不低于本地缓存。
4. 未过期。
5. 索引中的版本号合法，manifest 路径在仓库内且无 `..`。

拉取 manifest：

1. SHA-256 等于索引中的 `manifest_sha256`。
2. schema、ID、版本一致。
3. 重跑全部语义校验（不信任仓库的预校验）。
4. artifact 的 target 与主机匹配。

拉取 artifact：

1. `Content-Length`（若有）不超过声明大小。
2. 实际字节数等于 `size`。
3. SHA-256 等于清单值。
4. 解压阶段再次做路径/大小/数量校验。

### 5.3 签名文件格式

使用 minisign（Ed25519）兼容格式：

```text
untrusted comment: signature from kita repo key
RWS1...base64...
trusted comment: kita/ripgrep index generation 2026110901
...base64...
```

- 客户端验证 `trusted comment` 不参与安全判断，仅用于审计展示。
- 签名覆盖原始文件字节（不是解压后的字节）。
- 对于 `.zst` 文件，协议规定**签名覆盖压缩后的文件字节**；客户端先读原始字节验证，再解压。对于未压缩的 `.toml`（如 `repository.toml`），签名覆盖该 TOML 的原始字节。
- 允许 HTTP 传输压缩（`Content-Encoding`），但签名针对解压 HTTP 后的存储文件；不要与服务端动态压缩混淆。

### 5.4 密钥固定与第三方仓库

```console
$ kita repo add internal https://packages.corp.example/kita/v1
正在连接...
仓库       internal
根密钥指纹  SHA256:9f3c...a21e
有效期      2027-01-01
包数量      1,284

信任这个仓库？只有在你确认指纹后才继续。
输入 "trust internal" 继续：
```

- 第三方仓库默认不信任；必须交互确认指纹。
- `--yes` 不能跳过首次指纹确认；CI 可用 `kita repo trust --fingerprint <sha256>`。
- 仓库 URL 变化不自动迁移信任；视为新仓库。
- 同一包 ID 在多个仓库：按 `source` 精确解析，**绝不自动 fallback**。
- 仓库删除/不可达时，锁定的 `source` 仍必须匹配；否则拒绝安装，防止同名包替换。

### 5.5 过期与离线

- 元数据过期后：
  - 已有锁 + 完整缓存：允许安装，显示“元数据已过期，使用锁定版本”。
  - 需要新解析：默认拒绝，提示 `kita update --refresh` 或检查网络。
  - 超过 90 天：即使锁定也要求显式 `--offline-expired`（防止长期冻结在漏洞版本）。
- 时间来自系统时钟；检测到明显回拨（比本地记录早）时拒绝新解析。
- 离线 bundle 携带签名元数据与时间戳，可在无网络时验证；过期策略由 bundle 创建者决定。

## 6. 撤销与 Yank

### 6.1 Yank

- 不删除文件，只在索引中标记 `yanked = true`。
- 新解析排除；已锁定安装仍可用，但 `kita verify`/`update` 提示。
- 适合“有更好版本”或“该版本有非安全问题”。

### 6.2 Revocation

```toml
schema = 1
generation = 7
issued_at = "2026-11-09T00:00:00Z"

[[revoked]]
package = "kita/foo"
versions = ["<=1.4.2"]
severity = "critical"       # critical | high | medium
reason = "artifact 被替换，哈希与上游发布不一致"
advisory = "https://kita.dev/advisories/2026-001"
artifact_sha256 = ["..."]    # 可选：只撤销特定构建
```

- `critical`：即使已锁定也默认拒绝安装；`--allow-revoked` 仅用于取证，并记录审计。
- `high`：新解析拒绝，已安装告警。
- `medium`：告警。
- 撤销列表每次 `sync` 尝试刷新；离线时使用缓存，并显示缓存时间。
- 撤销不能“修复”已安装的恶意代码；它只阻止继续传播。Kita 不自动卸载，避免误伤。

## 7. 威胁模型

### 7.1 假设

- 攻击者可能控制：镜像、CDN、网络路径（在 TLS 之外无法解密，但可阻断/重放）、上游下载站、某个包维护者账号。
- 攻击者**不**控制：Kita 发布签名私钥、用户已信任的 root key、Windows 内核/管理员权限。
- 用户 scope 的本地恶意进程不在防护范围内（它已经能以用户身份做任何事）；machine scope 的 ACL 是边界。

### 7.2 威胁与缓解

| 威胁 | 影响 | 缓解 |
| --- | --- | --- |
| 镜像替换 artifact | 执行恶意代码 | manifest 固定 SHA-256 + 大小；签名索引固定 manifest |
| 镜像替换索引/manifest | 安装错误包 | 每个索引/元数据签名验证；manifest 哈希链 |
| 回滚到旧签名索引（freeze） | 安装有漏洞的旧版 | 单调 generation + 本地记录；过期检查 |
| 上游发布站被篡改 | 首次发布即恶意 | 维护者审核、哈希复现、可选 Authenticode/provenance；无法完全消除 |
| 包维护者账号被盗 | 恶意更新 | 签名权限分离（维护者不提签）、review、yank/revocation、审计 |
| 依赖混淆 | 安装内部包的同名公开包 | 锁文件 `source` 固定；仓库不隐式 fallback；命名空间所有权 |
| 抢注/仿冒包名 | 用户装错包 | 命名空间验证、相似名警告、发布者信息、官方推荐位 |
| 恶意安装脚本 | 任意代码执行 | **默认不存在脚本**；MSI 是唯一原生执行路径，显式确认 + UAC |
| Zip Slip / 路径逃逸 | 覆盖任意文件 | 严格路径规范化、拒绝 `..`/绝对路径/ADS/设备名/reparse |
| Zip bomb | 磁盘耗尽 | 压缩比、总大小、文件数上限；流式解压 |
| DLL 侧加载 | 执行恶意 DLL | 包目录隔离、签名展示、可选校验；不承诺沙箱 |
| PATH 劫持 | 运行错误程序 | 单一 shim 目录、doctor 检测 shadowing、机器目录 ACL |
| shim 被替换 | 任意代码执行 | shim Authenticode 签名；doctor 校验；用户 scope 下无法完全防御同用户攻击 |
| 提权代理被滥用 | 系统级破坏 | 一次性计划、ACL、nonce、操作白名单、重新校验状态 |
| 凭据泄露 | 私有仓库访问 | Windows Credential Manager；日志脱敏；不写入锁/清单 |
| 更新通道被攻击 | 供应链根替换 | 发布签名、SLSA、离线根密钥、可回滚的 self-update |
| 本地状态被篡改 | 执行错误目标 | shim 校验 generation 路径；`kita verify`；机器 scope ACL |

### 7.3 不在防护范围

- 用户主动以管理员身份运行恶意 MSI。
- 已获得用户 token 的恶意软件。
- Windows 本身或内核被攻破。
- 上游软件运行时从网络下载的额外代码（Kita 只能验证安装时的字节）。
- 包自身的逻辑漏洞（Kita 不是杀毒软件）。

## 8. 包发布流程

### 8.1 提交流程

```text
1. 维护者提交 package.toml + 测试证据（PR）
2. CI:
   - schema/语义校验
   - URL 可达、大小/哈希匹配
   - 在干净 Windows Sandbox / VM 中安装并运行 --version
   - 路径安全 fuzz
   - 与上一版本 diff（命令、权限、安装方式）
3. 审核者检查发布者、许可证、行为变化
4. 合并后由签名服务（HSM）更新 index/catalog 并签名
5. CDN 发布；generation +1
```

### 8.2 维护者不能做什么

- 不能直接拿到在线 repo 签名私钥。
- 不能在一个版本发布后修改该路径的内容（CDN 不可变 + 哈希）。
- 不能绕过 review 修改已有版本的 manifest。
- 不能声明脚本或任意注册表写入。

### 8.3 包命名空间

- 官方仓库默认 namespace `kita`，只分配给通过审核的包。
- 组织 namespace 需要证明所有权：GitHub org、DNS TXT、或人工审核。
- 私有仓库可以使用自己的 namespace，但必须独立信任其根密钥。
- 包 ID 一旦发布不可重命名；废弃使用 `deprecated` + 替代包字段。

## 9. 私有仓库与认证

- 使用同一静态协议；区别只是需要 `Authorization`。
- Token 存在 Windows Credential Manager：
  - target：`Kita:repo:<repo-id>`
  - 用户名：`token`
  - 密码：PAT / OAuth token
- 请求头只在内存中构造；日志与错误信息脱敏。
- 支持 `kita auth login <repo>` / `logout`；支持环境变量 `KITA_TOKEN_<REPO>` 供 CI（不推荐长期使用）。
- 不把 token 写进 URL、锁文件、manifest、bundle。
- 代理：使用系统 WinHTTP/环境变量代理；企业根证书通过 Windows 证书存储验证。

## 10. Artifact 的额外验证

### 10.1 Authenticode

- 可选字段：

```toml
[[artifact]]
# ...
publisher = "CN=Contoso Ltd, O=Contoso Ltd, L=..., C=US"
require_signature = true
```

- 下载后调用 `WinVerifyTrust`；校验签名链、时间戳、发布者主体。
- 失败时按 `require_signature` 决定拒绝或警告。
- 不把 Authenticode 当作哈希的替代：它证明“谁签了”，不证明“内容是你期望的版本”；两者都要。

### 10.2 Provenance / SBOM（可选）

```toml
[[artifact]]
# ...
[artifact.provenance]
type = "slsa"
url = "https://..."
sha256 = "..."
```

- v1 只存储和展示，不强制验证。
- v2 接入 Sigstore / in-toto 验证。
- SBOM 放在 manifest 或单独文件；用于漏洞扫描，不影响安装。

### 10.3 可复现构建

Kita 不要求上游可复现，但会：

- 显示 `reproducible = true/false/unknown`（维护者声明）；
- 在 `kita info` 展示构建来源；
- 对官方仓库优先收录可复现构建。

## 11. 隐私

- 默认**零遥测**。
- 可选遥测只包含：命令、耗时、成功/失败、Kita 版本、Windows 版本、架构、错误码。
- 不包含：包名、路径、用户名、仓库 URL、搜索词、IP 之外的精确定位。
- 本地日志默认保留 30 天；`kita log export` 由用户主动导出，导出前可预览。
- 搜索在本地 catalog 完成；不把查询发给服务端。
- 安装计数由仓库 CDN 的聚合访问日志产生，Kita 不额外上报。

## 12. Kita 自身的供应链

- 发布二进制使用 EV Authenticode 签名；shim 与主程序同签名。
- 发布 `SHA256SUMS` + minisign 签名；GitHub Releases 只是分发渠道。
- 构建使用固定工具链、锁定依赖、`cargo deny` / `cargo audit`。
- 目标：SLSA Level 3 provenance + 可复现构建。
- `kita self update`：
  - 只从配置的、签名的 channel 获取；
  - 下载到临时文件，验证签名与哈希；
  - 原子替换；保留上一版本用于回滚；
  - 不后台自动更新，必须显式命令或 `kita upgrade --self`；
  - 企业可用 `self_update = false` 禁用。
- 安全公告与 root key 轮换通过独立签名渠道发布。

## 13. 安全默认值清单

| 默认 | 值 |
| --- | --- |
| 安装脚本 | 禁止 |
| 未签名索引 | 拒绝 |
| 无哈希 artifact | 拒绝 |
| HTTP | 拒绝（本地测试需显式 `--allow-http`） |
| 第三方仓库 | 首次必须确认指纹 |
| 提权 | 仅 machine scope，逐次 UAC |
| 包列表遥测 | 关闭 |
| 已 yank 版本 | 新解析排除 |
| critical revocation | 锁定安装也拒绝 |
| PATH 修改 | 仅一个 shim 目录，需确认 |
| 自动更新 | 关闭 |
| 安装目录写入 | 只读对象；mutable 包需显式声明 |
