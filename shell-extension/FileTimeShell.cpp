#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shobjidl.h>
#include <shlwapi.h>
#include <objbase.h>
#include <winsqlite3.h>
#include <atomic>
#include <string>
#include <vector>

#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "shell32.lib")
#pragma comment(lib, "winsqlite3.lib")

// Must match installer/windows/AppxManifest.xml.
static const CLSID CLSID_FileTime = {0x134e91fa,0x54b5,0x48b4,{0x98,0xec,0x8c,0x9a,0x6e,0x6b,0x91,0x48}};
static HMODULE g_module = nullptr;
static std::atomic<long> g_objects{0};
static std::atomic<long> g_locks{0};

static std::wstring module_directory() {
    wchar_t path[MAX_PATH]{};
    GetModuleFileNameW(g_module,path,MAX_PATH);
    std::wstring value(path);
    return value.substr(0,value.find_last_of(L"\\/"));
}
static std::wstring data_directory() {
    wchar_t value[32768]{};
    DWORD n=GetEnvironmentVariableW(L"FILETIME_HOME",value,32768);
    if(n && n<32768) return value;
    n=GetEnvironmentVariableW(L"LOCALAPPDATA",value,32768);
    return n && n<32768 ? std::wstring(value)+L"\\FileTime" : L"";
}
static std::string utf8(const std::wstring& value) {
    int n=WideCharToMultiByte(CP_UTF8,0,value.c_str(),(int)value.size(),nullptr,0,nullptr,nullptr);
    std::string out(n,'\0');
    if(n) WideCharToMultiByte(CP_UTF8,0,value.c_str(),(int)value.size(),out.data(),n,nullptr,nullptr);
    return out;
}
static std::wstring selected_path(IShellItemArray* items) {
    if(!items) return L"";
    DWORD count=0;
    if(FAILED(items->GetCount(&count)) || count!=1) return L"";
    IShellItem* item=nullptr;
    if(FAILED(items->GetItemAt(0,&item))) return L"";
    PWSTR raw=nullptr;
    HRESULT hr=item->GetDisplayName(SIGDN_FILESYSPATH,&raw);
    item->Release();
    if(FAILED(hr) || !raw) return L"";
    std::wstring result(raw);
    CoTaskMemFree(raw);
    return result;
}
static std::wstring canonical_path(const std::wstring& path) {
    if(path.rfind(L"\\\\?\\",0)==0) return path;
    if(path.rfind(L"\\\\",0)==0) return L"\\\\?\\UNC\\"+path.substr(2);
    return L"\\\\?\\"+path;
}
static std::wstring db_file_id(sqlite3* db,const std::wstring& path) {
    sqlite3_stmt* stmt=nullptr;
    const char* sql="SELECT id FROM files WHERE (current_path=?1 COLLATE NOCASE OR current_path=?2 COLLATE NOCASE) AND status='active' LIMIT 1";
    if(sqlite3_prepare_v2(db,sql,-1,&stmt,nullptr)!=SQLITE_OK) return L"";
    auto canonical=canonical_path(path);
    sqlite3_bind_text16(stmt,1,path.c_str(),-1,SQLITE_TRANSIENT);
    sqlite3_bind_text16(stmt,2,canonical.c_str(),-1,SQLITE_TRANSIENT);
    std::wstring id;
    if(sqlite3_step(stmt)==SQLITE_ROW) {
        auto text=(const wchar_t*)sqlite3_column_text16(stmt,0);
        if(text) id=text;
    }
    sqlite3_finalize(stmt);
    return id;
}
struct Recent { int number; std::wstring title; };
static std::vector<Recent> recent_versions(const std::wstring& path,bool* managed=nullptr) {
    std::vector<Recent> result;
    auto root=data_directory();
    if(root.empty()) return result;
    sqlite3* db=nullptr;
    std::string dbpath=utf8(root+L"\\filetime.db");
    if(sqlite3_open_v2(dbpath.c_str(),&db,SQLITE_OPEN_READONLY|SQLITE_OPEN_NOMUTEX,nullptr)!=SQLITE_OK) {
        if(db) sqlite3_close(db);
        return result;
    }
    sqlite3_busy_timeout(db,50);
    auto id=db_file_id(db,path);
    if(managed) *managed=!id.empty();
    if(!id.empty()) {
        sqlite3_stmt* stmt=nullptr;
        if(sqlite3_prepare_v2(db,"SELECT version_number,description FROM versions WHERE file_id=?1 ORDER BY version_number DESC LIMIT 3",-1,&stmt,nullptr)==SQLITE_OK) {
            sqlite3_bind_text16(stmt,1,id.c_str(),-1,SQLITE_TRANSIENT);
            while(sqlite3_step(stmt)==SQLITE_ROW) {
                int number=sqlite3_column_int(stmt,0);
                auto text=(const wchar_t*)sqlite3_column_text16(stmt,1);
                std::wstring description=text ? text : L"";
                if(description.size()>36) description=description.substr(0,36)+L"…";
                result.push_back({number,L"v"+std::to_wstring(number)+L" - "+description});
            }
        }
        if(stmt) sqlite3_finalize(stmt);
    }
    sqlite3_close(db);
    return result;
}
static PWSTR duplicate_title(const std::wstring& title) {
    size_t bytes=(title.size()+1)*sizeof(wchar_t);
    auto out=(PWSTR)CoTaskMemAlloc(bytes);
    if(out) memcpy(out,title.c_str(),bytes);
    return out;
}
static std::wstring quote(const std::wstring& value) { return L"\""+value+L"\""; }
static bool launch(const std::wstring& action,const std::wstring& path,int number) {
    std::wstring exe=module_directory()+L"\\FileTime.exe";
    std::wstring command=quote(exe)+L" shell-action "+action+L" "+quote(path);
    if(number>0) command+=L" "+std::to_wstring(number);
    std::vector<wchar_t> buffer(command.begin(),command.end()); buffer.push_back(0);
    STARTUPINFOW startup{}; startup.cb=sizeof(startup);
    PROCESS_INFORMATION process{};
    if(!CreateProcessW(exe.c_str(),buffer.data(),nullptr,nullptr,FALSE,CREATE_NO_WINDOW,nullptr,nullptr,&startup,&process)) return false;
    CloseHandle(process.hThread); CloseHandle(process.hProcess);
    return true;
}

