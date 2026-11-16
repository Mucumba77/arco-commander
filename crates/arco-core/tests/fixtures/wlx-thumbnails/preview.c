#include <windows.h>
typedef struct { int size; DWORD version_low; DWORD version_high; char ini[MAX_PATH]; } Defaults;
static int initialized;
static void test_delay(const char *name) {
    char value[16];
    DWORD length = GetEnvironmentVariableA(name, value, sizeof(value));
    if (!length || length >= sizeof(value)) return;
    DWORD delay = 0;
    for (DWORD i = 0; i < length; ++i) {
        if (value[i] < '0' || value[i] > '9') return;
        delay = delay * 10 + value[i] - '0';
        if (delay > 15000) return;
    }
    Sleep(delay);
}
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance;
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) test_delay("ARCO_TEST_WLX_LOAD_DELAY_MS");
    return TRUE;
}
#ifdef _WIN64
#define WLX_EXPORT __declspec(dllexport)
#else
#define WLX_EXPORT
#pragma comment(linker, "/export:ListSetDefaultParams=_ListSetDefaultParams@4")
#pragma comment(linker, "/export:ListGetPreviewBitmap=_ListGetPreviewBitmap@20")
#endif
WLX_EXPORT void __stdcall ListSetDefaultParams(Defaults *params) {
    initialized = params && params->size == sizeof(Defaults) && params->version_high == 2 && params->ini[0] != 0;
}
static HBITMAP draw(int width, int height, const unsigned char* content, int length) {
    if (!initialized || width != 144 || height != 88 || length != 8192 || content[1] || content[8191] != 'Z') return NULL;
    if (content[0] != 'A' && content[0] != 'O') return NULL;
    test_delay("ARCO_TEST_WLX_PREVIEW_DELAY_MS");
    BITMAPINFO info = {0};
    info.bmiHeader.biSize = sizeof(info.bmiHeader);
    info.bmiHeader.biWidth = content[0] == 'O' ? width + 1 : 2;
    info.bmiHeader.biHeight = -2;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    DWORD *pixels = NULL;
    HBITMAP bitmap = CreateDIBSection(NULL, &info, DIB_RGB_COLORS, (void **)&pixels, NULL, 0);
    if (bitmap && pixels) {
        pixels[0] = 0x00E02010;
        pixels[1] = 0x00E02010;
        pixels[info.bmiHeader.biWidth] = 0x0011263D;
        pixels[info.bmiHeader.biWidth + 1] = 0x0011263D;
    }
    return bitmap;
}
#ifdef _WIN64
WLX_EXPORT HBITMAP __stdcall ListGetPreviewBitmapW(wchar_t* file, int width, int height, unsigned char* content, int length) {
#else
WLX_EXPORT HBITMAP __stdcall ListGetPreviewBitmap(char* file, int width, int height, unsigned char* content, int length) {
#endif
    if (!file || !*file) return NULL;
    return draw(width, height, content, length);
}
