use crate::{
    shell,
    store::{ManagedFile, Store, Version},
};
use anyhow::{Result, anyhow};
use slint::{CloseRequestResponse, ModelRc, VecModel};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, SystemTime},
};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
    System::Threading::CreateMutexW,
};

slint::include_modules!();

struct State {
    store: Store,
    files: Vec<ManagedFile>,
    versions: Vec<Version>,
}

#[cfg(windows)]
struct InstanceGuard(HANDLE);

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn request_dir() -> PathBuf {
    dirs_base().join("FileTime").join("gui-requests")
}

fn dirs_base() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(windows)]
fn acquire_instance(initial: &Option<PathBuf>) -> Result<Option<InstanceGuard>> {
    use std::os::windows::ffi::OsStrExt;
    let name = std::ffi::OsStr::new("Local\\FileTime.Gui.Singleton.v1")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let dir = request_dir();
        std::fs::create_dir_all(&dir)?;
        let id = uuid::Uuid::new_v4().to_string();
        let partial = dir.join(format!("{id}.part"));
        let ready = dir.join(format!("{id}.request"));
        std::fs::write(&partial, serde_json::to_vec(initial)?)?;
        std::fs::rename(partial, ready)?;
        unsafe {
            CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(InstanceGuard(handle)))
}

fn receive_requests(app: &App, state: &Rc<RefCell<State>>, started: SystemTime) {
    let Ok(entries) = std::fs::read_dir(request_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("request") {
            continue;
        }
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t >= started)
            .unwrap_or(false);
        if fresh {
            if let Ok(data) = std::fs::read(&path) {
                if let Ok(Some(value)) = serde_json::from_slice::<Option<PathBuf>>(&data) {
                    app.set_path(value.to_string_lossy().to_string().into());
                    show_history(app, state);
                }
                let _ = app.window().show();
            }
        }
        let _ = std::fs::remove_file(path);
    }
}