enum class Action { Root, Init, Commit, History, Rollback, Compare, Info, Stop, Archive, Import, QuickVersion };
class Command;

class Enumerator final : public IEnumExplorerCommand {
    std::atomic<ULONG> refs{1};
    std::vector<IExplorerCommand*> commands;
    size_t offset=0;
public:
    Enumerator(std::vector<IExplorerCommand*> values,size_t position=0):commands(std::move(values)),offset(position) { ++g_objects; for(auto* p:commands) p->AddRef(); }
    ~Enumerator() { for(auto* p:commands) p->Release(); --g_objects; }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override { if(!out)return E_POINTER; *out=nullptr; if(iid==IID_IUnknown||iid==IID_IEnumExplorerCommand){*out=this;AddRef();return S_OK;}return E_NOINTERFACE; }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++refs; }
    ULONG STDMETHODCALLTYPE Release() override { auto n=--refs; if(!n)delete this; return n; }
    HRESULT STDMETHODCALLTYPE Next(ULONG count,IExplorerCommand** out,ULONG* fetched) override {
        if(!out || (count!=1 && !fetched))return E_POINTER;
        ULONG n=0; while(n<count && offset<commands.size()){out[n]=commands[offset++];out[n++]->AddRef();}
        if(fetched)*fetched=n;
        return n==count?S_OK:S_FALSE;
    }
    HRESULT STDMETHODCALLTYPE Skip(ULONG count) override { offset=min(commands.size(),offset+count);return offset<commands.size()?S_OK:S_FALSE; }
    HRESULT STDMETHODCALLTYPE Reset() override {offset=0;return S_OK;}
    HRESULT STDMETHODCALLTYPE Clone(IEnumExplorerCommand** out) override {if(!out)return E_POINTER;*out=new Enumerator(commands,offset);return S_OK;}
};

