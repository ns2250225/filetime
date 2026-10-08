# FileTime（文件时光机）

<p align="center">
  <img src="assets/filetime-icon.png" width="160" alt="FileTime 图标">
</p>

FileTime 是一款面向 Windows 的本地单文件版本管理工具。无需学习 Git，也无需创建仓库，直接在文件资源管理器中对任意普通文件创建版本、提交快照、查看历史、比较或安全回滚。

数据仅保存在本机 `%LOCALAPPDATA%\FileTime`，无需账号或云服务。

## 主要功能

- **单文件版本管理**：文档、代码、图片、视频、压缩包和二进制文件均可管理。
- **Windows 11 原生右键菜单**：基于 `IExplorerCommand` COM 扩展和稀疏 MSIX 身份包，直接显示在现代一级右键菜单中。
- **最近版本快速回滚**：右键菜单直接列出最近三个版本，确认后即可恢复。
- **安全回滚**：替换前保存未提交内容，校验目标快照，并通过 `ReplaceFileW` 原子替换原文件。
- **内容寻址与去重**：以 SHA-256 标识快照，相同内容复用同一对象。
- **版本比较**：文本和常见代码文件提供逐行 Diff，其他文件显示大小及哈希差异。
- **便携归档**：将当前文件、全部历史和元数据导出为 `.filetime`，可在另一台电脑导入。
- **完整性检查**：检查 SQLite 数据库以及每个历史对象的大小和哈希。
- **系统托盘驻留**：关闭窗口时自动隐藏；重复启动只唤醒现有实例，托盘始终保留一个图标。
- **单 EXE 交付**：主程序使用 Windows 图形子系统，运行时没有黑色控制台窗口。

## 环境要求

### 运行

- Windows 10 或 Windows 11 x64
- Windows 11 原生右键菜单首次安装需要 UAC 授权，用于安装本地签名证书和身份包

### 构建

- Rust stable（MSVC 工具链）
- Visual Studio 2022 Build Tools，包含“使用 C++ 的桌面开发”
- Windows 10/11 SDK（当前脚本使用 `10.0.26100.0`）
- PowerShell 7 或 Windows PowerShell

项目的 `.cargo/config.toml` 已配置 rsproxy 国内镜像，Cargo 依赖会从国内源安装。

## 构建单文件 EXE

```powershell
git clone https://github.com/ns2250225/filetime.git
cd filetime
powershell -NoProfile -ExecutionPolicy Bypass -File .\installer\windows\build-single.ps1
```

输出文件：

```text
target\release\filetime.exe
```

构建脚本会依次完成：

1. 编译 Windows Shell COM 扩展。
2. 生成并签名稀疏 MSIX 身份包。
3. 将 COM DLL、MSIX 和证书嵌入主程序。
4. 嵌入多分辨率应用图标。
5. 生成无控制台窗口的 Release EXE。

## 快速开始

启动 `filetime.exe` 后，程序会显示主窗口并驻留系统托盘。

在主界面点击“安装资源管理器右键菜单”，或运行：

```powershell
.\filetime.exe install-menu
```

首次安装时需同意 UAC 提示。安装完成后，在资源管理器中右键任意文件：

- 未管理文件会显示“创建版本管理”。
- 已管理文件会显示提交、历史、最近版本快速回滚、普通回滚、比较、导出归档、信息和停止管理。
- `.filetime` 文件会显示“导入便携归档”。

如需移除右键菜单：

```powershell
.\filetime.exe uninstall-menu
```

移除菜单不会删除历史版本数据。

## 图形界面与托盘

- 点击窗口关闭按钮：隐藏主窗口，程序继续驻留托盘。
- 点击托盘图标或“打开 FileTime”：恢复主窗口。
- 托盘右键选择“退出”：完全关闭程序。
- 再次运行 EXE：唤醒已存在的窗口，不创建第二个托盘实例。

## 命令行用法

