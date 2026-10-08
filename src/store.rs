use anyhow::{Context, Result, anyhow, ensure};
use chrono::{DateTime, Local};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::{self, File}, io::{Read, Write}, path::{Path, PathBuf}};
use tempfile::NamedTempFile;
use uuid::Uuid;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Serialize)]
pub struct Version { pub number: i64, pub hash: String, pub size: i64, pub description: String, pub created_at: String, pub operation: String }
#[derive(Clone, Serialize)]
pub struct ManagedFile { pub id: String, pub path: String, pub name: String, pub status: String, pub latest: i64, pub count: i64, pub bytes: i64 }

#[derive(Serialize, Deserialize)]
struct PortableArchive {
    format: u32,
    file_name: String,
    current_hash: String,
    current_size: i64,
    versions: Vec<PortableVersion>,
}

#[derive(Serialize, Deserialize)]
struct PortableVersion {
    number: i64,
    hash: String,
    size: i64,
    description: String,
    created_at: i64,
    source_version: Option<i64>,
    operation: String,
}

pub struct Store { db: Connection, root: PathBuf }

fn valid_hash(hash: &str) -> bool { hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) }

impl Store {
    pub fn open() -> Result<Self> {
        let root = std::env::var_os("FILETIME_HOME").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into())).join("FileTime")
        });
        fs::create_dir_all(root.join("repository/objects"))?;
        fs::create_dir_all(root.join("temp"))?;
        let db = Connection::open(root.join("filetime.db"))?;
        db.busy_timeout(std::time::Duration::from_secs(15))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS files (id TEXT PRIMARY KEY, original_path TEXT NOT NULL, current_path TEXT NOT NULL UNIQUE, file_name TEXT NOT NULL, created_at INTEGER NOT NULL, latest_version INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL, file_identity TEXT);
            CREATE TABLE IF NOT EXISTS objects (hash TEXT PRIMARY KEY, storage_path TEXT NOT NULL, original_size INTEGER NOT NULL, stored_size INTEGER NOT NULL, compression TEXT NOT NULL DEFAULT 'none', created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS versions (id TEXT PRIMARY KEY, file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE, version_number INTEGER NOT NULL, object_hash TEXT NOT NULL REFERENCES objects(hash), file_size INTEGER NOT NULL, description TEXT NOT NULL, created_at INTEGER NOT NULL, source_version INTEGER, operation_type TEXT NOT NULL, UNIQUE(file_id, version_number));
            CREATE INDEX IF NOT EXISTS idx_versions_file ON versions(file_id,version_number);")?;
        Ok(Self { db, root })
    }

    fn path(path: &Path) -> Result<PathBuf> {
        if path.exists() { Ok(fs::canonicalize(path)?) }
        else if path.is_absolute() { Ok(path.to_path_buf()) }
        else { Ok(std::env::current_dir()?.join(path)) }
    }
    fn key(path: &Path) -> Result<String> { Ok(Self::path(path)?.to_string_lossy().to_string()) }
    fn file(&self, path: &Path) -> Result<(String, String, i64)> {
        let key = Self::key(path)?;
        self.db.query_row("SELECT id,status,latest_version FROM files WHERE current_path=?1 COLLATE NOCASE", [key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or_else(|| anyhow!("文件尚未启用版本管理"))
    }
    fn check_regular(path: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(path).context("无法读取文件")?;
        ensure!(meta.is_file() && !meta.file_type().is_symlink(), "只支持普通文件，不允许符号链接");
        Ok(())
    }
    fn now() -> i64 { chrono::Utc::now().timestamp() }
    fn hash_file(path: &Path) -> Result<(String, i64)> {
        let mut input = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut size = 0i64;
        let mut buf = [0u8; 64 * 1024];
        loop { let n = input.read(&mut buf)?; if n == 0 { break; } hasher.update(&buf[..n]); size += n as i64; }
        Ok((hex::encode(hasher.finalize()), size))
    }
    fn object_path(&self, hash: &str) -> PathBuf { self.root.join("repository/objects").join(&hash[..2]).join(hash) }
    fn snapshot(&self, path: &Path) -> Result<(String, i64)> {
        Self::check_regular(path)?;
        let mut temp = NamedTempFile::new_in(self.root.join("temp"))?;
        let mut input = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut size = 0i64;
        let mut buf = [0u8; 64 * 1024];
        loop { let n = input.read(&mut buf)?; if n == 0 { break; } temp.write_all(&buf[..n])?; hasher.update(&buf[..n]); size += n as i64; }
        temp.as_file().sync_all()?;
        let hash = hex::encode(hasher.finalize());
        let dest = self.object_path(&hash);
        if !dest.exists() { fs::create_dir_all(dest.parent().unwrap())?; match temp.persist_noclobber(&dest) { Ok(_) => (), Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => (), Err(e) => return Err(e.error.into()) } }
        ensure!(Self::hash_file(&dest)? == (hash.clone(), size), "快照校验失败");
        self.db.execute("INSERT OR IGNORE INTO objects(hash,storage_path,original_size,stored_size,compression,created_at) VALUES(?1,?2,?3,?3,'none',?4)", params![hash, dest.to_string_lossy(), size, Self::now()])?;
        Ok((hash, size))
    }
    fn add_version(&mut self, id: &str, path: &Path, description: &str, op: &str, source: Option<i64>) -> Result<i64> {
        let (hash,size) = self.snapshot(path)?;
        let tx = self.db.transaction()?;
        let latest: i64 = tx.query_row("SELECT latest_version FROM files WHERE id=?1", [id], |r| r.get(0))?;
        let number = latest + 1;
        tx.execute("INSERT INTO versions(id,file_id,version_number,object_hash,file_size,description,created_at,source_version,operation_type) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![Uuid::new_v4().to_string(),id,number,hash,size,description,Self::now(),source,op])?;
        tx.execute("UPDATE files SET latest_version=?1 WHERE id=?2", params![number,id])?;
        tx.commit()?;
        Ok(number)
    }
    pub fn init(&mut self, path: &Path) -> Result<String> {
        Self::check_regular(path)?;
        let key = Self::key(path)?;
        let exists: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE current_path=?1 COLLATE NOCASE)",[&key],|r|r.get(0))?;
        ensure!(!exists, "文件已受管理");
        let id = Uuid::new_v4().to_string();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        self.db.execute("INSERT INTO files(id,original_path,current_path,file_name,created_at,status) VALUES(?1,?2,?2,?3,?4,'active')", params![id,key,name,Self::now()])?;
        if let Err(error) = self.add_version(&id, path, "初始版本", "initial", None) {
            self.db.execute("DELETE FROM files WHERE id=?1", [&id])?;
            return Err(error);
        }
        Ok("已创建初始版本 v1".into())
    }
    pub fn commit(&mut self, path: &Path, description: &str, force: bool) -> Result<String> {
        Self::check_regular(path)?;
        let (id,status,latest) = self.file(path)?;
        ensure!(status == "active", "文件已停止管理");
        let (hash,_) = Self::hash_file(path)?;
        let previous: String = self.db.query_row("SELECT object_hash FROM versions WHERE file_id=?1 AND version_number=?2", params![id,latest], |r| r.get(0))?;
        ensure!(force || hash != previous, "文件内容未修改");
        let number = self.add_version(&id,path,description,"commit",None)?;
        Ok(format!("已提交 v{number}"))
    }
    pub fn history(&self, path: &Path) -> Result<Vec<Version>> {
        let (id,_,_) = self.file(path)?;
        let mut stmt = self.db.prepare("SELECT version_number,object_hash,file_size,description,created_at,operation_type FROM versions WHERE file_id=?1 ORDER BY version_number DESC")?;
        let values = stmt.query_map([id], |r| {
            let at: i64 = r.get(4)?;
            Ok(Version { number:r.get(0)?,hash:r.get(1)?,size:r.get(2)?,description:r.get(3)?,created_at:DateTime::from_timestamp(at,0).unwrap().with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string(),operation:r.get(5)? })
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(values)
    }
    fn version(&self, id: &str, number: i64) -> Result<(String,i64)> {
        self.db.query_row("SELECT object_hash,file_size FROM versions WHERE file_id=?1 AND version_number=?2", params![id,number], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or_else(|| anyhow!("版本 v{number} 不存在"))
    }
    fn verified_object(&self, hash: &str, size: i64) -> Result<PathBuf> {
        let path = self.object_path(hash);
        ensure!(Self::hash_file(&path)? == (hash.to_string(),size), "历史快照损坏，操作已取消");
        Ok(path)
    }
    pub fn export(&self, path: &Path, number: i64, destination: &Path) -> Result<()> {
        let (id,_,_) = self.file(path)?;
        let (hash,size) = self.version(&id,number)?;
        let source = self.verified_object(&hash,size)?;
        ensure!(!destination.exists(), "导出目标已存在");
        fs::copy(source,destination)?;
        Ok(())
    }
    /// Exports the current file and every saved version into a verified portable archive.
    pub fn export_archive(&self, path: &Path, destination: &Path) -> Result<()> {
        let (id,_,_) = self.file(path)?;
        ensure!(!destination.exists(), "归档目标已存在");
        let mut stmt = self.db.prepare("SELECT version_number,object_hash,file_size,description,created_at,source_version,operation_type FROM versions WHERE file_id=?1 ORDER BY version_number")?;
        let versions = stmt.query_map([id], |r| Ok(PortableVersion { number:r.get(0)?,hash:r.get(1)?,size:r.get(2)?,description:r.get(3)?,created_at:r.get(4)?,source_version:r.get(5)?,operation:r.get(6)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(!versions.is_empty(), "没有可导出的历史版本");
        let current_source = if path.is_file() { path.to_path_buf() } else { self.verified_object(&versions.last().unwrap().hash,versions.last().unwrap().size)? };
        let (current_hash,current_size) = Self::hash_file(&current_source)?;
        let manifest = PortableArchive { format:1,file_name:path.file_name().ok_or_else(||anyhow!("无效文件名"))?.to_string_lossy().to_string(),current_hash,current_size,versions };
        let parent = destination.parent().ok_or_else(||anyhow!("无效目标路径"))?;
        fs::create_dir_all(parent)?;
        let temp = NamedTempFile::new_in(parent)?;
        let mut zip = ZipWriter::new(temp);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("manifest.json",options)?;
        zip.write_all(&serde_json::to_vec(&manifest)?)?;
        zip.start_file("current",options)?;
        std::io::copy(&mut File::open(current_source)?,&mut zip)?;
        let mut seen = std::collections::HashSet::new();
        for version in &manifest.versions {
            if seen.insert(version.hash.clone()) {
                let source = self.verified_object(&version.hash,version.size)?;
                zip.start_file(format!("objects/{}",version.hash),options)?;
                std::io::copy(&mut File::open(source)?,&mut zip)?;
            }
        }
        let temp = zip.finish()?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(destination)?;
        Ok(())
    }
    pub fn archive_name(archive_path:&Path) -> Result<String> {
        let mut zip=ZipArchive::new(File::open(archive_path)?)?;
        let mut entry=zip.by_name("manifest.json")?;
        ensure!(entry.size() <= 8 * 1024 * 1024, "归档元数据过大");
        let manifest:PortableArchive=serde_json::from_reader(&mut entry)?;
        ensure!(manifest.format==1,"不支持的归档格式");
        let name=Path::new(&manifest.file_name);
        ensure!(name.file_name().is_some() && name.components().count()==1 && !manifest.file_name.contains(['/', '\\', ':']),"归档文件名无效");
        Ok(manifest.file_name)
    }
    pub fn suggested_import_path(archive_path:&Path) -> Result<PathBuf> {
        let name=Self::archive_name(archive_path)?;
        let parent=archive_path.parent().ok_or_else(||anyhow!("无效归档路径"))?;
        let initial=parent.join(&name);
        if !initial.exists() { return Ok(initial); }
        let file=Path::new(&name);
        let stem=file.file_stem().unwrap().to_string_lossy();
        let extension=file.extension().map(|e|format!(".{}",e.to_string_lossy())).unwrap_or_default();
        for n in 1..1000 {
            let candidate=parent.join(format!("{stem} (导入 {n}){extension}"));
            if !candidate.exists() { return Ok(candidate); }
        }
        Err(anyhow!("找不到可用的导入文件名"))
    }
    /// Imports a portable archive to a new destination and creates a new managed file identity.
    pub fn import_archive(&mut self, archive_path: &Path, destination: &Path) -> Result<String> {
        ensure!(!destination.exists(), "导入目标已存在");
        let mut zip = ZipArchive::new(File::open(archive_path)?)?;
        let manifest: PortableArchive = {
            let mut entry = zip.by_name("manifest.json")?;
            ensure!(entry.size() <= 8 * 1024 * 1024, "归档元数据过大");
            serde_json::from_reader(&mut entry)?
        };
        ensure!(manifest.format == 1 && !manifest.versions.is_empty(), "不支持的归档格式");
        ensure!(manifest.versions.len() <= 100_000, "版本数量超出限制");
        let mut numbers = std::collections::HashSet::new();
        for v in &manifest.versions {
            ensure!(v.number > 0 && numbers.insert(v.number) && v.size >= 0 && valid_hash(&v.hash), "归档版本信息无效");
        }
        ensure!(valid_hash(&manifest.current_hash) && manifest.current_size >= 0, "归档当前文件信息无效");
        let parent = destination.parent().ok_or_else(||anyhow!("无效导入目标"))?;
        fs::create_dir_all(parent)?;
        let mut current = NamedTempFile::new_in(parent)?;
        {
            let mut entry = zip.by_name("current")?;
            ensure!(entry.size() == manifest.current_size as u64, "归档当前文件大小不符");
            std::io::copy(&mut entry,&mut current)?;
        }
        current.as_file().sync_all()?;
        ensure!(Self::hash_file(current.path())? == (manifest.current_hash,manifest.current_size), "归档当前文件校验失败");
        let mut seen = std::collections::HashSet::new();
        for v in &manifest.versions {
            if seen.insert(v.hash.clone()) {
                let mut entry = zip.by_name(&format!("objects/{}",v.hash))?;
                ensure!(entry.size() == v.size as u64, "归档快照大小不符");
                let mut temp = NamedTempFile::new_in(self.root.join("temp"))?;
                std::io::copy(&mut entry,&mut temp)?;
                temp.as_file().sync_all()?;
                ensure!(Self::hash_file(temp.path())? == (v.hash.clone(),v.size), "归档快照校验失败");
                let dest = self.object_path(&v.hash);
                if !dest.exists() { fs::create_dir_all(dest.parent().unwrap())?; match temp.persist_noclobber(&dest) { Ok(_) => (), Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => (), Err(e) => return Err(e.error.into()) } }
                ensure!(Self::hash_file(&dest)? == (v.hash.clone(),v.size), "本地快照校验失败");
            }
        }
        let key = fs::canonicalize(parent)?.join(destination.file_name().ok_or_else(||anyhow!("无效导入文件名"))?).to_string_lossy().to_string();
        let id = Uuid::new_v4().to_string();
        let latest = manifest.versions.iter().map(|v|v.number).max().unwrap();
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO files(id,original_path,current_path,file_name,created_at,latest_version,status) VALUES(?1,?2,?2,?3,?4,?5,'active')",params![id,key,destination.file_name().unwrap().to_string_lossy(),Self::now(),latest])?;
        for v in &manifest.versions {
            let storage = self.root.join("repository/objects").join(&v.hash[..2]).join(&v.hash);
            tx.execute("INSERT OR IGNORE INTO objects(hash,storage_path,original_size,stored_size,compression,created_at) VALUES(?1,?2,?3,?3,'none',?4)",params![v.hash,storage.to_string_lossy(),v.size,Self::now()])?;
            tx.execute("INSERT INTO versions(id,file_id,version_number,object_hash,file_size,description,created_at,source_version,operation_type) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![Uuid::new_v4().to_string(),id,v.number,v.hash,v.size,v.description,v.created_at,v.source_version,v.operation])?;
        }
        current.persist_noclobber(destination)?;
        if let Err(e) = tx.commit() { let _ = fs::remove_file(destination); return Err(e.into()); }
        Ok(format!("已导入 {} 个版本",manifest.versions.len()))
    }
    pub fn rollback(&mut self, path: &Path, number: i64) -> Result<String> {
        Self::check_regular(path)?;
        let (id,status,latest) = self.file(path)?;
        ensure!(status == "active", "文件已停止管理");
        let (hash,size) = self.version(&id,number)?;
        let source = self.verified_object(&hash,size)?;
        let (current_hash,_) = Self::hash_file(path)?;
        let latest_hash = self.version(&id,latest)?.0;
        if current_hash != latest_hash { self.add_version(&id,path,"回滚前自动备份未提交内容","backup",None)?; }
        let parent = path.parent().ok_or_else(|| anyhow!("无效文件路径"))?;
        let mut temp = NamedTempFile::new_in(parent)?;
        std::io::copy(&mut File::open(source)?, &mut temp)?;
        temp.as_file().sync_all()?;
        ensure!(Self::hash_file(temp.path())? == (hash,size), "恢复文件校验失败");
        let temp_path = temp.into_temp_path().keep()?;
        #[cfg(windows)] {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_IGNORE_MERGE_ERRORS};
            let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
            let backup = parent.join(format!(".filetime-{}.bak",Uuid::new_v4()));
            let ok = unsafe { ReplaceFileW(wide(path).as_ptr(),wide(&temp_path).as_ptr(),wide(&backup).as_ptr(),REPLACEFILE_IGNORE_MERGE_ERRORS, std::ptr::null(),std::ptr::null()) };
            if ok == 0 { let _ = fs::remove_file(&temp_path); }
            ensure!(ok != 0, "无法替换文件：{}；原文件未被删除", std::io::Error::last_os_error());
            let result = self.add_version(&id,path,&format!("回滚至 v{number}"),"rollback",Some(number));
            if result.is_ok() { let _ = fs::remove_file(&backup); }
            return result.map(|n| format!("已回滚至 v{number}，记录为 v{n}")).map_err(|e| anyhow!("回滚内容已写入，但版本记录失败：{e:#}。原文件备份保留在 {}",backup.display()));
        }
        #[cfg(not(windows))] {
            fs::rename(temp_path,path)?;
            let n = self.add_version(&id,path,&format!("回滚至 v{number}"),"rollback",Some(number))?;
            Ok(format!("已回滚至 v{number}，记录为 v{n}"))
        }
    }
    pub fn compare(&self, path: &Path, left: i64, right: i64, ignore_whitespace: bool) -> Result<String> {
        let (id,_,_) = self.file(path)?;
        let (a,asize) = self.version(&id,left)?; let (b,bsize) = self.version(&id,right)?;
        let ap = self.verified_object(&a,asize)?; let bp = self.verified_object(&b,bsize)?;
        let extension = path.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        if ["txt","md","json","xml","js","ts","py","rs","java","csv","toml","yaml","yml","html","css"].contains(&extension.as_str()) && asize < 10_000_000 && bsize < 10_000_000 {
            let atext = String::from_utf8(fs::read(ap)?)?; let btext = String::from_utf8(fs::read(bp)?)?;
            let (atext,btext) = if ignore_whitespace { (atext.lines().map(str::trim).collect::<Vec<_>>().join("\n"),btext.lines().map(str::trim).collect::<Vec<_>>().join("\n")) } else { (atext,btext) };
            Ok(similar::TextDiff::from_lines(&atext,&btext).unified_diff().header(&format!("v{left}"),&format!("v{right}")).to_string())
        } else { Ok(format!("v{left}: {asize} 字节 SHA-256 {a}\nv{right}: {bsize} 字节 SHA-256 {b}")) }
    }
    pub fn stop(&mut self, path: &Path, delete: bool) -> Result<()> {
        let (id,_,_) = self.file(path)?;
        if delete { self.db.execute("DELETE FROM files WHERE id=?1", [&id])?; self.gc()?; }
        else { self.db.execute("UPDATE files SET status='stopped' WHERE id=?1", [&id])?; }
        Ok(())
    }
    pub fn resume(&mut self, path: &Path) -> Result<()> { let (id,_,_) = self.file(path)?; self.db.execute("UPDATE files SET status='active' WHERE id=?1",[id])?; Ok(()) }
    pub fn delete_version(&mut self, path: &Path, number: i64) -> Result<()> {
        let (id,_,latest) = self.file(path)?;
        ensure!(number != latest, "不能删除最新版本");
        ensure!(self.db.execute("DELETE FROM versions WHERE file_id=?1 AND version_number=?2",params![id,number])? == 1, "版本不存在");
        self.gc()
    }
    fn gc(&mut self) -> Result<()> {
        let mut stmt = self.db.prepare("SELECT hash FROM objects WHERE NOT EXISTS(SELECT 1 FROM versions WHERE object_hash=objects.hash)")?;
        let hashes = stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for hash in hashes { let _ = fs::remove_file(self.object_path(&hash)); self.db.execute("DELETE FROM objects WHERE hash=?1",[hash])?; }
        Ok(())
    }
    pub fn relocate(&mut self, old: &Path, new: &Path) -> Result<()> {
        Self::check_regular(new)?;
        let (id,_,_) = self.file(old)?;
        let key = Self::key(new)?;
        let name = new.file_name().unwrap().to_string_lossy().to_string();
        self.db.execute("UPDATE files SET current_path=?1,file_name=?2 WHERE id=?3",params![key,name,id])?;
        Ok(())
    }
    pub fn info(&self, path: &Path) -> Result<ManagedFile> {
        let key = Self::key(path)?;
        self.db.query_row("SELECT f.id,f.current_path,f.file_name,f.status,f.latest_version,COUNT(v.id),COALESCE(SUM(v.file_size),0) FROM files f LEFT JOIN versions v ON v.file_id=f.id WHERE f.current_path=?1 COLLATE NOCASE GROUP BY f.id",[key],|r|Ok(ManagedFile{id:r.get(0)?,path:r.get(1)?,name:r.get(2)?,status:r.get(3)?,latest:r.get(4)?,count:r.get(5)?,bytes:r.get(6)?})).optional()?.ok_or_else(||anyhow!("文件尚未启用版本管理"))
    }
    pub fn files(&self) -> Result<Vec<ManagedFile>> {
        let mut stmt = self.db.prepare("SELECT f.id,f.current_path,f.file_name,f.status,f.latest_version,COUNT(v.id),COALESCE(SUM(v.file_size),0) FROM files f LEFT JOIN versions v ON v.file_id=f.id GROUP BY f.id ORDER BY f.created_at DESC")?;
        Ok(stmt.query_map([],|r|Ok(ManagedFile{id:r.get(0)?,path:r.get(1)?,name:r.get(2)?,status:r.get(3)?,latest:r.get(4)?,count:r.get(5)?,bytes:r.get(6)?}))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn verify(&self, path: Option<&Path>) -> Result<String> {
        let result: String = self.db.query_row("PRAGMA integrity_check",[],|r|r.get(0))?;
        ensure!(result == "ok", "数据库完整性检查失败：{result}");
        let files = if let Some(path) = path { vec![self.info(path)?] } else { self.files()? };
        let mut count = 0;
        for file in files { for v in self.history(Path::new(&file.path))? { self.verified_object(&v.hash,v.size)?; count += 1; } }
        Ok(format!("数据库正常，已验证 {count} 个版本"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_file_lifecycle() -> Result<()> {
        let home = tempfile::tempdir()?;
        // The public constructor creates the schema and directories; redirect it to a private test home.
        let previous = std::env::var_os("FILETIME_HOME");
        unsafe { std::env::set_var("FILETIME_HOME",home.path()); }
        let mut store = Store::open()?;
        if let Some(previous) = previous { unsafe { std::env::set_var("FILETIME_HOME",previous); } } else { unsafe { std::env::remove_var("FILETIME_HOME"); } }
        let file = home.path().join("测试 file.txt");
        fs::write(&file,"first\n")?;
        store.init(&file)?;
        assert_eq!(store.history(&file)?.len(),1);
        assert!(store.commit(&file,"same",false).is_err());
        fs::write(&file,"second\n")?;
        store.commit(&file,"second",false)?;
        fs::write(&file,"unsaved\n")?;
        store.rollback(&file,1)?;
        assert_eq!(fs::read_to_string(&file)?,"first\n");
        assert_eq!(store.history(&file)?.len(),4);
        let exported = home.path().join("export.txt");
        store.export(&file,2,&exported)?;
        assert_eq!(fs::read_to_string(exported)?,"second\n");
        store.verify(Some(&file))?;
        let archive = home.path().join("portable.filetime");
        store.export_archive(&file,&archive)?;
        let imported = home.path().join("导入 文件.txt");
        store.import_archive(&archive,&imported)?;
        assert_eq!(fs::read_to_string(&imported)?,"first\n");
        assert_eq!(store.history(&imported)?.len(),4);
        store.verify(Some(&imported))?;
        store.stop(&file,false)?;
        assert!(store.commit(&file,"blocked",false).is_err());
        store.resume(&file)?;
        store.delete_version(&file,2)?;
        assert_eq!(store.history(&file)?.len(),3);
        store.stop(&file,true)?;
        assert!(file.exists());
        Ok(())
    }
}
