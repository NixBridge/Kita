# 07 · Rust 实现设计

> 本文是实现蓝图，不包含已实现代码。目标是把前六篇的决策落成可测试、可演进的 Rust workspace。

## 0. 为什么用 Rust

- **解析不可信输入**：TOML、zip/tar、索引、路径都来自网络；Rust 的所有权/边界检查能减少内存安全漏洞，同时保持零成本抽象。
- **单二进制、无运行时**：`kita.exe` 不依赖 .NET / JVM / Node；shim 可以做到极小、快速启动。
- **并发下载/解压**：Tokio + 有界并发，配合 `spawn_blocking` 处理 CPU 密集的哈希/解压。
- **Win32 可控封装**：`windows` / `windows-sys` 提供类型化 API；把 `unsafe` 限制在 `kita-win`。
- **可测试性**：强类型让计划、journal、签名状态可以被构造和断言；属性测试适合版本/路径这类规则。
- **分发**：交叉编译 x64/ARM64、静态链接 CRT、单文件发布相对成熟。

代价是开发速度与生态学习曲线；对系统工具而言，长期维护成本与安全收益更高。

## 1. 工具链与约束

| 项 | 选择 |
| --- | --- |
| Edition | 2024 |
| MSRV | 1.85+（Edition 2024 的最低稳定版本） |
| 目标 | `x86_64-pc-windows-msvc`、`aarch64-pc-windows-msvc` |
| 异步运行时 | Tokio（multi-thread，仅网络/并发） |
| 错误 | `thiserror` + 自定义诊断层；库层不用 `anyhow` |
| 序列化 | `serde` + `toml` + `serde_json`；编辑清单用 `toml_edit` |
| 数据库 | SQLite（`rusqlite`，bundled）+ WAL |
| HTTP | `reqwest`（rustls + Windows 平台证书验证） |
| 日志 | `tracing` + 本地 JSONL 审计 |
| CLI | `clap` derive |
| 终端 | `anstream` / `anstyle` / `indicatif` |
| 测试 | `insta`、`proptest`、`assert_cmd`、`tempfile`、`wiremock` |
| 基准 | `criterion` |

不引入 async runtime 到 shim。shim 必须是同步、极小、启动快、无网络。

## 2. Workspace

```text
kita/
├─ Cargo.toml                # workspace
├─ Cargo.lock
├─ rust-toolchain.toml
├─ deny.toml
├─ apps/
│  ├─ kita/                  # CLI 二进制
│  ├─ kita-elevated/         # 提权代理（仅执行已校验的计划）
│  └─ kita-shim/             # shim 二进制（console/gui）
├─ crates/
│  ├─ kita-core/             # 领域类型、错误、Plan
│  ├─ kita-manifest/         # TOML 解析、校验、规范化
│  ├─ kita-registry/         # 仓库客户端、签名、索引
│  ├─ kita-resolver/         # PubGrub 适配
│  ├─ kita-store/            # CAS、tree、generation、GC
│  ├─ kita-app/              # use cases / 编排
│  ├─ kita-install/          # 执行引擎、journal、portable/MSI
│  ├─ kita-win/              # Win32 封装（唯一允许 unsafe 的库）
│  ├─ kita-bundle/           # 离线 bundle
│  └─ kita-testkit/          # 测试仓库/fixture/fault injection
└─ xtask/                    # 发布、schema 生成、bench
```

依赖方向：

```text
apps/kita
  └─ kita-app
       ├─ kita-manifest ─┐
       ├─ kita-registry ─┤
       ├─ kita-resolver ─┼─ kita-core
       ├─ kita-store ────┤
       ├─ kita-install ──┘
       │    └─ kita-win
       └─ kita-bundle
```

- `kita-core` 不依赖 I/O、Tokio、Windows。
- `kita-win` 不依赖产品策略。
- `kita-app` 不打印到 stdout；通过 `Ui` trait 发出事件，便于 CLI/JSON/测试复用。
- `kita-testkit` 只作为 dev-dependency。

## 3. `kita-core`：类型即约束

### 3.1 标识与版本

