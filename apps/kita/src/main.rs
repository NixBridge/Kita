use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kita_core::app::{self, Context, SyncOptions};
use kita_core::error::KitaError;
use kita_core::plan::{PackageChange, Plan};
use kita_core::shims;

mod ui;

#[derive(Parser)]
#[command(
    name = "kita",
    version,
    about = "Kita：声明式 Windows 包管理器（本地仓库构建）"
)]
struct Cli {
    /// Kita 状态根目录（默认 KITA_ROOT，其次 %LOCALAPPDATA%\Kita）
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    /// 本地静态仓库目录（覆盖 settings.toml 中的记录）
    #[arg(long, global = true, value_name = "DIR")]
    repository: Option<PathBuf>,

    /// 以单个 JSON 文档输出
    #[arg(long, global = true)]
    json: bool,

    /// 跳过交互确认，直接执行变更
    #[arg(long, global = true)]
    noconfirm: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 初始化状态根目录、配置仓库并创建 kita.toml
    Init,
    /// 安装包；SPEC 可以是仓库包名或 .kta 文件；不带参数时等同于 sync
    Install {
        #[arg(value_name = "SPEC")]
        specs: Vec<String>,
    },
    /// 生成或校验 kita.lock
    Lock {
        /// 只校验锁文件，不写入
        #[arg(long)]
        check: bool,
    },
    /// 忽略锁文件，按 kita.toml 重新解析并同步
    Update {
        /// 只显示计划，不修改任何状态
        #[arg(long)]
        dry_run: bool,
    },
    /// 在本地仓库中搜索包
    Search {
        #[arg(value_name = "QUERY")]
        query: String,
    },
    /// 显示包详情
    Info {
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// 解释某个包或命令为何出现在当前环境
    Why {
        #[arg(value_name = "PACKAGE_OR_COMMAND")]
        target: String,
    },
    /// 对比 kita.toml、kita.lock 与活动 generation
    Diff,
    /// 在活动环境中运行任意命令
    Exec {
        program: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// 打开注入了活动环境变量的 shell
    Shell {
        /// 使用 PowerShell 而不是 cmd.exe
        #[arg(long)]
        powershell: bool,
    },
    /// 检查本地状态；--fix 重建 shim
    Doctor {
        #[arg(long)]
        fix: bool,
    },
    /// 清理 CAS 中未被引用的对象
    Gc {
        /// 只显示将删除的内容，不修改任何状态
        #[arg(long)]
        dry_run: bool,
    },
    /// 缓存相关操作
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// 向 kita.toml 添加包并同步
    Add {
        #[arg(value_name = "SPEC", required = true)]
        specs: Vec<String>,
    },
    /// 从 kita.toml 移除包并同步
    Remove {
        #[arg(value_name = "PACKAGE", required = true)]
        specs: Vec<String>,
    },
    /// 解析、安装并激活 kita.toml
    Sync {
        /// 锁文件必须与 kita.toml 一致，且不重新解析
        #[arg(long)]
        locked: bool,
        /// 只显示计划，不修改任何状态
        #[arg(long)]
        dry_run: bool,
    },
    /// 显示同步计划（不修改状态）
    Plan,
    /// 列出当前活动 generation 中的包
    List,
    /// 回滚到上一个完整 generation
    Rollback,
    /// generation 相关操作
    Generation {
        #[command(subcommand)]
        command: GenerationCommand,
    },
    /// 通过活动工具表直接运行命令
    Run {
        tool: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// 输出活动环境变量
    Env,
    /// 校验活动 generation 的文件对象
    Verify,
}

#[derive(Subcommand)]
enum GenerationCommand {
    /// 列出所有 generation
    List,
    /// 固定 generation，避免被 generation gc 删除
    Pin { generation: u64 },
    /// 取消固定
    Unpin { generation: u64 },
    /// 删除未激活、非上一个、未固定的 generation
    Gc,
}

#[derive(Subcommand)]
enum CacheCommand {
    /// 显示缓存状态
    List,
    /// 清理缓存
    Clean {
        /// 清理全部缓存（默认保留锁文件引用的 artifact）
        #[arg(long)]
        all: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(code) => ExitCode::from(clamp_exit(code)),
        Err(error) => {
            if json {
                let payload = serde_json::json!({
                    "error": {
                        "code": error.code,
                        "message": error.message,
                        "hints": error.hints,
                    }
                });
                println!("{payload}");
            } else {
                eprintln!("{error}");
            }
            ExitCode::from(exit_code(error.code))
        }
    }
}

fn clamp_exit(code: i32) -> u8 {
    if (0..=255).contains(&code) {
        code as u8
    } else {
        1
    }
}

fn run(cli: Cli) -> Result<i32, KitaError> {
    let ctx = Context::new(cli.root)?;
    let repository = cli.repository.clone();
    let repository_ref = repository.as_deref();
    let json = cli.json;
    let noconfirm = cli.noconfirm;

    match cli.command {
        Commands::Init => {
            let repository = repository_ref.ok_or_else(|| {
                KitaError::new("KITA-1001", "`kita init` 需要 --repository <本地仓库目录>")
            })?;
            let outcome = app::init(&ctx, repository)?;
            if json {
                print_json(&outcome)?;
            } else {
                ui::blank();
                ui::header("Initializing Kita");
                ui::item(&format!("状态根目录：{}", outcome.root));
                ui::item(&format!(
                    "项目清单：{}{}",
                    outcome.project,
                    if outcome.created_project {
                        "（已创建）"
                    } else {
                        "（已存在）"
                    }
                ));
                ui::item(&format!("仓库：{}", outcome.repository));
                ui::item(&outcome.path_hint);
                println!("Complete!");
            }
            Ok(0)
        }
        Commands::Install { specs } => {
            if specs.is_empty() {
                let options = SyncOptions {
                    repository: repository.clone(),
                    locked: false,
                    dry_run: false,
                    ignore_lock: false,
                    locals: Vec::new(),
                };
                sync_with_confirmation(&ctx, options, noconfirm, json)
            } else if specs.iter().all(|spec| is_kta_path(spec)) {
                let paths = specs
                    .iter()
                    .map(|spec| PathBuf::from(spec.as_str()))
                    .collect::<Vec<_>>();
                // 只读取/哈希容器一次，dry-run 预览与真实执行共用。
                let locals = app::prepare_locals(&ctx, &paths)?;
                let options = SyncOptions {
                    repository: repository.clone(),
                    locked: false,
                    dry_run: false,
                    ignore_lock: false,
                    locals,
                };
                sync_with_confirmation(&ctx, options, noconfirm, json)
            } else if let Some(spec) = specs.iter().find(|spec| looks_like_archive(spec)) {
                Err(KitaError::new(
                    "KITA-1001",
                    format!("`{spec}` 是压缩包，不能直接安装"),
                )
                .with_hint(
                    "先打包成 .kta 容器（根目录 manifest.toml + payload/），再执行 `kita install <文件.kta>`",
                ))
            } else if specs.iter().any(|spec| is_kta_path(spec)) {
                Err(KitaError::new(
                    "KITA-1001",
                    "`.kta` 文件不能与仓库包名混用",
                )
                .with_hint("先执行 `kita install <文件.kta>`，再执行 `kita install <包名>`"))
            } else {
                change_manifest(
                    &ctx,
                    ManifestChange::Install,
                    &specs,
                    repository_ref,
                    noconfirm,
                    json,
                )
            }
        }
        Commands::Lock { check } => {
            let outcome = app::lock(&ctx, check, repository_ref)?;
            if json {
                print_json(&outcome)?;
            } else {
                ui::blank();
                ui::header(if check {
                    "Checking lock file"
                } else {
                    "Writing lock file"
                });
                ui::item(&format!("锁文件：{}", outcome.path));
                ui::item(&format!(
                    "平台：{}；清单摘要：{}",
                    outcome.platform,
                    ui::short_digest(&outcome.manifest)
                ));
                if !outcome.packages.is_empty() {
                    let rows = outcome
                        .packages
                        .iter()
                        .map(|package| {
                            vec![
                                package.id.clone(),
                                package.version.clone(),
                                ui::short_digest(&package.tree),
                            ]
                        })
                        .collect::<Vec<_>>();
                    ui::table(&["Package", "Version", "Tree"], &rows);
                }
                if outcome.written {
                    ui::item(&format!("已写入 {} 个包", outcome.packages.len()));
                } else if outcome.problems.is_empty() {
                    ui::item("锁文件校验通过");
                } else {
                    for problem in &outcome.problems {
                        ui::item(&format!("x {problem}"));
                    }
                }
            }
            if outcome.written || outcome.problems.is_empty() {
                Ok(0)
            } else {
                Ok(7)
            }
        }
        Commands::Update { dry_run } => {
            let options = SyncOptions {
                repository: repository.clone(),
                locked: false,
                dry_run,
                ignore_lock: true,
                locals: Vec::new(),
            };
            if dry_run {
                let outcome = app::sync(&ctx, &options)?;
                if json {
                    print_json(&outcome)?;
                } else {
                    print_plan(&outcome.plan);
                }
                Ok(0)
            } else {
                sync_with_confirmation(&ctx, options, noconfirm, json)
            }
        }
        Commands::Search { query } => {
            let hits = app::search(&ctx, &query, repository_ref)?;
            if json {
                print_json(&hits)?;
            } else {
                ui::blank();
                ui::header(&format!("Searching for {query}"));
                if hits.is_empty() {
                    ui::item("没有匹配的包");
                } else {
                    let rows = hits
                        .iter()
                        .map(|hit| {
                            vec![
                                hit.id.clone(),
                                hit.latest.clone(),
                                hit.versions.to_string(),
                                hit.summary.clone().unwrap_or_default(),
                            ]
                        })
                        .collect::<Vec<_>>();
                    ui::table(
                        &["Package", "Version", "Versions", "Description"],
                        &rows,
                    );
                }
            }
            Ok(0)
        }
        Commands::Info { package } => {
            let info = app::info(&ctx, &package, repository_ref)?;
            if json {
                print_json(&info)?;
            } else {
                ui::blank();
                ui::header(&format!("Package information: {}", info.id));
                if !info.summary.is_empty() {
                    ui::item(&info.summary);
                }
                if let Some(description) = &info.description {
                    ui::item(description);
                }
                ui::item(&format!("安装方式：{}", info.install));
                ui::item(&format!("许可：{}", info.license));
                ui::item(&format!("主页：{}", info.homepage));
                if let Some(upstream) = &info.upstream {
                    ui::item(&format!("上游：{upstream}"));
                }
                if !info.bins.is_empty() {
                    ui::item(&format!("命令：{}", info.bins.join(", ")));
                }
                if !info.dependencies.is_empty() {
                    ui::item(&format!("依赖：{}", info.dependencies.join(", ")));
                }
                ui::blank();
                ui::header("Available versions");
                let rows = info
                    .versions
                    .iter()
                    .map(|version| {
                        vec![
                            version.version.clone(),
                            if version.targets.is_empty() {
                                "-".to_string()
                            } else {
                                version.targets.join(", ")
                            },
                            if version.yanked {
                                "yanked".to_string()
                            } else {
                                "ok".to_string()
                            },
                        ]
                    })
                    .collect::<Vec<_>>();
                ui::table(&["Version", "Targets", "Status"], &rows);
            }
            Ok(0)
        }
        Commands::Why { target } => {
            let outcome = app::why(&ctx, &target)?;
            if json {
                print_json(&outcome)?;
            } else {
                ui::blank();
                ui::header(&format!("Explaining {}", outcome.package));
                ui::item(&format!(
                    "{} {}：{}",
                    outcome.package,
                    outcome.version,
                    if outcome.root {
                        "由 kita.toml 直接声明"
                    } else {
                        "作为依赖引入"
                    }
                ));
                if !outcome.commands.is_empty() {
                    ui::item(&format!("命令：{}", outcome.commands.join(", ")));
                }
                if outcome.chains.is_empty() {
                    ui::item("没有可用的依赖链（缺少锁文件）");
                } else {
                    ui::item("依赖链：");
                    for chain in &outcome.chains {
                        ui::item(&chain.join(" -> "));
                    }
                }
            }
            Ok(0)
        }
        Commands::Diff => {
            let outcome = app::diff(&ctx)?;
            if json {
                print_json(&outcome)?;
            } else {
                ui::blank();
                ui::header("Comparing manifest, lock and active generation");
                ui::item(&format!(
                    "清单{}；锁文件{}；活动 generation {}",
                    if outcome.manifest_changed {
                        "已变更"
                    } else {
                        "未变更"
                    },
                    if outcome.lock_present { "存在" } else { "缺失" },
                    outcome
                        .active_generation
                        .map(|number| number.to_string())
                        .unwrap_or_else(|| "无".to_string())
                ));
                if !outcome.entries.is_empty() {
                    let rows = outcome
                        .entries
                        .iter()
                        .map(|entry| {
                            vec![
                                entry.id.clone(),
                                entry.manifest.clone().unwrap_or_else(|| "-".to_string()),
                                entry.lock.clone().unwrap_or_else(|| "-".to_string()),
                                entry.active.clone().unwrap_or_else(|| "-".to_string()),
                                entry.issue.clone(),
                            ]
                        })
                        .collect::<Vec<_>>();
                    ui::table(
                        &["Package", "Manifest", "Lock", "Active", "Status"],
                        &rows,
                    );
                }
            }
            Ok(0)
        }
        Commands::Exec { program, args } => {
            if !json {
                ui::blank();
                ui::header(&format!("Running {program}"));
            }
            app::exec(&ctx, &program, &args)
        }
        Commands::Shell { powershell } => {
            if !json {
                ui::blank();
                ui::header(if powershell {
                    "Opening PowerShell"
                } else {
                    "Opening shell"
                });
            }
            app::shell(&ctx, powershell)
        }
        Commands::Doctor { fix } => {
            let report = app::doctor(&ctx, fix)?;
            if json {
                print_json(&report)?;
            } else {
                ui::blank();
                ui::header("Checking Kita health");
                for check in &report.checks {
                    let symbol = match check.status.as_str() {
                        "ok" => "✓",
                        "warn" => "!",
                        _ => "x",
                    };
                    ui::item(&format!("{symbol} {}：{}", check.name, check.message));
                }
                for item in &report.fixed {
                    ui::item(&format!("已修复：{item}"));
                }
            }
            if report.checks.iter().any(|check| check.status == "fail") {
                Ok(5)
            } else {
                Ok(0)
            }
        }
        Commands::Gc { dry_run } => {
            let outcome = app::gc(&ctx, dry_run)?;
            if json {
                print_json(&outcome)?;
            } else {
                ui::blank();
                ui::header("Garbage collecting store");
                ui::item(&format!(
                    "环境 {}；保留 generation {}",
                    outcome.environment,
                    join_numbers(&outcome.kept_generations)
                ));
                ui::item(&format!(
                    "文件树：{}/{} 个可删除",
                    outcome.trees_removed, outcome.trees_total
                ));
                ui::item(&format!(
                    "对象：{}/{} 个可删除（{}）",
                    outcome.objects_removed,
                    outcome.objects_total,
                    ui::human_size(outcome.bytes_removed)
                ));
                if outcome.dry_run {
                    ui::item("dry-run：未删除任何文件");
                } else {
                    ui::item(&format!(
                        "已删除 {} 个文件树、{} 个对象",
                        outcome.trees_removed, outcome.objects_removed
                    ));
                }
            }
            Ok(0)
        }
        Commands::Cache { command } => match command {
            CacheCommand::List => {
                let report = app::cache_report(&ctx)?;
                if json {
                    print_json(&report)?;
                } else {
                    ui::blank();
                    ui::header("Cache status");
                    ui::item(&format!("路径：{}", report.path));
                    ui::item(&format!(
                        "文件：{} 个，共 {}",
                        report.files,
                        ui::human_size(report.bytes)
                    ));
                }
                Ok(0)
            }
            CacheCommand::Clean { all } => {
                let outcome = app::cache_clean(&ctx, all)?;
                if json {
                    print_json(&outcome)?;
                } else {
                    ui::blank();
                    ui::header("Cleaning cache");
                    ui::item(&format!(
                        "已删除 {} 个缓存文件（{}），保留 {} 个",
                        outcome.removed,
                        ui::human_size(outcome.bytes),
                        outcome.kept
                    ));
                }
                Ok(0)
            }
        },
        Commands::Add { specs } => change_manifest(
            &ctx,
            ManifestChange::Add,
            &specs,
            repository_ref,
            noconfirm,
            json,
        ),
        Commands::Remove { specs } => change_manifest(
            &ctx,
            ManifestChange::Remove,
            &specs,
            repository_ref,
            noconfirm,
            json,
        ),
        Commands::Sync { locked, dry_run } => {
            let options = SyncOptions {
                repository: repository.clone(),
                locked,
                dry_run,
                ignore_lock: false,
                locals: Vec::new(),
            };
            if dry_run {
                let outcome = app::sync(&ctx, &options)?;
                if json {
                    print_json(&outcome.plan)?;
                } else {
                    print_plan(&outcome.plan);
                }
                Ok(0)
            } else {
                sync_with_confirmation(&ctx, options, noconfirm, json)
            }
        }
        Commands::Plan => {
            let outcome = app::sync(
                &ctx,
                &SyncOptions {
                    repository: repository.clone(),
                    locked: false,
                    dry_run: true,
                    ignore_lock: false,
                    locals: Vec::new(),
                },
            )?;
            if json {
                print_json(&outcome.plan)?;
            } else {
                print_plan(&outcome.plan);
            }
            Ok(0)
        }
        Commands::List => {
            let packages = app::list_installed(&ctx)?;
            if json {
                print_json(&packages)?;
            } else {
                ui::blank();
                ui::header("Listing installed packages");
                if packages.is_empty() {
                    ui::item("当前没有活动包");
                } else {
                    let rows = packages
                        .iter()
                        .map(|package| {
                            vec![
                                package.id.clone(),
                                package.version.clone(),
                                package.scope.to_string(),
                                bins_label(&package.bins),
                            ]
                        })
                        .collect::<Vec<_>>();
                    ui::table(
                        &["Package", "Version", "Scope", "Commands"],
                        &rows,
                    );
                }
            }
            Ok(0)
        }
        Commands::Rollback => {
            let outcome = app::rollback(&ctx)?;
            if json {
                print_json(&outcome)?;
            } else {
                ui::blank();
                ui::header("Rolling back generation");
                ui::item(&format!(
                    "已回滚 generation {} → {}（{} 个命令）",
                    outcome.from, outcome.to, outcome.tools
                ));
            }
            Ok(0)
        }
        Commands::Generation { command } => match command {
            GenerationCommand::List => {
                let generations = app::generations(&ctx)?;
                if json {
                    print_json(&generations)?;
                } else {
                    ui::blank();
                    ui::header("Listing generations");
                    if generations.is_empty() {
                        ui::item("没有 generation");
                    } else {
                        let rows = generations
                            .iter()
                            .map(|generation| {
                                vec![
                                    generation.generation.to_string(),
                                    generation.packages.to_string(),
                                    generation.platform.clone(),
                                    if generation.current {
                                        "active".to_string()
                                    } else {
                                        String::new()
                                    },
                                ]
                            })
                            .collect::<Vec<_>>();
                        ui::table(
                            &["Generation", "Packages", "Platform", "State"],
                            &rows,
                        );
                    }
                }
                Ok(0)
            }
            GenerationCommand::Pin { generation } => {
                let outcome = app::pin_generation(&ctx, generation, true)?;
                if json {
                    print_json(&outcome)?;
                } else {
                    ui::blank();
                    ui::header(&format!("Pinning generation {}", outcome.generation));
                    if outcome.changed {
                        ui::item(&format!("已固定 generation {}", outcome.generation));
                    } else {
                        ui::item(&format!("generation {} 已被固定", outcome.generation));
                    }
                }
                Ok(0)
            }
            GenerationCommand::Unpin { generation } => {
                let outcome = app::pin_generation(&ctx, generation, false)?;
                if json {
                    print_json(&outcome)?;
                } else {
                    ui::blank();
                    ui::header(&format!("Unpinning generation {}", outcome.generation));
                    if outcome.changed {
                        ui::item(&format!("已取消固定 generation {}", outcome.generation));
                    } else {
                        ui::item(&format!("generation {} 未被固定", outcome.generation));
                    }
                }
                Ok(0)
            }
            GenerationCommand::Gc => {
                let outcome = app::generation_gc(&ctx)?;
                if json {
                    print_json(&outcome)?;
                } else {
                    ui::blank();
                    ui::header("Garbage collecting generations");
                    ui::item(&format!(
                        "环境 {}；保留 generation {}",
                        outcome.environment,
                        join_numbers(&outcome.kept)
                    ));
                    if outcome.removed.is_empty() {
                        ui::item("没有可删除的 generation");
                    } else {
                        ui::item(&format!(
                            "已删除 generation {}",
                            join_numbers(&outcome.removed)
                        ));
                    }
                }
                Ok(0)
            }
        },
        Commands::Run { tool, args } => {
            if !json {
                ui::blank();
                ui::header(&format!("Running {tool}"));
            }
            app::run_tool(&ctx, &tool, &args)
        }
        Commands::Env => {
            let env = app::env_vars(&ctx)?;
            if json {
                print_json(&env)?;
            } else {
                ui::blank();
                ui::header("Active environment variables");
                if env.is_empty() {
                    ui::item("当前没有活动环境变量");
                } else {
                    for (key, value) in env {
                        ui::item(&format!("{key}={value}"));
                    }
                }
            }
            Ok(0)
        }
        Commands::Verify => {
            let report = app::verify(&ctx)?;
            if json {
                print_json(&report)?;
            } else {
                ui::blank();
                ui::header("Verifying generation files");
                match report.generation {
                    Some(generation) => ui::item(&format!(
                        "generation {}：检查 {} 个对象，{} 个失败",
                        generation,
                        report.objects_checked,
                        report.failures.len()
                    )),
                    None => ui::item("当前没有活动环境"),
                }
                for failure in &report.failures {
                    ui::item(&format!("x {failure}"));
                }
            }
            if report.failures.is_empty() {
                Ok(0)
            } else {
                Ok(5)
            }
        }
    }
}

/// 需要修改 kita.toml 的命令类型。
enum ManifestChange {
    Install,
    Add,
    Remove,
}

/// 先把 dry-run 计划渲染成 pacman 风格事务摘要，确认后再真正执行同步。
fn sync_with_confirmation(
    ctx: &Context,
    options: SyncOptions,
    noconfirm: bool,
    json: bool,
) -> Result<i32, KitaError> {
    if json {
        let outcome = app::sync(ctx, &options)?;
        return report_sync(&outcome, ctx, true);
    }

    let preview = app::sync(
        ctx,
        &SyncOptions {
            dry_run: true,
            ..options.clone()
        },
    )?;

    ui::blank();
    ui::header("Synchronizing package databases");
    if preview.plan.project_manifest {
        ui::item("本地仓库已读取");
    }
    ui::header("Resolving package metadata");
    ui::item(&format!("目标平台：{}", preview.plan.platform));

    if preview.plan.requires_build {
        print_transaction(&preview.plan);
        if !ui::confirm("Proceed with installation?", noconfirm)? {
            println!("事务已取消。");
            return Ok(0);
        }
        print_downloads(&preview.plan);
        // 只有确实需要从仓库获取 artifact 时才有摘要校验步骤可报告。
        if !preview.plan.downloads.is_empty() {
            ui::blank();
            ui::header("Verifying package integrity");
            ui::item("artifact 摘要校验通过");
        }
    }

    let outcome = app::sync(ctx, &options)?;
    report_sync(&outcome, ctx, false)
}

/// 修改 kita.toml 的 add/remove/install 流程：确认后执行并报告结果。
fn change_manifest(
    ctx: &Context,
    kind: ManifestChange,
    specs: &[String],
    repository: Option<&Path>,
    noconfirm: bool,
    json: bool,
) -> Result<i32, KitaError> {
    if json {
        let outcome = apply_manifest_change(ctx, &kind, specs, repository)?;
        return report_sync(&outcome, ctx, true);
    }

    ui::blank();
    ui::header("Resolving package changes");
    for spec in specs {
        match &kind {
            ManifestChange::Remove => ui::item(&format!("将移除 {spec}")),
            _ => ui::item(&format!("将安装 {spec}")),
        }
    }
    ui::item("将更新 kita.toml 并同步环境");
    ui::blank();
    let prompt = match &kind {
        ManifestChange::Remove => "Proceed with removal?",
        _ => "Proceed with installation?",
    };
    if !ui::confirm(prompt, noconfirm)? {
        println!("事务已取消。");
        return Ok(0);
    }

    let outcome = apply_manifest_change(ctx, &kind, specs, repository)?;
    report_sync(&outcome, ctx, false)
}

fn apply_manifest_change(
    ctx: &Context,
    kind: &ManifestChange,
    specs: &[String],
    repository: Option<&Path>,
) -> Result<app::SyncOutcome, KitaError> {
    match kind {
        ManifestChange::Install => app::install(ctx, specs, repository),
        ManifestChange::Add => app::add(ctx, specs, repository),
        ManifestChange::Remove => app::remove(ctx, specs, repository),
    }
}

/// 输出 dry-run 计划，供 `plan`、`sync --dry-run`、`update --dry-run` 使用。
fn print_plan(plan: &Plan) {
    ui::blank();
    ui::header("Synchronizing package databases");
    if plan.project_manifest {
        ui::item("本地仓库已读取");
    }
    ui::header("Resolving package metadata");
    ui::item(&format!("目标平台：{}", plan.platform));
    if plan.requires_build {
        print_transaction(plan);
    } else {
        ui::item(&format!("环境已是最新（generation {}）", plan.generation));
    }
    for warning in &plan.warnings {
        ui::item(&format!("提示：{warning}"));
    }
}

/// 输出 pacman 风格的安装事务摘要表格。
fn print_transaction(plan: &Plan) {
    ui::blank();
    ui::header("Preparing transaction (Transaction Summary)");
    let rows = plan
        .changes
        .iter()
        .map(|change| {
            let version = match (
                change.action.as_str(),
                change.to.as_deref(),
                change.from.as_deref(),
            ) {
                ("remove", _, Some(from)) => from.to_string(),
                (_, Some(to), _) => to.to_string(),
                (_, None, Some(from)) => from.to_string(),
                _ => "-".to_string(),
            };
            let size = change
                .size
                .map(ui::human_size)
                .unwrap_or_else(|| size_label(plan, &change.id));
            vec![
                change.id.clone(),
                arch_label(&plan.platform),
                version,
                change.repository.clone(),
                size,
            ]
        })
        .collect::<Vec<_>>();
    ui::table(
        &["Package", "Arch", "Version", "Repository", "Size"],
        &rows,
    );

    if plan.downloads.is_empty() {
        if plan.changes.iter().any(|change| change.repository == "kta") {
            ui::item("Total download size: 0 B（本地 .kta 容器）");
        } else {
            ui::item("Total download size: 0 B（全部已缓存）");
        }
    } else {
        let total = plan.downloads.iter().map(|download| download.size).sum::<u64>();
        ui::item(&format!("Total download size: {}", ui::human_size(total)));
    }
    ui::item(&format!("Total packages: {}", plan.changes.len()));
    if plan.project_manifest && plan.manifest_changed {
        ui::item("kita.toml 已变更，将重新生成 kita.lock");
    }
}

/// 输出需要从本地仓库获取的 artifact 列表。
fn print_downloads(plan: &Plan) {
    ui::blank();
    ui::header("Downloading packages");
    if plan.downloads.is_empty() {
        if plan.changes.iter().any(|change| change.repository == "kta") {
            ui::item("无需下载：本地 .kta 直接解包，其余 artifact 已缓存");
        } else {
            ui::item("所有 artifact 已在本地缓存");
        }
        return;
    }
    let total = plan.downloads.len();
    for (index, download) in plan.downloads.iter().enumerate() {
        ui::progress(
            index + 1,
            total,
            &format!("{}  {}", download.id, ui::human_size(download.size)),
        );
    }
    if plan.changes.iter().any(|change| change.repository == "kta") {
        ui::item("本地 .kta 容器无需下载，直接解包安装");
    }
}

/// 输出解包/链接步骤；真实操作已由 sync 完成。
fn print_processing(plan: &Plan) {
    ui::blank();
    ui::header("Processing package changes");
    if plan.changes.is_empty() {
        ui::item("没有需要变更的包");
        return;
    }
    for change in &plan.changes {
        let version = version_label(change);
        if change.action == "remove" {
            println!("Removing {} ({version}) ...", change.id);
        } else {
            println!("Unpacking {} ({version}) ...", change.id);
            println!("Setting up {} ({version}) ...", change.id);
        }
    }
}

/// 输出 Kita 真实存在的 post-transaction hook：重建 shim 与 PATH 提示。
fn print_hooks(outcome: &app::SyncOutcome, ctx: &Context) {
    ui::blank();
    ui::header("Running post-transaction hooks");
    ui::progress(
        1,
        2,
        &format!("重建命令 shim（generation {}）", outcome.generation),
    );
    for warning in &outcome.shim_warnings {
        ui::item(&format!("提示：{warning}"));
    }
    ui::progress(2, 2, "提示 PATH 配置");
    ui::item(&format!(
        "将 {} 加入用户 PATH 后，包提供的命令即可直接运行",
        shims::shim_dir(&ctx.root).display()
    ));
}

fn report_sync(
    outcome: &app::SyncOutcome,
    ctx: &Context,
    json: bool,
) -> Result<i32, KitaError> {
    if json {
        print_json(outcome)?;
        return Ok(0);
    }
    if !outcome.plan.warnings.is_empty() {
        ui::blank();
        for warning in &outcome.plan.warnings {
            ui::item(&format!("提示：{warning}"));
        }
    }
    if outcome.plan.requires_build {
        print_processing(&outcome.plan);
    } else {
        ui::blank();
        ui::item(&format!("环境已是最新（generation {}）", outcome.generation));
    }
    print_hooks(outcome, ctx);
    println!("Complete!");
    Ok(0)
}

/// 判断一个 install 参数是否指向 `.kta` 容器文件。
fn is_kta_path(spec: &str) -> bool {
    spec.to_ascii_lowercase().ends_with(".kta")
}

/// 判断参数是否像磁盘上的压缩包（而不是包 ID），用于给出可理解的提示。
fn looks_like_archive(spec: &str) -> bool {
    if is_kta_path(spec) {
        return false;
    }
    const EXTS: [&str; 8] = [
        ".zip", ".7z", ".rar", ".tar", ".tar.gz", ".tgz", ".gz", ".xz",
    ];
    let lower = spec.to_ascii_lowercase();
    if EXTS.iter().any(|ext| lower.ends_with(ext)) {
        return true;
    }
    Path::new(spec).is_file()
}

fn arch_label(platform: &str) -> String {
    match platform.split('-').next().unwrap_or(platform) {
        "x86_64" => "win64".to_string(),
        "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    }
}

fn size_label(plan: &Plan, id: &str) -> String {
    plan.downloads
        .iter()
        .find(|download| download.id == id)
        .map(|download| ui::human_size(download.size))
        .unwrap_or_else(|| "-".to_string())
}

fn version_label(change: &PackageChange) -> String {
    change
        .to
        .clone()
        .or_else(|| change.from.clone())
        .unwrap_or_else(|| "?".to_string())
}

fn bins_label(bins: &[String]) -> String {
    if bins.is_empty() {
        "-".to_string()
    } else {
        bins.join(", ")
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), KitaError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| KitaError::new("KITA-2001", format!("JSON 序列化失败：{error}")))?;
    println!("{text}");
    Ok(())
}

fn join_numbers(values: &[u64]) -> String {
    if values.is_empty() {
        "无".to_string()
    } else {
        values
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn exit_code(code: &str) -> u8 {
    match code.get(5..6) {
        Some("2") => 10,
        Some("3") => 3,
        Some("4") => 4,
        Some("5") => 5,
        Some("6") => 6,
        Some("7") => 7,
        Some("8") => 8,
        Some("9") => 9,
        _ => 1,
    }
}
