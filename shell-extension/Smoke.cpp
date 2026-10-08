#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shobjidl.h>
#include <shlwapi.h>
#include <cstdio>
#include <string>

#pragma comment(lib,"ole32.lib")
#pragma comment(lib,"shell32.lib")

int wmain(int argc,wchar_t** argv) {
    if(argc!=3) return 2;
    CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED);
    const CLSID clsid={0x134e91fa,0x54b5,0x48b4,{0x98,0xec,0x8c,0x9a,0x6e,0x6b,0x91,0x48}};
    HMODULE dll=nullptr;
    IClassFactory* factory=nullptr;
    IExplorerCommand* command=nullptr;
    if(wcscmp(argv[1],L"registered")==0){
        if(FAILED(CoCreateInstance(clsid,nullptr,CLSCTX_LOCAL_SERVER,IID_IExplorerCommand,(void**)&command)))return 14;
    }else{
        dll=LoadLibraryW(argv[1]);
        if(!dll) return 3;
        auto get=(HRESULT (WINAPI*)(REFCLSID,REFIID,void**))GetProcAddress(dll,"DllGetClassObject");
        if(!get) return 4;
        if(FAILED(get(clsid,IID_IClassFactory,(void**)&factory)))return 5;
        if(FAILED(factory->CreateInstance(nullptr,IID_IExplorerCommand,(void**)&command)))return 6;
    }
    IShellItem* file=nullptr;
    if(FAILED(SHCreateItemFromParsingName(argv[2],nullptr,IID_IShellItem,(void**)&file)))return 7;
    IShellItemArray* items=nullptr;
    if(FAILED(SHCreateShellItemArrayFromShellItem(file,IID_IShellItemArray,(void**)&items)))return 8;
    EXPCMDSTATE state{};
    if(FAILED(command->GetState(items,FALSE,&state)) || state!=ECS_ENABLED)return 9;
    IEnumExplorerCommand* children=nullptr;
    if(FAILED(command->EnumSubCommands(&children)))return 10;
    int count=0, recent=0;
    IExplorerCommand* child=nullptr;
    while(children->Next(1,&child,nullptr)==S_OK) {
        PWSTR title=nullptr;
        if(SUCCEEDED(child->GetTitle(items,&title)) && title) {
            wprintf(L"%s\n",title);
            if(wcsncmp(title,L"快速回滚到 ",6)==0)++recent;
            CoTaskMemFree(title);
            count++;
        }
        EXPCMDFLAGS flags{};
        if(SUCCEEDED(child->GetFlags(&flags)) && (flags&ECF_HASSUBCOMMANDS)) {
            IEnumExplorerCommand* nested=nullptr;
            if(FAILED(child->EnumSubCommands(&nested)))return 12;
            int nested_count=0;IExplorerCommand* version=nullptr;
            while(nested->Next(1,&version,nullptr)==S_OK){++nested_count;version->Release();}
            nested->Release();
            if(nested_count<1)return 13;
        }
        child->Release();
    }
    children->Release();items->Release();file->Release();command->Release();if(factory)factory->Release();
    if(dll)FreeLibrary(dll);CoUninitialize();
    printf("recent=%d\n",recent);
    return count>0?0:11;
}
