"""检查候选窗落在哪个 z 带，以及服务进程是否拿到 uiAccess 令牌。

为什么需要：候选窗被任务栏搜索面板盖住的根因是带号——面板在 ZBID_IMMERSIVE_SEARCH(13)，
桌面带是 1，两带之间 SetWindowPos 无效。uiAccess 令牌能把窗口放进 ZBID_UIACCESS(2)，
而 2 带排在各 immersive 带之上。令牌有四个条件，任一不满足都会静默退回桌面带，
所以「进程起来了」不等于「生效了」，只有 band 号能证明。

用法：
    python tools\\heng-bandcheck.py              # 采样 25 秒，期间在搜索框里打字
    python tools\\heng-bandcheck.py --seconds 60
    python tools\\heng-bandcheck.py --once       # 只看当前状态

输出示例（生效）：
    [server] pid=1234 path=C:\\Program Files\\HengIME\\WeaselServer.exe uiaccess=1
    [cand]   heng_ui_cand band=2 rect=(88,1369,526,59)
    [fg]     Windows.UI.Core.CoreWindow band=13 rect=(48,760,784,680)
"""

import argparse
import ctypes
import ctypes.wintypes as w
import time

u32 = ctypes.WinDLL('user32', use_last_error=True)
k32 = ctypes.WinDLL('kernel32', use_last_error=True)
adv = ctypes.WinDLL('advapi32', use_last_error=True)

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
TOKEN_QUERY = 0x0008
TOKEN_UIACCESS = 26
TH32CS_SNAPPROCESS = 0x2


class PROCESSENTRY32(ctypes.Structure):
    _fields_ = [
        ('dwSize', w.DWORD),
        ('cntUsage', w.DWORD),
        ('th32ProcessID', w.DWORD),
        ('th32DefaultHeapID', ctypes.c_size_t),
        ('th32ModuleID', w.DWORD),
        ('cntThreads', w.DWORD),
        ('th32ParentProcessID', w.DWORD),
        ('pcPriClassBase', ctypes.c_long),
        ('dwFlags', w.DWORD),
        ('szExeFile', w.WCHAR * 260),
    ]


def find_pids(name='WeaselServer.exe'):
    snap = k32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if snap == -1:
        return []
    out = []
    pe = PROCESSENTRY32()
    pe.dwSize = ctypes.sizeof(PROCESSENTRY32)
    ok = k32.Process32FirstW(snap, ctypes.byref(pe))
    while ok:
        if pe.szExeFile.lower() == name.lower():
            out.append(pe.th32ProcessID)
        ok = k32.Process32NextW(snap, ctypes.byref(pe))
    k32.CloseHandle(snap)
    return out


def process_path(pid):
    h = k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return None
    buf = ctypes.create_unicode_buffer(1024)
    size = w.DWORD(1024)
    ok = k32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size))
    k32.CloseHandle(h)
    return buf.value if ok else None


def token_uiaccess(pid):
    """返回 1/0；打不开令牌返回 None（跨会话或权限不足）。"""
    h = k32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        return None
    tok = ctypes.c_void_p()
    if not adv.OpenProcessToken(h, TOKEN_QUERY, ctypes.byref(tok)):
        k32.CloseHandle(h)
        return None
    val = ctypes.c_int(0)
    ret = w.DWORD(0)
    ok = adv.GetTokenInformation(tok, TOKEN_UIACCESS, ctypes.byref(val), 4,
                                 ctypes.byref(ret))
    k32.CloseHandle(tok)
    k32.CloseHandle(h)
    return val.value if ok else None


def window_band(hwnd):
    fn = getattr(u32, 'GetWindowBand', None)
    if fn is None:
        return -1
    fn.restype = w.BOOL
    fn.argtypes = [w.HWND, ctypes.POINTER(w.DWORD)]
    b = w.DWORD(0)
    return b.value if fn(hwnd, ctypes.byref(b)) else -1


def describe(hwnd):
    cn = ctypes.create_unicode_buffer(256)
    u32.GetClassNameW(hwnd, cn, 256)
    r = w.RECT()
    u32.GetWindowRect(hwnd, ctypes.byref(r))
    return cn.value, window_band(hwnd), (r.left, r.top, r.right - r.left, r.bottom - r.top)


CAND_CLASSES = ('heng_ui_cand', 'heng_ui_letters')


def snapshot():
    """(候选窗行, 前台行)。前台取 GetForegroundWindow 的类名与带。"""
    rows = []
    fg = u32.GetForegroundWindow()
    fg_info = describe(fg) if fg else None

    @ctypes.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    def cb(h, _):
        info = describe(h)
        if info[0] in CAND_CLASSES and u32.IsWindowVisible(h):
            rows.append(info)
        return True

    u32.EnumWindows(cb, 0)
    return rows, fg_info


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--seconds', type=int, default=25)
    ap.add_argument('--once', action='store_true')
    args = ap.parse_args()

    pids = find_pids()
    if not pids:
        print('[server] 没有运行中的 WeaselServer.exe')
    for pid in pids:
        path = process_path(pid) or '?'
        uia = token_uiaccess(pid)
        tag = {1: '1  ← 有令牌，候选窗应进 2 带', 0: '0  ← 无令牌，退回桌面带', None: '?（打不开令牌）'}[uia]
        print('[server] pid=%d uiaccess=%s' % (pid, tag))
        print('         %s' % path)

    print()
    print('现在切到任务栏搜索框打字，观察下面是否出现 band=2 的候选窗…'
          if not args.once else '当前状态：')

    last = None
    deadline = time.time() + (1 if args.once else args.seconds)
    while time.time() < deadline:
        rows, fg = snapshot()
        sig = (tuple(rows), fg)
        if sig != last:
            last = sig
            for cls, band, rect in rows:
                print('[cand]   %-16s band=%d rect=%s' % (cls, band, rect))
            if fg:
                print('[fg]     %-16s band=%d rect=%s' % (fg[0][:16], fg[1], fg[2]))
            if not rows:
                print('[cand]   （没有可见的 heng_ui 候选窗）')
            print()
        time.sleep(0.15)

    print('判读：候选窗 band=2 → uiAccess 生效，可压在搜索面板（band=13）之上；')
    print('      band=1 → 令牌没落下，只能靠避让定位，仍会被面板遮挡。')


if __name__ == '__main__':
    main()