```rust
use std::fmt;
use std::str::FromStr;

pub struct PackageId {
    namespace: Namespace,
    name: PackageName,
}

impl PackageId {
    pub fn parse(s: &str) -> Result<Self, IdError>;
    pub fn canonical(&self) -> &str;       // "kita/ripgrep"
    pub fn namespace(&self) -> &Namespace;
    pub fn name(&self) -> &PackageName;
}

#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Version {
    parts: SmallVec<[u64; 4]>,             // 1..=8 段
    pre: Vec<PreRelease>,
}

impl Version {
    pub fn parse(s: &str) -> Result<Self, VersionError>;
    pub fn is_prerelease(&self) -> bool;
}

impl Ord for Version { /* 数字段补零比较；pre 按 SemVer 规则 */ }
```

要点：

- `Version` 只允许合法字符；拒绝前导零。
- 解析时去除末尾零段（至少保留一段），所以 `1.2.0.0` 与 `1.2` 是同一个规范版本；`Ord` / `Eq` / `Hash` 一致。
- `Ord` 与 `Display` 必须满足 `parse(display(v)) == v`。
- v1 拒绝 `+build`，因此没有“显示不同但排序相同”的歧义；未来如需支持 build metadata，必须同时定义身份与排序规则并升级版本模型。
- `Version` 的数字段用 `u64`；超出范围拒绝。
- 预发布标识符：数字/字母/连字符，点分隔。

需求：

```rust
pub struct Requirement {
    sets: Vec<Range>,          // OR 的并集
    allow_prerelease: bool,
}

impl Requirement {
    pub fn parse(s: &str) -> Result<Self, RequirementError>;
    pub fn contains(&self, v: &Version) -> bool;
    pub fn intersect(&self, other: &Self) -> Option<Self>;
    pub fn is_any(&self) -> bool;
}
```

范围用半开区间 `[lower, upper)` 表达，边界显式携带包含性；不要用浮点/字符串比较。

### 3.2 清单类型

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    pub schema: u32,
    pub environment: EnvironmentSpec,
    pub packages: BTreeMap<PackageId, PackageRequest>,
    pub env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub schema: u32,
    pub id: PackageId,
    pub version: Version,
    pub summary: String,
    pub description: Option<String>,
    pub license: SpdxExpression,
    pub homepage: Url,
    pub upstream: Option<Url>,
    pub platform: PlatformSpec,
    pub install: InstallSpec,
    pub capabilities: Capabilities,
    pub artifacts: Vec<Artifact>,
    pub bins: Vec<BinSpec>,
    pub env: BTreeMap<String, String>,
    pub dependencies: BTreeMap<PackageId, DependencySpec>,
}
```

- 所有 `String` 在解析后由 newtype 校验，不把“校验过的字符串”到处传。
- `PackageManifest` 的 `artifacts` 不允为空；`BinSpec` 名称唯一。
- `ProjectManifest` 的包 ID 在反序列化时规范化；重复 key（大小写不同）报错。
- 未知字段默认拒绝；未来兼容通过 `schema` 升级，不靠忽略字段。

### 3.3 计划

```rust
pub struct Plan {
    pub schema: u32,
    pub command: CommandKind,
    pub scope: Scope,
    pub generation: GenerationChange,
    pub actions: Vec<PlannedAction>,
    pub downloads: DownloadSummary,
    pub native: Vec<NativePlan>,
    pub elevation: ElevationSummary,
    pub reversibility: Reversibility,
    pub warnings: Vec<Diagnostic>,
    pub digest: PlanDigest,
}

pub enum PlannedAction {
    Add { package: ResolvedPackage },
    Update { from: InstalledPackage, to: ResolvedPackage },
    Remove { package: InstalledPackage },
    Relink { package: InstalledPackage },  // 修复缺失链接
    Noop,
}
```

- `Plan::digest` 是 canonical JSON 的 SHA-256；用于 UI 展示与提权一致性。
- 计划中不能包含凭据、临时路径、时间戳；`created_at` 不参与 digest。
- `Reversibility`：`Full` / `Installer` / `BestEffort` / `None`，取所有步骤的最弱值。

### 3.4 安全类型

用类型防止“未验证就用”：

```rust
pub struct Sha256Digest([u8; 32]);

