#include <windows.h>
#include <string.h>

#ifdef _WIN64
#define EXPORT __declspec(dllexport)
#else
#define EXPORT
#pragma comment(linker, "/export:ContentGetSupportedField=_ContentGetSupportedField@16")
#pragma comment(linker, "/export:ContentGetValue=_ContentGetValue@24")
#endif

EXPORT int __stdcall ContentGetSupportedField(int index, char *name, char *units, int size) {
    if (index != 0) return 0;
    strcpy_s(name, size, "Score");
    strcpy_s(units, size, "raw|scaled");
    return 1;
}

#ifdef _WIN64
EXPORT int __stdcall ContentGetValueW(wchar_t *path, int field, int unit, void *value, int capacity, int flags) {
#else
EXPORT int __stdcall ContentGetValue(char *path, int field, int unit, void *value, int capacity, int flags) {
#endif
    (void)flags;
    if (!path || field != 0 || unit < 0 || unit > 1 || capacity < (int)sizeof(int)) return -2;
    *(int *)value = 40 + unit;
    return 1;
}
