use anyhow::{Result, ensure};
#[cfg(all(windows,feature="bundled-shell"))]
use std::{fs,path::PathBuf,process::Command};

#[cfg(feature="bundled-shell")]
static SHELL_DLL: &[u8] = include_bytes!(env!("FILETIME_SHELL_DLL"));
#[cfg(feature="bundled-shell")]
static SHELL_MSIX: &[u8] = include_bytes!(env!("FILETIME_SHELL_MSIX"));
#[cfg(feature="bundled-shell")]
static SHELL_CERT: &[u8] = include_bytes!(env!("FILETIME_SHELL_CERT"));

pub fn install() -> Result<()> {
    #[cfg(windows)] {
        install_legacy()?;
        #[cfg(feature="bundled-shell")]
        install_modern()?;
        notify_shell();
        println!("已安装 FileTime 右键菜单。");
        Ok(())
    }
    #[cfg(not(windows))] { anyhow::bail!("仅支持 Windows") }
}
#[cfg(windows)]
fn install_legacy() -> Result<()> {
        let exe = std::env::current_exe()?;
        for (key,label,command) in [
            ("init","创建版本管理","shell-action init"),("commit","提交新版本","gui"),("history","查看版本历史","gui"),
            ("rollback","回滚版本","gui"),("compare","比较版本","gui"),("info","版本管理信息","gui"),
            ("stop","停止版本管理","gui")
        ] {
            let base = format!(r"Software\Classes\*\shell\FileTime\shell\{key}");
            set_value(&base,"MUIVerb",label)?;
            set_value(&format!(r"{base}\command"),"",&format!("\"{}\" {} \"%1\"",exe.display(),command))?;
        }
        set_value(r"Software\Classes\*\shell\FileTime","MUIVerb","FileTime")?;
        set_value(r"Software\Classes\*\shell\FileTime","SubCommands","")?;
        Ok(())
}
pub fn uninstall() -> Result<()> {
    #[cfg(windows)] {
        #[cfg(feature="bundled-shell")]
        run_ps("Get-AppxPackage -Name 'FileTime.Local' | Remove-AppxPackage")?;
        use windows_sys::Win32::System::Registry::{RegDeleteTreeW,HKEY_CURRENT_USER};
        let key = wide(r"Software\Classes\*\shell\FileTime");
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER,key.as_ptr()) };
        ensure!(status == 0 || status == 2, "移除菜单失败，错误码 {status}");
        notify_shell();
        println!("右键菜单已移除；历史数据保留。");
        Ok(())
    }
    #[cfg(not(windows))] { anyhow::bail!("仅支持 Windows") }
}
#[cfg(windows)]
fn notify_shell() {
    use windows_sys::Win32::UI::Shell::{SHChangeNotify,SHCNE_ASSOCCHANGED,SHCNF_IDLIST};
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32,SHCNF_IDLIST,std::ptr::null(),std::ptr::null()); }
}
#[cfg(all(windows,feature="bundled-shell"))]
fn install_modern() -> Result<()> {
    use sha2::{Digest,Sha256};
    let current_exe=std::env::current_exe()?;
    let exe_bytes=fs::read(&current_exe)?;
    let mut hasher=Sha256::new(); hasher.update(SHELL_DLL); hasher.update(&exe_bytes);
    let digest=hex::encode(hasher.finalize());
    let root=PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or_else(||anyhow::anyhow!("找不到 LOCALAPPDATA"))?)
        .join("FileTime").join("integration").join(&digest[..16]);
    fs::create_dir_all(&root)?;
    if !root.join("FileTimeShell.dll").exists() { fs::write(root.join("FileTimeShell.dll"),SHELL_DLL)?; }
    if !root.join("FileTime.msix").exists() { fs::write(root.join("FileTime.msix"),SHELL_MSIX)?; }
    if !root.join("FileTime.cer").exists() { fs::write(root.join("FileTime.cer"),SHELL_CERT)?; }
    let exe=root.join("FileTime.exe");
    if !exe.exists() { fs::write(&exe,&exe_bytes)?; }
    let cert=ps_quote(&root.join("FileTime.cer"));
    let msix=ps_quote(&root.join("FileTime.msix"));
    let location=ps_quote(&root);
    let trusted=certificate_is_trusted();
    if !trusted {
        let helper=root.join("trust-certificate.ps1");
        let script=format!("$ErrorActionPreference='Stop'\nImport-Certificate -FilePath {cert} -CertStoreLocation Cert:\\LocalMachine\\TrustedPeople | Out-Null\n");
        let mut bytes=vec![0xef,0xbb,0xbf]; bytes.extend_from_slice(script.as_bytes());
        fs::write(&helper,bytes)?;
        let args=format!("-NoLogo -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\"",helper.display());
        let args=format!("'{}'",args.replace("'","''"));
        run_ps(&format!("$p=Start-Process -FilePath powershell.exe -Verb RunAs -WindowStyle Hidden -ArgumentList {args} -Wait -PassThru; if($p.ExitCode -ne 0) {{ throw '证书信任安装失败或已取消' }}"))?;
    }
    run_ps("Get-AppxPackage -Name 'FileTime.Local' | Remove-AppxPackage")?;
    // The surrogate may keep the previous DLL loaded after a package update. Only stop
    // the dllhost instance whose command line contains FileTime's CLSID.
    run_ps("Get-CimInstance Win32_Process -Filter \"Name='dllhost.exe'\" | Where-Object { $_.CommandLine -match '134E91FA-54B5-48B4-98EC-8C9A6E6B9148' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }")?;
    run_ps(&format!("Add-AppxPackage -Path {msix} -ExternalLocation {location}"))?;
    Ok(())
}
#[cfg(all(windows,feature="bundled-shell"))]
fn certificate_is_trusted() -> bool {
    use sha1::{Digest,Sha1};
    use windows_sys::Win32::System::Registry::{RegOpenKeyExW,RegCloseKey,HKEY_LOCAL_MACHINE,KEY_READ};
    let thumb=hex::encode_upper(Sha1::digest(SHELL_CERT));
    let path=format!(r"SOFTWARE\Microsoft\SystemCertificates\TrustedPeople\Certificates\{thumb}");
    let mut key=std::ptr::null_mut();
    let status=unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE,wide(&path).as_ptr(),0,KEY_READ,&mut key) };
    if status==0 { unsafe { RegCloseKey(key); } true } else { false }
}
#[cfg(all(windows,feature="bundled-shell"))]
fn ps_quote(path:&std::path::Path) -> String { format!("'{}'",path.to_string_lossy().replace("'","''")) }
#[cfg(all(windows,feature="bundled-shell"))]
fn run_ps(script:&str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let script=format!("$ErrorActionPreference='Stop'; {script}");
    let output=Command::new("powershell.exe").args(["-NoLogo","-NoProfile","-NonInteractive","-WindowStyle","Hidden","-Command",&script]).creation_flags(0x0800_0000).output()?;
    ensure!(output.status.success(),"PowerShell 失败：{}",String::from_utf8_lossy(&output.stderr));
    Ok(())
}
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> { use std::os::windows::ffi::OsStrExt; std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect() }
#[cfg(windows)]
fn set_value(path: &str, name: &str, value: &str) -> Result<()> {
    use windows_sys::Win32::System::Registry::{RegCreateKeyExW,RegSetValueExW,RegCloseKey,HKEY_CURRENT_USER,KEY_SET_VALUE,REG_SZ};
    let mut key = std::ptr::null_mut();
    let status = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER,wide(path).as_ptr(),0,std::ptr::null(),0,KEY_SET_VALUE,std::ptr::null(),&mut key,std::ptr::null_mut()) };
    ensure!(status == 0,"创建注册表项失败：{status}");
    let data = wide(value);
    let status = unsafe { RegSetValueExW(key,wide(name).as_ptr(),0,REG_SZ,data.as_ptr() as *const u8,(data.len()*2) as u32) };
    unsafe { RegCloseKey(key); }
    ensure!(status == 0,"写入注册表失败：{status}");
    Ok(())
}