pub struct VerifiedArtifact {
    digest: Sha256Digest,
    size: u64,
    path: PathBuf,
    // 只能由 Verifier 构造
}

pub struct TrustedRepository {
    id: RepoId,
    metadata: RepositoryMetadata,
    // 只能由签名验证 + 用户信任后构造
}

pub struct ReadyGeneration {
    number: Generation,
    dir: PathBuf,
    // 只能由 GenerationBuilder 在 READY 写完后构造
}
```

构造器私有，模块外不能伪造。执行引擎只接受 `VerifiedArtifact` / `TrustedRepository` / `ReadyGeneration`。

### 3.5 诊断

```rust
pub struct Diagnostic {
    pub code: ErrorCode,
    pub severity: Severity,
    pub message: String,
    pub context: Vec<ContextItem>,
    pub hints: Vec<String>,
    pub docs: Option<Url>,
}

pub struct ErrorCode(&'static str); // "KITA-3007"
```

`thiserror` 用于底层错误；在 use case 边界映射为 `Diagnostic`。CLI 统一渲染，库层不格式化人类文本。

## 4. `kita-manifest`

职责：

- 读取/写入 `kita.toml`。
- 严格解析、语义校验、规范化 digest。
- 用 `toml_edit` 做保留注释的编辑（`kita add/remove`）。
- 不访问网络。

关键函数：

```rust
pub fn load_project(path: &Path) -> Result<ProjectManifest, ManifestError>;
pub fn load_package(bytes: &[u8]) -> Result<PackageManifest, ManifestError>;
pub fn canonical_digest(manifest: &ProjectManifest) -> Sha256Digest;
pub fn add_package(path: &Path, spec: &PackageSpec) -> Result<(), ManifestEditError>;
pub fn remove_package(path: &Path, id: &PackageId) -> Result<(), ManifestEditError>;
```

编辑流程：

```text
读取原始文本 -> toml_edit::DocumentMut
  -> 校验目标表/键
  -> 修改（保留注释、顺序）
  -> 写临时文件 -> 原子 rename
  -> 重新解析校验
```

如果 `toml_edit` 写入后无法通过强类型校验，回滚并不写入。

## 5. `kita-registry`

职责：

- 仓库信任与元数据缓存。
- 拉取/验证 index、catalog、manifest。
- ETag、zstd、镜像、离线。
- 签名验证。

```rust
#[async_trait]
pub trait Registry: Send + Sync {
    async fn metadata(&self, repo: &RepoId) -> Result<TrustedRepository, RegistryError>;
    async fn package_index(&self, repo: &RepoId, id: &PackageId) -> Result<PackageIndex, RegistryError>;
    async fn manifest(&self, repo: &RepoId, version: &Version) -> Result<PackageManifest, RegistryError>;
    async fn catalog(&self, repo: &RepoId) -> Result<Catalog, RegistryError>;
    async fn revocations(&self) -> Result<Revocations, RegistryError>;
}
```

内部模块：

```text
client.rs      HTTP、超时、重试、代理、认证
cache.rs       ETag、磁盘缓存、offline 回退
signature.rs   minisign/Ed25519、密钥固定、generation 防回滚
verify.rs      manifest 哈希、schema 二次校验
mirror.rs      镜像选择与失败切换
```

并发：

- 包索引请求去重（同一个包只发一次）。
- 上限 `Semaphore`（默认 16）。
- 失败重试指数退避 + jitter；4xx 不重试（401/403 触发 auth 提示）。
- 不允许无限重定向；最多 5 次，最终仍须 HTTPS。

## 6. `kita-resolver`

### 6.1 PubGrub 适配

```rust
pub struct RepoProvider<'a> {
    registry: &'a dyn Registry,
    indexes: HashMap<PackageId, PackageIndex>,
    platform: Target,
    preferences: Preferences,
}