```powershell
# 创建管理并保存初始版本 v1
filetime init "D:\文档\报告.docx"

# 提交新版本
filetime commit "D:\文档\报告.docx" --message "修改第三章"

# 查看历史
filetime history "D:\文档\报告.docx"

# 回滚到 v1（会替换当前文件）
filetime rollback "D:\文档\报告.docx" 1 --yes

# 比较两个版本
filetime compare "D:\文档\报告.docx" 1 2

# 导出单个历史版本
filetime export "D:\文档\报告.docx" 1 "D:\导出\报告-v1.docx"

# 导出、导入便携归档
filetime export-archive "D:\文档\报告.docx" "D:\导出\报告.filetime"
filetime import-archive "D:\导出\报告.filetime" "D:\恢复\报告.docx"

# 校验历史数据
filetime verify "D:\文档\报告.docx"

# 停止和恢复管理
filetime stop "D:\文档\报告.docx" --yes
filetime resume "D:\文档\报告.docx"

# 文件移动后手动更新路径
filetime relocate "D:\旧路径\报告.docx" "D:\新路径\报告.docx"
```

`stop --delete-history --yes` 会删除该文件的历史记录及无引用快照，但保留原文件。

可通过 `FILETIME_HOME` 环境变量指定自定义数据目录。

## 数据存储

默认目录结构：

```text
%LOCALAPPDATA%\FileTime\
├── filetime.db
├── repository\objects\
├── integration\
└── gui-requests\
```

- `filetime.db`：文件、版本和对象元数据。
- `repository\objects`：按 SHA-256 保存的完整文件快照。
- `integration`：安装右键菜单时释放的程序、COM 扩展和身份包。
- `gui-requests`：单实例窗口唤醒请求，处理后自动清理。

回滚不会删除旧版本。若当前文件存在未提交修改，FileTime 会先创建安全版本，再恢复目标内容。

## 便携归档

`.filetime` 归档包含：

- 当前文件内容；
- 全部历史版本；
- 版本说明与提交时间；
- 文件大小与 SHA-256；
- 回滚来源和操作类型。

导入时会逐项校验归档内容，验证通过后才写入目标文件和本地版本仓库。

## 技术栈

| 模块 | 技术 |
| --- | --- |
| 核心与 CLI | Rust |
| 桌面界面 | Slint |
| 元数据 | SQLite / rusqlite |
| 文件校验 | SHA-256 |
| 文本比较 | similar |
| 便携归档 | ZIP |
| Windows 集成 | Win32、COM、IExplorerCommand、稀疏 MSIX |

## 项目结构

```text
filetime/
├── assets/                 # 应用图标和 MSIX 图标
├── installer/windows/      # 身份包清单与单文件构建脚本
├── shell-extension/        # Windows 11 现代右键菜单 COM 扩展
├── src/
│   ├── gui.rs              # Slint 界面、托盘与单实例逻辑
│   ├── main.rs             # CLI 入口和右键命令分发
│   ├── shell.rs            # 右键菜单安装、卸载和内嵌资源释放
│   └── store.rs            # SQLite、对象存储、回滚和归档
├── ui/app.slint            # 中文桌面界面
├── build.rs                # Slint、EXE 图标和内嵌资源构建
├── prd.md                  # 产品需求文档
└── Cargo.toml
```

## 测试

```powershell
cargo test --features bundled-shell
```

Shell 扩展的测试程序位于 `shell-extension/Smoke.cpp`，用于验证直接加载和已注册 COM 两种调用路径。

## 安全说明

- 历史快照写入后不会原地修改。
- 快照在导出、校验和恢复前会检查大小与 SHA-256。
- 回滚在原文件所在目录创建临时文件，再调用 Windows 安全替换接口。
- SQLite 写入使用事务；无引用对象可在删除版本时进行垃圾回收。
- 右键菜单扩展只负责查询状态和启动命令，文件操作由独立的 Rust 进程完成。

当前身份包使用本地自签名证书，适合本机安装和开发测试。公开分发时应替换为受信任的代码签名证书。

## 当前范围

已实现 PRD 中的核心版本管理功能、Windows 11 原生现代右键菜单、14.3 文件版本便携化和 14.4 文件右键快速回滚。

自动提交、图片叠加比较、二进制增量存储和完整设置中心尚未实现。

详细产品设计见 [prd.md](prd.md)。
