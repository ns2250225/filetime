#![cfg_attr(windows, windows_subsystem = "windows")]

mod store;
mod shell;
mod gui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "FileTime", version, about = "单文件版本管理")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Init { path: PathBuf },
    Commit { path: PathBuf, #[arg(short, long)] message: Option<String>, #[arg(long)] force: bool },
    History { path: PathBuf },
    Rollback { path: PathBuf, version: Option<i64>, #[arg(long)] yes: bool },
    Compare { path: PathBuf, left: Option<i64>, right: Option<i64> },
    Export { path: PathBuf, version: i64, destination: PathBuf },
    ExportArchive { path: PathBuf, destination: PathBuf },
    ImportArchive { archive: PathBuf, destination: PathBuf },
    DeleteVersion { path: PathBuf, version: i64, #[arg(long)] yes: bool },
    Stop { path: PathBuf, #[arg(long)] delete_history: bool, #[arg(long)] yes: bool },
    Resume { path: PathBuf },
    Relocate { old_path: PathBuf, new_path: PathBuf },
    Info { path: PathBuf },
    List,
    Verify { path: Option<PathBuf> },
    InstallMenu,
    UninstallMenu,
    Gui { path: Option<PathBuf> },
    #[command(hide = true)]
    ShellAction { action: String, path: PathBuf, version: Option<i64> },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("错误：{error:#}");
        #[cfg(windows)] show_message(&format!("错误：{error:#}"),false);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Command::InstallMenu) => shell::install(),
        Some(Command::UninstallMenu) => shell::uninstall(),
        Some(Command::Gui { path }) => gui::run(path),
        Some(Command::ShellAction { action, path, version }) => shell_action(&action,&path,version),
        command => {
            let mut store = store::Store::open()?;
            match command {
                Some(Command::Init { path }) => println!("{}", store.init(&path)?),
                Some(Command::Commit { path, message, force }) => println!("{}", store.commit(&path, message.as_deref().unwrap_or("新版本"), force)?),
                Some(Command::History { path }) => {
                    for v in store.history(&path)? { println!("v{}\t{}\t{} bytes\t{}\t{}", v.number, v.created_at, v.size, v.hash, v.description); }
                }
                Some(Command::Rollback { path, version, yes }) => {
                    if let Some(version) = version {
                        anyhow::ensure!(yes, "回滚将替换文件内容，请加 --yes 确认，或使用图形界面");
                        println!("{}", store.rollback(&path, version)?);
                    } else { return gui::run(Some(path)); }
                }
                Some(Command::Compare { path, left, right }) => {
                    if let (Some(left), Some(right)) = (left, right) { println!("{}", store.compare(&path, left, right, false)?); }
                    else { return gui::run(Some(path)); }
                }
                Some(Command::Export { path, version, destination }) => store.export(&path, version, &destination)?,
                Some(Command::ExportArchive { path, destination }) => { store.export_archive(&path,&destination)?; println!("已导出 {}",destination.display()); },
                Some(Command::ImportArchive { archive, destination }) => println!("{}",store.import_archive(&archive,&destination)?),
                Some(Command::DeleteVersion { path, version, yes }) => { anyhow::ensure!(yes, "删除历史版本请加 --yes 确认"); store.delete_version(&path, version)?; },
                Some(Command::Stop { path, delete_history, yes }) => { anyhow::ensure!(yes, "停止管理请加 --yes 确认"); store.stop(&path, delete_history)?; },
                Some(Command::Resume { path }) => store.resume(&path)?,
                Some(Command::Relocate { old_path, new_path }) => store.relocate(&old_path, &new_path)?,
                Some(Command::Info { path }) => println!("{}", serde_json::to_string_pretty(&store.info(&path)?)?),
                Some(Command::List) => println!("{}", serde_json::to_string_pretty(&store.files()?)?),
                Some(Command::Verify { path }) => println!("{}", store.verify(path.as_deref())?),
                None => return gui::run(None),
                _ => unreachable!(),
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
fn show_message(message:&str, confirm:bool) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW,MB_ICONINFORMATION,MB_ICONWARNING,MB_OK,MB_OKCANCEL,IDOK};
    let wide = |s:&str| std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let flags = if confirm { MB_OKCANCEL | MB_ICONWARNING } else { MB_OK | MB_ICONINFORMATION };
    unsafe { MessageBoxW(std::ptr::null_mut(),wide(message).as_ptr(),wide("FileTime").as_ptr(),flags) == IDOK }
}

fn shell_action(action:&str,path:&std::path::Path,version:Option<i64>) -> Result<()> {
    let mut store=store::Store::open()?;
    let result = match action {
        "init" => store.init(path)?,
        "commit" | "history" | "rollback" | "compare" | "info" | "stop" => return gui::run(Some(path.to_path_buf())),
        "quick-rollback" => {
            let number=version.ok_or_else(||anyhow::anyhow!("缺少目标版本"))?;
            let target=store.history(path)?.into_iter().find(|v|v.number==number).ok_or_else(||anyhow::anyhow!("版本不存在"))?;
            #[cfg(windows)] if !show_message(&format!("将当前文件替换为 v{}？\n{}\n{} 字节\n{}\n\n未提交内容会先保存为安全版本。",target.number,target.created_at,target.size,target.description),true) { return Ok(()); }
            store.rollback(path,number)?
        }
        "export-archive" => {
            let dest=path.with_extension("filetime");
            store.export_archive(path,&dest)?;
            format!("已导出至 {}",dest.display())
        }
        "import-archive" => {
            let dest=store::Store::suggested_import_path(path)?;
            #[cfg(windows)] if !show_message(&format!("从归档导入到以下位置？\n{}",dest.display()),true) { return Ok(()); }
            let message=store.import_archive(path,&dest)?;
            format!("{message}\n{}",dest.display())
        }
        _ => anyhow::bail!("未知右键操作"),
    };
    #[cfg(windows)] show_message(&result,false);
    Ok(())
}