impl DependencyProvider for RepoProvider<'_> {
    type P = PackageId;
    type V = Version;
    type VS = Range<Version>;
    type M = Incompatibility<PackageId, Range<Version>>;
    type Err = ResolverError;

    fn prioritize(&mut self, package: &PackageId, range: &Range<Version>) -> Priority;
    fn choose_version(&mut self, package: &PackageId, range: &Range<Version>)
        -> Result<Option<Version>, Self::Err>;
    fn get_dependencies(&mut self, package: &PackageId, version: &Version)
        -> Result<Dependencies, Self::Err>;
}
```

### 6.2 懒加载

- 初始只有根需求。
- 求解器请求某个包时，才通过 `Registry` 拉取其 index。
- index 已包含依赖与目标平台信息，避免拉 manifest。
- 选定版本后，才拉完整 manifest，做最终校验（哈希、平台、bin 路径）。
- 所有请求缓存到内存；失败缓存不跨命令。

### 6.3 偏好

```rust
pub struct Preferences {
    pub prefer_installed: bool,
    pub allow_prerelease: bool,
    pub prefer_stable: bool,
    pub avoid_yanked: bool,
}
```

`choose_version` 不使用 HashMap 的随机顺序；候选先排序再按偏好选择。保证同一输入得到同一锁文件。

### 6.4 冲突解释

PubGrub 的 derivation 需要映射为人类可读：

```text
Incompatibility { terms: [(node ^10.2), (pnpm >=11)] }
  → "kita/node 22.12.0 要求 npm ^10.2，而 kita/pnpm 9.15.0 要求 npm >=11"
```

保存每个版本被选中的原因（derivation），渲染时优先显示最接近根的冲突。

## 7. `kita-store`

### 7.1 模块

```text
object.rs      CAS 写入/读取/校验
tree.rs        tree manifest、digest、路径规范化
generation.rs  staging -> READY -> rename
gc.rs          mark-sweep、trash
link.rs        硬链接/复制回退
```

### 7.2 API

```rust
pub struct Store { root: PathBuf }

impl Store {
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Sha256Digest, StoreError>;
    pub fn put_reader(&self, r: impl Read, expected: &Sha256Digest) -> Result<(), StoreError>;
    pub fn has_object(&self, digest: &Sha256Digest) -> bool;
    pub fn object_path(&self, digest: &Sha256Digest) -> PathBuf;
    pub fn write_tree(&self, entries: &[TreeEntry]) -> Result<TreeDigest, StoreError>;
    pub fn build_generation(&self, plan: &Plan, env: &Environment) -> Result<ReadyGeneration, StoreError>;
    pub fn gc(&self, policy: &GcPolicy) -> Result<GcReport, StoreError>;
}
```

- `put_reader` 边读边哈希，不信任调用者传入的 digest；不匹配时删除临时文件。
- `build_generation` 只接受 `VerifiedArtifact` 与 `TreeManifest`。
- 所有路径操作使用 `kita-win` 的 `SafePath` / 长路径辅助。

### 7.3 树摘要

```rust
pub struct TreeEntry {
    pub path: RelPath,          // 已校验，无 .. / 绝对路径 / ADS
    pub size: u64,
    pub digest: Sha256Digest,
}

pub fn tree_digest(entries: &[TreeEntry]) -> TreeDigest;
```

- 排序使用路径的 UTF-8 字节序，不依赖区域设置。
- 大小写冲突检测在构建 `RelPath` 集合时完成。

## 8. `kita-install`：执行引擎

### 8.1 步骤与 journal

```rust
pub enum Step {
    Download(DownloadStep),
    Extract(ExtractStep),
    Link(LinkStep),
    MsiInstall(MsiStep),
    MsiUninstall(MsiUninstallStep),
    WritePath(PathStep),
    Activate(ActivateStep),
}

pub struct ExecCtx<'a> {
    pub store: &'a Store,
    pub platform: &'a dyn Platform,
    pub journal: &'a mut Journal,
    pub cancel: CancellationToken,
    pub ui: &'a dyn Ui,
}

