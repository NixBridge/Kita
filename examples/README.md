# 示例说明

本目录用于评审 Kita 的文件格式。

- `kita.toml`：项目声明（意图层）。
- `kita.lock`：解析结果（事实层）。
- `packages/ripgrep.toml`：portable 包清单。
- `packages/example-msi.toml`：原生 MSI 包清单。
- `repository.toml`：仓库元数据。
- `local-repo/`：**可安装**的本地示例仓库（`kita/hello` 与真实的 `kita/fastfetch` 2.64.2，portable zip，哈希为真实值）。

本地试用：

```powershell
cargo run -p kita-cli -- init --repository examples/local-repo
cargo run -p kita-cli -- add kita/hello
cargo run -p kita-cli -- run hello -- hi

cargo run -p kita-cli -- add kita/fastfetch
cargo run -p kita-cli -- run fastfetch --version
```

注意：

1. 除 `local-repo/` 外，所有 SHA-256 均为占位值（重复字符），不是真实文件哈希。
2. `example-msi.toml` 中的 GUID、域名、发布者都是虚构的。
3. 实际发布时，包清单会被签名索引固定；锁文件中的哈希必须与真实 artifact 一致。
4. 锁文件中的 `manifest` digest 对应 `kita.toml` 的规范化语义，不是原始文本的哈希。

格式的权威定义见 [包模型](../docs/03-package-model.md)。
