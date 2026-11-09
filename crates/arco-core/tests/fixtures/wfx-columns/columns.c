#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <string.h>
#include <wchar.h>
#ifdef WIDE
typedef WCHAR Char;
typedef WIN32_FIND_DATAW FindData;
#define C(s) L##s
#define Compare wcscmp
#define Copy wcscpy_s
#else
typedef CHAR Char;
typedef WIN32_FIND_DATAA FindData;
#define C(s) s
#define Compare strcmp
#define Copy strcpy_s
#endif

static BOOL entered = FALSE;

int __stdcall FxInit(int number, void *progress, void *log, void *request) {
    (void)number; (void)progress; (void)log; (void)request;
    return 0;
}

HANDLE __stdcall FxFindFirst(Char *name, FindData *entry) {
    if (Compare(name, C("\\Shelf")) != 0) {
        SetLastError(ERROR_PATH_NOT_FOUND);
        return INVALID_HANDLE_VALUE;
    }
    entered = TRUE;
    memset(entry, 0, sizeof(*entry));
    entry->dwFileAttributes = FILE_ATTRIBUTE_NORMAL;
    memcpy(entry->cFileName, C("item.txt"), sizeof(C("item.txt")));
    return (HANDLE)1;
}

BOOL __stdcall FxFindNext(HANDLE handle, FindData *entry) {
    (void)handle; (void)entry;
    SetLastError(ERROR_NO_MORE_FILES);
    return FALSE;
}

int __stdcall FxFindClose(HANDLE handle) {
    (void)handle;
    return 0;
}

int __stdcall FxFields(int index, char *name, char *units, int maxlen) {
    if (maxlen < 20) return 0;
    if (index == 0) {
        strcpy_s(name, maxlen, "Amount");
        strcpy_s(units, maxlen, "Bytes|KiB");
        return 2;
    }
    if (index == 1) {
        strcpy_s(name, maxlen, "Label");
        units[0] = '\0';
        return 8;
    }
    if (index == 2) {
        strcpy_s(name, maxlen, "Choice");
        strcpy_s(units, maxlen, "One|Two");
        return 7;
    }
    return 0;
}

int __stdcall FxSort(int index) {
    return index == 0 ? -1 : 1;
}

BOOL __stdcall FxDefaultView(Char *contents, Char *headers, Char *widths, Char *options, int maxlen) {
    if (maxlen < 80 || entered) return FALSE;
    Copy(contents, maxlen, C("[=<fs>.Amount.KiB]\\n[=fs.Label]\\n[=tc.writetime]"));
#ifdef WIDE
    Copy(headers, maxlen, L"Gr\x00f6\x00df" L"e\\n\x6f22\\nDatum");
#else
    Copy(headers, maxlen, "Amount\\nLabel\\nDate");
#endif
    Copy(widths, maxlen, C("148,0,-80,120,-130"));
    Copy(options, maxlen, C("-1|1"));
    return TRUE;
}

int __stdcall FxValue(Char *name, int field, int unit, void *value, int maxlen, int flags) {
    if (!entered || flags != 0 || maxlen < 50) return -2;
    if (Compare(name, C("\\Shelf\\empty.txt")) == 0) return -3;
#ifdef WIDE
    if (Compare(name, L"\\Shelf\\\x6f22 file.txt") != 0) return -2;
#else
    if (Compare(name, "\\Shelf\\plain file.txt") != 0) return -2;
#endif
    if (field == 0) {
        *(__int64 *)value = unit == 1 ? 3 : 3072;
        return 2;
    }
    if (field == 1) {
#ifdef WIDE
        wcscpy_s((wchar_t *)value, maxlen / sizeof(wchar_t), L"Unicode \x6f22");
        return 11;
#else
        strcpy_s((char *)value, maxlen, "ANSI label");
        return 8;
#endif
    }
    if (field == 2) {
        if (unit != 0) return -1;
        strcpy_s((char *)value, maxlen, "Two");
        return 7;
    }
    return -1;
}