#[async_trait]
impl Step {
    async fn execute(&self, ctx: &mut ExecCtx<'_>) -> Result<StepOutcome, StepError>;
    async fn undo(&self, ctx: &mut ExecCtx<'_>) -> Result<(), StepError>;
    fn reversibility(&self) -> Reversibility;
}
```

- 每一步执行前写 `Started`，成功后写 `Completed`，失败写 `Failed`。
- `undo` 逆序执行已完成的步骤；`Activate` 之前的失败自动 undo。
- `Activate` 成功后不再自动 undo（否则可能把已成功状态弄坏）；记录完成。
- 不可逆步骤必须 `reversibility() != Full`，并在 UI 中展示。

### 8.2 执行顺序

```text
1. Acquire lock
2. Reconcile active.json / state
3. Verify plan preconditions (generation still current, files unchanged)
4. Download missing artifacts (parallel)
5. Extract / CAS (parallel, bounded)
6. Native installer steps (if any, serial, elevated) → receipts
7. Build generation (staging, hardlinks + native receipt paths)
8. Activate (ReplaceFile active.json)
9. Commit DB + audit
10. GC best-effort
```

下载与解压可以流水线：下载完成一个就解压一个，但**激活必须等全部成功**。native 安装必须在构建 generation 之前完成，因为 shim 目标来自 MSI receipt；若第 6 步之后失败，journal 负责卸载新 MSI / 重装旧 MSI。native 步骤串行执行，不与其他 MSI 并行。

### 8.3 前置条件

执行前重新检查：

- 当前 generation 等于计划生成时的 generation；
- 计划涉及的包未被其他进程改变；
- 需要的 artifact 缓存仍通过哈希；
- 磁盘空间足够（至少估计大小的 1.2 倍）；
- state lock 仍持有。

不一致时返回 `KITA-2004 stale plan`，要求重新 `plan`。

## 9. `kita-win`

### 9.1 原则

```rust
#![deny(clippy::undocumented_unsafe_blocks)]
```

每个 `unsafe` 块必须有 `// SAFETY:` 说明。其余 crate：

```rust
#![forbid(unsafe_code)]
```

### 9.2 模块与 API

```text
fs.rs       长路径、原子替换、只读属性、卷判断、硬链接
process.rs  CreateProcess、Job Object、控制台、退出码
msi.rs      MsiEnumRelatedProducts、MsiGetProductInfo、安装/卸载
security.rs WinVerifyTrust、signer 信息
elevate.rs  ShellExecuteEx runas、命名管道、计划校验
restart.rs  Restart Manager
path.rs     用户/系统 PATH 读写（仅显式调用）
acl.rs      受限 ACL 设置（计划文件、机器目录）
```

关键封装：

```rust
pub fn create_hard_link(from: &Path, to: &Path) -> io::Result<()>;
pub fn replace_file(src: &Path, dst: &Path) -> io::Result<()>;
pub fn is_same_volume(a: &Path, b: &Path) -> io::Result<bool>;
pub fn set_readonly(path: &Path, readonly: bool) -> io::Result<()>;
pub fn verify_authenticode(path: &Path) -> Result<SignatureInfo, SecurityError>;
pub fn close_file_holders(paths: &[PathBuf], ui: &dyn Ui) -> Result<HolderAction, RestartError>;
```

### 9.3 路径安全

所有来自压缩包/清单的路径先转换为 `RelPath`：

```rust
pub struct RelPath(PathBuf);

impl RelPath {
    pub fn parse(input: &str) -> Result<Self, PathError>;
    pub fn join_under(&self, root: &Path) -> Result<PathBuf, PathError>;
}
```

规则：

- 拒绝绝对路径、盘符、UNC、`..`、`.` 以外的特殊组件；
- 拒绝 NUL、ADS `:`、结尾空格/点；
- 拒绝 `CON`、`PRN`、`AUX`、`NUL`、`COM1-9`、`LPT1-9` 及其带扩展名形式；
- 拒绝 reparse point（v1 不跟随包内链接）；
- 大小写折叠后唯一；
- 长度上限 32,767，警告 > 260。

`join_under` 之后用 `GetFinalPathNameByHandleW` 再次确认最终路径在 root 下（防 junction 逃逸）。

### 9.4 shim 二进制

- 使用 `windows-sys` 而不是完整 `windows` crate，减少体积。
- 不使用 Tokio、clap、reqwest。
- 依赖：`serde` + `serde_json`（读 active.json）、`windows-sys`。
- 编译为 `#![windows_subsystem = "windows"]` 的 GUI 版本和 console 版本两个二进制。
- 目标：stripped < 1.5 MB，启动额外开销 < 15 ms。

## 10. `kita-app`

每个命令是一个 use case：

```rust
pub struct App {
    pub env: Environment,
    pub registry: Arc<dyn Registry>,
    pub store: Arc<Store>,
    pub platform: Arc<dyn Platform>,
    pub state: Arc<State>,
    pub ui: Arc<dyn Ui>,
}

impl App {
    pub async fn add(&self, req: AddRequest) -> Result<AddOutcome>;
    pub async fn sync(&self, req: SyncRequest) -> Result<SyncOutcome>;
    pub async fn plan(&self, req: PlanRequest) -> Result<Plan>;
    pub async fn rollback(&self, req: RollbackRequest) -> Result<RollbackOutcome>;
}
```

- `App` 不解析 CLI 参数。
- `Ui` trait 负责进度/确认/输出；测试使用 `RecordingUi`。
- 所有 use case 返回结构化结果；CLI 负责渲染。

## 11. `kita-shim` 的启动细节

```rust
fn main() {
    // 子进程退出码是 u32；用 process::exit 原样返回，避免截断。
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("kita-shim: {e}");
            1
        }
    };
    std::process::exit(code as i32);
}

fn run() -> Result<i32, ShimError> {
    let exe = current_exe()?;
    let name = exe.file_stem()?;
    let root = exe.parent().and_then(Path::parent)?;
    let active = read_active_json(root)?;
    let tool = active.tools.get(name).ok_or(ShimError::UnknownTool(name))?;
    let target = safe_join(root, &active.generation_path, &tool.path)?;
    let env = build_env(&active, tool);
    let child = spawn_suspended(&target, &env)?;
    let job = create_kill_on_close_job()?;
    assign(&job, &child)?;
    resume(&child)?;
    wait(&child)
}
```

错误信息必须行动化：

```text
kita-shim: 当前环境指针损坏（C:\...\active.json）。
请运行：kita doctor --fix
```

## 12. 错误码

| 段 | 范围 | 含义 |
| --- | --- | --- |
| 1xxx | 1000–1999 | 清单/配置 |
| 2xxx | 2000–2999 | 本地状态/锁/generation |
| 3xxx | 3000–3999 | 解析 |
| 4xxx | 4000–4999 | 网络/仓库 |
| 5xxx | 5000–5999 | 校验/安全 |
| 6xxx | 6000–6999 | 权限/提权 |
| 7xxx | 7000–7999 | 锁文件/frozen |
| 8xxx | 8000–8999 | 原生安装器 |
| 9xxx | 9000–9999 | 平台/架构 |
| 10xxx | 10000+ | 内部错误 |

示例：

- `KITA-1002` 未知字段
- `KITA-2004` 计划已过期
- `KITA-3007` 版本冲突
- `KITA-4102` 镜像回退
- `KITA-5003` 哈希不匹配
- `KITA-5007` 签名无效
- `KITA-6001` UAC 被拒绝
- `KITA-7001` 锁文件与清单不一致
- `KITA-8003` MSI 安装失败
- `KITA-9002` 没有匹配架构的构建

每个错误码在文档中有示例、原因、修复步骤。错误码一旦发布不复用。

## 13. 测试策略

### 13.1 单元测试

- 版本解析/排序/需求匹配（含 property test：任意版本 `parse(display(v)) == v`）。
- 路径安全（property test：生成恶意路径，必须拒绝）。
- 树摘要（排序稳定性、大小写冲突）。
- 计划 diff（新增/更新/删除/无变更）。
- 锁文件 round-trip。
- 冲突解释。

### 13.2 集成测试

使用 `kita-testkit`：

```text
TempRoot
FakeHttpRepo（wiremock 或本地文件服务器）
Fixtures:
  - hello-portable.zip
  - hello-msi.msi（仅 Windows CI）
  - signed repo metadata
  - tampered artifact
```

覆盖：

- 首次安装、重复 sync（幂等）、升级、降级、回滚。
- 哈希错误、签名错误、索引回滚。
- 下载中断、镜像切换、离线。
- 文件占用、GC 跳过。
- MSI 安装/卸载（Windows CI + Windows Sandbox）。
- 提权计划校验（不实际 UAC，注入 FakeElevator）。
- 崩溃点注入：在每个 Step 前后模拟 panic/断电，验证恢复。

### 13.3 快照测试

- CLI 人类输出（固定终端宽度/颜色关闭）。
- JSON / JSONL schema。
- 错误渲染。
- 使用 `insta`；更新快照需要 review。

### 13.4 模糊测试

- `cargo-fuzz`：manifest/index TOML、zip/tar 解压、路径解析。
- 目标：无 panic、无越界、无路径逃逸。

### 13.5 Windows 矩阵

| 环境 | 用途 |
| --- | --- |
| Windows 11 24H2 x64 | 主开发/CI |
| Windows 11 24H2 ARM64 | shim/CAS/路径 |
| Windows 10 22H2 x64 | 最低支持 |
| Windows Server 2022 | CI/企业 |
| Windows Sandbox | 干净安装、MSI |
| 受限用户（非管理员） | 用户 scope |
| 企业代理/自定义根证书 | 网络 |

### 13.6 性能测试

- `criterion`：版本解析、树哈希、计划 diff、路径校验。
- 端到端脚本：100 个包、20k 文件、热/冷缓存。
- CI 阈值：超过预算 20% 失败。

## 14. 性能实现要点

- **启动**：避免在 `main` 前构建大静态表；clap 命令懒加载；不在 `--version` 时初始化 Tokio。
- **网络**：连接池、HTTP/2、zstd、ETag；小文件并发，大文件分块（后续）。
- **哈希**：`sha2` 启用 x86 SHA-NI（通过 `cpufeatures` 或 asm feature），失败回退。
- **解压**：流式；固定 64 KiB buffer；避免整文件读入内存。
- **硬链接**：批量 `CreateHardLinkW`；错误聚合；失败回退复制。
- **SQLite**：`journal_mode=WAL`、`synchronous=NORMAL`、`busy_timeout=5000`、预编译语句；网络等待时不持有连接。
- **进度**：每 100 ms 合并一次，避免刷屏。
- **二进制**：`lto = "fat"`、`codegen-units = 1`、`strip = "symbols"`、`panic = "abort"`（CLI）；shim 单独优化 `opt-level = "z"`。
- **依赖**：`cargo bloat` 定期检查；避免为一个小功能引入大 crate。

## 15. 发布与供应链

- CI：`fmt`、`clippy -D warnings`、`test`、`doc`、`cargo deny`、`cargo audit`、MSRV、`cargo build --release`（x64/ARM64）。
- Release：
  - 生成 `SHA256SUMS`；
  - minisign 签名；
  - Authenticode 签名 `kita.exe`、`kita-shim.exe`、`kita-elevated.exe`；
  - 生成 SBOM（`cargo cyclonedx`）；
  - 目标 SLSA provenance。
- 不在 PR CI 中持有签名密钥；release 使用受保护的 environment / HSM。
- 可复现构建：固定 `rust-toolchain.toml`、锁定依赖、`--locked`、确定性打包。
- `kita self update` 只接受签名 channel 元数据；支持回滚到上一版本。

## 16. 代码质量规则

- 所有公开 API 有文档；安全相关函数有 `# Safety` 或不变式说明。
- 禁止 `unwrap()` / `expect()` 在库代码路径；测试可用。
- 禁止 `panic!` 处理用户输入；用 `Result`。
- 禁止在 `kita-core` 中读环境变量/文件。
- 禁止用 `HashMap` 迭代顺序影响输出；一律排序或用 `BTreeMap`。
- 时间、随机数、网络、文件系统通过 trait 注入，便于测试。
- 每个新错误码必须加测试与文档。
- 每个事务步骤必须有对应的崩溃点测试。

## 17. 未来拆分点

当出现以下信号时再拆 crate，避免过早复杂：

- `kita-registry` 超过 8k 行或需要独立发布 → 拆 `kita-index`。
- 支持多种求解器 → `kita-resolver` 定义 trait，PubGrub 作为实现。
- 支持 GUI → `kita-app` 已提供 JSON/结构体 API，GUI 直接复用。
- 支持多平台 → `kita-win` 抽象为 `kita-platform`，Windows 实现保留。
- 支持插件 → 不开放任意脚本；只考虑编译期 feature。