pub fn run(initial: Option<PathBuf>) -> Result<()> {
    #[cfg(windows)]
    let Some(_instance) = acquire_instance(&initial)? else {
        return Ok(());
    };
    let started = SystemTime::now();
    let app = App::new()?;
    if let Some(path) = initial {
        app.set_path(path.to_string_lossy().to_string().into());
    }
    let state = Rc::new(RefCell::new(State {
        store: Store::open()?,
        files: Vec::new(),
        versions: Vec::new(),
    }));
    refresh(&app, &state);
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_refresh(move || {
            if let Some(a) = a.upgrade() {
                refresh(&a, &s);
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_choose_file(move |index| {
            if let Some(a) = a.upgrade() {
                let path = s.borrow().files.get(index as usize).map(|f| f.path.clone());
                if let Some(path) = path {
                    a.set_path(path.into());
                    show_history(&a, &s);
                }
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_choose_version(move |index| {
            if let Some(a) = a.upgrade() {
                if let Some(v) = s.borrow().versions.get(index as usize) {
                    a.set_selected_version(index);
                    a.set_detail_text(
                        format!(
                            "v{} · {}\n{} 字节\nSHA-256: {}\n{}",
                            v.number, v.created_at, v.size, v.hash, v.description
                        )
                        .into(),
                    );
                }
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_init_file(move || {
            if let Some(a) = a.upgrade() {
                action(&a, &s, |x, p| x.init(p));
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_commit_file(move || {
            if let Some(a) = a.upgrade() {
                let desc = a.get_description().to_string();
                action(&a, &s, |x, p| {
                    x.commit(p, if desc.is_empty() { "新版本" } else { &desc }, false)
                });
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_rollback_file(move || {
            if let Some(a) = a.upgrade() {
                if !a.get_confirm_replace() {
                    a.set_status_text("请先勾选回滚确认".into());
                    return;
                }
                let n = selected(&a, &s);
                action(&a, &s, |x, p| x.rollback(p, n?));
                a.set_confirm_replace(false);
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_export_file(move || {
            if let Some(a) = a.upgrade() {
                let n = selected(&a, &s);
                action(&a, &s, |x, p| {
                    let number = n?;
                    let dest = p.with_file_name(format!(
                        "{}.v{}.export",
                        p.file_name().unwrap().to_string_lossy(),
                        number
                    ));
                    x.export(p, number, &dest)?;
                    Ok(format!("已导出至 {}", dest.display()))
                });
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_export_archive(move || {
            if let Some(a) = a.upgrade() {
                action(&a, &s, |x, p| {
                    let dest = p.with_extension("filetime");
                    x.export_archive(p, &dest)?;
                    Ok(format!("已导出至 {}", dest.display()))
                });
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_import_archive(move || {
            if let Some(a) = a.upgrade() {
                let archive = PathBuf::from(a.get_path().to_string());
                let result = (|| {
                    let dest = Store::suggested_import_path(&archive)?;
                    let message = s.borrow_mut().store.import_archive(&archive, &dest)?;
                    a.set_path(dest.to_string_lossy().to_string().into());
                    Ok::<_, anyhow::Error>(message)
                })();
                a.set_status_text(result.unwrap_or_else(|e| format!("错误：{e:#}")).into());
                refresh(&a, &s);
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_compare_file(move || {
            if let Some(a) = a.upgrade() {
                let n = selected(&a, &s);
                let path = PathBuf::from(a.get_path().to_string());
                let result = (|| {
                    let n = n?;
                    let state = s.borrow();
                    let latest = state.store.info(&path)?.latest;
                    state.store.compare(&path, n, latest, false)
                })();
                a.set_detail_text(result.unwrap_or_else(|e| format!("错误：{e:#}")).into());
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_stop_file(move || {
            if let Some(a) = a.upgrade() {
                if !a.get_confirm_stop() {
                    a.set_status_text("请先勾选停止管理确认".into());
                    return;
                }
                action(&a, &s, |x, p| {
                    x.stop(p, false)?;
                    Ok("已停止管理，历史数据保留".into())
                });
                a.set_confirm_stop(false);
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_resume_file(move || {
            if let Some(a) = a.upgrade() {
                action(&a, &s, |x, p| {
                    x.resume(p)?;
                    Ok("已恢复管理".into())
                });
            }
        });
    }
    {
        let a = app.as_weak();
        let s = state.clone();
        app.on_verify_file(move || {
            if let Some(a) = a.upgrade() {
                action(&a, &s, |x, p| x.verify(Some(p)));
            }
        });
    }
    {
        let a = app.as_weak();
        app.on_install_menu(move || {
            if let Some(a) = a.upgrade() {
                a.set_status_text(
                    shell::install()
                        .map(|_| "右键菜单已安装".to_string())
                        .unwrap_or_else(|e| format!("错误：{e:#}"))
                        .into(),
                );
            }
        });
    }
    {
        let a = app.as_weak();
        app.on_remove_menu(move || {
            if let Some(a) = a.upgrade() {
                a.set_status_text(
                    shell::uninstall()
                        .map(|_| "右键菜单已移除".to_string())
                        .unwrap_or_else(|e| format!("错误：{e:#}"))
                        .into(),
                );
            }
        });
    }
    app.window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);
    let tray = Tray::new()?;
    {
        let a = app.as_weak();
        tray.on_open(move || {
            if let Some(a) = a.upgrade() {
                let _ = a.window().show();
            }
        });
    }
    tray.on_exit(|| {
        let _ = slint::quit_event_loop();
    });
    let request_timer = slint::Timer::default();
    {
        let a = app.as_weak();
        let s = state.clone();
        request_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                if let Some(a) = a.upgrade() {
                    receive_requests(&a, &s, started);
                }
            },
        );
    }
    app.show()?;
    slint::run_event_loop()?;
    drop(tray);
    Ok(())
}

fn selected(app: &App, state: &Rc<RefCell<State>>) -> Result<i64> {
    let index = app.get_selected_version();
    state
        .borrow()
        .versions
        .get(index as usize)
        .map(|v| v.number)
        .ok_or_else(|| anyhow!("请先选择历史版本"))
}
fn refresh(app: &App, state: &Rc<RefCell<State>>) {
    let result = state.borrow().store.files();
    match result {
        Ok(files) => {
            app.set_files(ModelRc::new(VecModel::from(
                files
                    .iter()
                    .map(|f| {
                        format!(
                            "{} · v{} · {}",
                            f.name,
                            f.latest,
                            if f.status == "active" {
                                "管理中"
                            } else {
                                "已停止"
                            }
                        )
                        .into()
                    })
                    .collect::<Vec<slint::SharedString>>(),
            )));
            state.borrow_mut().files = files;
            show_history(app, state);
        }
        Err(e) => app.set_status_text(format!("错误：{e:#}").into()),
    }
}
fn show_history(app: &App, state: &Rc<RefCell<State>>) {
    let path = PathBuf::from(app.get_path().to_string());
    let result = state.borrow().store.history(&path);
    match result {
        Ok(versions) => {
            app.set_versions(ModelRc::new(VecModel::from(
                versions
                    .iter()
                    .map(|v| format!("v{} · {} · {}", v.number, v.created_at, v.description).into())
                    .collect::<Vec<slint::SharedString>>(),
            )));
            state.borrow_mut().versions = versions;
            app.set_selected_version(-1);
        }
        Err(_) => {
            app.set_versions(ModelRc::new(VecModel::from(
                Vec::<slint::SharedString>::new(),
            )));
            state.borrow_mut().versions.clear();
        }
    }
}
fn action<F>(app: &App, state: &Rc<RefCell<State>>, f: F)
where
    F: FnOnce(&mut Store, &Path) -> Result<String>,
{
    let path = PathBuf::from(app.get_path().to_string());
    let result = f(&mut state.borrow_mut().store, &path);
    app.set_status_text(result.unwrap_or_else(|e| format!("错误：{e:#}")).into());
    refresh(app, state);
}
