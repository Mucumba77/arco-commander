/* MIT licensed test fixture. Minimal Unicode WCX SDK ABI, no UI or callbacks. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <string.h>
#include <wchar.h>
typedef struct {
    wchar_t archive[1024], name[1024];
    int flags;
    unsigned packed, packed_high, size, size_high;
    int host_os, crc, time, version, method, attributes;
    wchar_t *comment;
    int comment_capacity, comment_size, comment_state;
    char reserved[1024];
} Header;
__declspec(dllexport) void * __stdcall OpenArchiveW(void *data) {
    (void)data; return HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(int));
}
__declspec(dllexport) int __stdcall ReadHeaderExW(void *handle, Header *header) {
    int *index = (int *)handle;
    if ((*index)++) return 10;
    memset(header, 0, sizeof(*header));
    wcscpy_s(header->name, 1024, L"sample.txt");
    header->size = header->packed = 6;
    return 0;
}
__declspec(dllexport) int __stdcall ProcessFileW(void *handle, int action, wchar_t *directory, wchar_t *target) {
    (void)handle; (void)directory;
    if (action != 2) return 0;
    HANDLE file = CreateFileW(target, GENERIC_WRITE, 0, NULL, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) return 17;
    DWORD written = 0;
    BOOL ok = WriteFile(file, "plugin", 6, &written, NULL);
    CloseHandle(file);
    return ok && written == 6 ? 0 : 19;
}
__declspec(dllexport) int __stdcall CloseArchive(void *handle) {
    HeapFree(GetProcessHeap(), 0, handle); return 0;
}