class Command final : public IExplorerCommand {
    std::atomic<ULONG> refs{1};
    Action action;
    std::wstring path;
    std::wstring title;
    int version=0;
public:
    Command(Action type,std::wstring file=L"",std::wstring name=L"",int number=0):action(type),path(std::move(file)),title(std::move(name)),version(number){++g_objects;}
    ~Command(){--g_objects;}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {if(!out)return E_POINTER;*out=nullptr;if(iid==IID_IUnknown||iid==IID_IExplorerCommand){*out=this;AddRef();return S_OK;}return E_NOINTERFACE;}
    ULONG STDMETHODCALLTYPE AddRef() override {return ++refs;}
    ULONG STDMETHODCALLTYPE Release() override {auto n=--refs;if(!n)delete this;return n;}
    HRESULT STDMETHODCALLTYPE GetTitle(IShellItemArray* items,LPWSTR* out) override {
        if(!out)return E_POINTER;
        if(action==Action::Root && items)path=selected_path(items);
        *out=duplicate_title(title.empty()?L"FileTime":title);
        return *out?S_OK:E_OUTOFMEMORY;
    }
    HRESULT STDMETHODCALLTYPE GetIcon(IShellItemArray*,LPWSTR* out) override {if(out)*out=nullptr;return E_NOTIMPL;}
    HRESULT STDMETHODCALLTYPE GetToolTip(IShellItemArray*,LPWSTR* out) override {if(out)*out=nullptr;return E_NOTIMPL;}
    HRESULT STDMETHODCALLTYPE GetCanonicalName(GUID* out) override {if(!out)return E_POINTER;*out=CLSID_FileTime;return S_OK;}
    HRESULT STDMETHODCALLTYPE GetState(IShellItemArray* items,BOOL,EXPCMDSTATE* out) override {
        if(!out)return E_POINTER;
        if(action==Action::Root && items)path=selected_path(items);
        *out=path.empty()?ECS_HIDDEN:ECS_ENABLED;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE Invoke(IShellItemArray* items,IBindCtx*) override {
        if(action==Action::Root)return E_NOTIMPL;
        auto current=items?selected_path(items):path;
        if(current.empty())current=path;
        if(current.empty())return E_INVALIDARG;
        const wchar_t* name=L"";
        switch(action){
        case Action::Init:name=L"init";break;case Action::Commit:name=L"commit";break;case Action::History:name=L"history";break;
        case Action::Rollback:name=L"rollback";break;case Action::Compare:name=L"compare";break;case Action::Info:name=L"info";break;
        case Action::Stop:name=L"stop";break;case Action::Archive:name=L"export-archive";break;case Action::Import:name=L"import-archive";break;case Action::QuickVersion:name=L"quick-rollback";break;
        default:return E_NOTIMPL;
        }
        return launch(name,current,version)?S_OK:HRESULT_FROM_WIN32(GetLastError());
    }
    HRESULT STDMETHODCALLTYPE GetFlags(EXPCMDFLAGS* out) override {if(!out)return E_POINTER;*out=action==Action::Root?ECF_HASSUBCOMMANDS:ECF_DEFAULT;return S_OK;}
    HRESULT STDMETHODCALLTYPE EnumSubCommands(IEnumExplorerCommand** out) override {
        if(!out)return E_POINTER;*out=nullptr;
        std::vector<IExplorerCommand*> items;
        auto add=[&](Action type,const wchar_t* name){items.push_back(new Command(type,path,name));};
        if(action==Action::Root){
            if(path.empty())return E_FAIL;
            if(path.size()>=9 && _wcsicmp(path.c_str()+path.size()-9,L".filetime")==0){
                add(Action::Import,L"导入便携归档");
                *out=new Enumerator(items);for(auto* p:items)p->Release();return S_OK;
            }
            bool managed=false;auto recent=recent_versions(path,&managed);
            if(managed){
                add(Action::Commit,L"提交新版本");add(Action::History,L"查看版本历史");
                for(const auto& item:recent)items.push_back(new Command(Action::QuickVersion,path,L"快速回滚到 "+item.title,item.number));
                add(Action::Rollback,L"回滚版本");add(Action::Compare,L"比较版本");add(Action::Archive,L"导出便携归档");
                add(Action::Info,L"版本管理信息");add(Action::Stop,L"停止版本管理");
            }else add(Action::Init,L"创建版本管理");
        }else return E_NOTIMPL;
        *out=new Enumerator(items);
        for(auto* p:items)p->Release();
        return S_OK;
    }
};

class Factory final : public IClassFactory {
    std::atomic<ULONG> refs{1};
public:
    Factory(){++g_objects;} ~Factory(){--g_objects;}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid,void** out) override {if(!out)return E_POINTER;*out=nullptr;if(iid==IID_IUnknown||iid==IID_IClassFactory){*out=this;AddRef();return S_OK;}return E_NOINTERFACE;}
    ULONG STDMETHODCALLTYPE AddRef() override {return ++refs;}
    ULONG STDMETHODCALLTYPE Release() override {auto n=--refs;if(!n)delete this;return n;}
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer,REFIID iid,void** out) override {
        if(outer)return CLASS_E_NOAGGREGATION;
        auto* command=new Command(Action::Root);
        auto hr=command->QueryInterface(iid,out);
        command->Release();
        return hr;
    }
    HRESULT STDMETHODCALLTYPE LockServer(BOOL lock) override {if(lock)++g_locks;else --g_locks;return S_OK;}
};

extern "C" BOOL WINAPI DllMain(HINSTANCE module,DWORD reason,LPVOID){if(reason==DLL_PROCESS_ATTACH){g_module=module;DisableThreadLibraryCalls(module);}return TRUE;}
STDAPI DllGetClassObject(REFCLSID clsid,REFIID iid,void** out){
    if(clsid!=CLSID_FileTime)return CLASS_E_CLASSNOTAVAILABLE;
    auto* factory=new Factory();auto hr=factory->QueryInterface(iid,out);factory->Release();return hr;
}
STDAPI DllCanUnloadNow(){return g_objects==0 && g_locks==0?S_OK:S_FALSE;}
