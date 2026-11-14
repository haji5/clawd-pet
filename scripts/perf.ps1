#Requires -Version 5.1
<#
.SYNOPSIS
    Measures Clawd against the budgets in PLAN.md §1 and shows where each cost
    comes from.

.DESCRIPTION
    Every pet this script starts is isolated and never appears on the screen:
    its own APPDATA and TEMP in a throwaway folder, a spare UDP port (which
    also gives it its own single-instance mutex, so it runs beside your pet),
    and either --hidden or --offscreen (the real window, timers and drawing,
    parked off every monitor). Both flags are always passed, so an old exe
    that doesn't know --offscreen stays hidden instead of showing up.

    Sections (-Only):
      static   exe size, sections, imports
      startup  process start to first window on screen / message loop idle
      hook     --hook with stdin ready: median, p95, worst, per payload
      memory   working set split by DLL and region, private vs shared
      states   CPU and actual timer fires/redraws in every state
      extras   what the tooltip and the chime cost once used
      soak     growth over a long, busy run (leaks)

.EXAMPLE
    scripts\perf.ps1                                  # everything, 5 min per state
.EXAMPLE
    scripts\perf.ps1 -Only hook,memory -Json out.json
.EXAMPLE
    scripts\perf.ps1 -Only states -Seconds 30 -State idle,bash -Compare before.json
#>
[CmdletBinding()]
param(
    [string]$Exe,
    [ValidateSet('all', 'static', 'startup', 'hook', 'memory', 'states', 'extras', 'soak')]
    [string[]]$Only = @('all'),
    # Seconds sampled per state (PLAN §9 asks for 300).
    [int]$Seconds = 300,
    [int]$HookRuns = 300,
    [int]$StartupRuns = 30,
    [int]$SoakMinutes = 30,
    [int]$Port = 47899,
    # Only these states (default: all of them).
    [string[]]$State,
    [string]$Json,
    # A previous -Json file: print before → after.
    [string]$Compare,
    # Second exe for -Only states: runs beside -Exe in the same seconds with the
    # same events, so a change is compared under the same conditions. Runs
    # apart differ by more than most changes do. Same exe twice = the noise floor.
    [string]$Against,
    # Read-only look at the memory of a pet that's already running (yours, say):
    # nothing is sent to it, trimmed or changed.
    [int]$InspectPid = 0,
    # Keep this script and everything it starts on the first N logical CPUs
    # (12 = the P-cores of a 6P+8E chip), which removes the difference between
    # landing on a fast or a slow core.
    [int]$PinCpus = 0
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $repo 'target\release\clawd-pet.exe' }
$Exe = (Resolve-Path $Exe).Path
if ($Against) { $Against = (Resolve-Path $Against).Path }

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public sealed class Snap {
    public double T;
    public ulong Cycles;
    public long UserTicks, KernelTicks, CtxSwitches, WS, PrivateWS, PeakWS, Commit, PageFaults, HardFaults;
    public int Handles, Gdi, GdiPeak, User, UserPeak, Threads;
    public Dictionary<long, long> ByThread = new Dictionary<long, long>();
}

public sealed class HookResult {
    public double Ms, CpuMs;
    public uint Exit;
    public long PageFaults, PeakWS;
}

public sealed class WsRow {
    public string Name, Kind;
    public long Total, Exec, ReadOnly, Writable, Shareable, SharedNow, Region;
}

public static class Perf {
    const uint QUERY = 0x0400, VMREAD = 0x0010, QUERY_LIMITED = 0x1000, SYNCHRONIZE = 0x00100000;

    [DllImport("kernel32", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32", SetLastError = true)] static extern IntPtr OpenThread(uint access, bool inherit, int tid);
    [DllImport("kernel32")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32")] static extern bool QueryProcessCycleTime(IntPtr h, out ulong cycles);
    [DllImport("kernel32")] static extern bool QueryThreadCycleTime(IntPtr h, out ulong cycles);
    [DllImport("kernel32")] static extern IntPtr GetCurrentThread();
    [DllImport("kernel32")] static extern bool GetProcessTimes(IntPtr h, out long create, out long exit, out long kernel, out long user);
    [DllImport("kernel32")] static extern bool GetSystemTimes(out long idle, out long kernel, out long user);
    [DllImport("kernel32")] static extern bool GetProcessHandleCount(IntPtr h, out int count);
    [DllImport("kernel32")] static extern bool QueryPerformanceCounter(out long v);
    [DllImport("kernel32")] static extern bool QueryPerformanceFrequency(out long v);
    [DllImport("user32")] static extern int GetGuiResources(IntPtr h, int flags);
    [DllImport("psapi", SetLastError = true)] static extern bool QueryWorkingSet(IntPtr h, IntPtr buf, int size);
    [DllImport("psapi", CharSet = CharSet.Unicode)] static extern int GetMappedFileNameW(IntPtr h, IntPtr addr, StringBuilder name, int size);
    [DllImport("psapi")] static extern bool GetProcessMemoryInfo(IntPtr h, out PMC c, int size);
    [DllImport("ntdll")] static extern int NtQuerySystemInformation(int cls, IntPtr buf, int size, out int ret);
    [DllImport("ntdll")] static extern int NtQueryInformationProcess(IntPtr h, int cls, IntPtr buf, int size, out int ret);
    [DllImport("ntdll")] static extern int NtQueryInformationThread(IntPtr h, int cls, IntPtr buf, int size, out int ret);
    [DllImport("kernel32", SetLastError = true)] static extern bool ReadProcessMemory(IntPtr h, IntPtr addr, IntPtr buf, IntPtr size, out IntPtr read);
    [DllImport("kernel32", SetLastError = true)] static extern IntPtr VirtualQueryEx(IntPtr h, IntPtr addr, out MBI info, IntPtr size);

    [StructLayout(LayoutKind.Sequential)] struct MBI {
        public IntPtr BaseAddress, AllocationBase; public uint AllocationProtect; public uint pad0;
        public IntPtr RegionSize; public uint State, Protect, Type, pad1;
    }
    [StructLayout(LayoutKind.Sequential)] struct PMC {
        public uint cb, PageFaultCount; public IntPtr PeakWS, WS, a, b, c, d, Pagefile, PeakPagefile, Private;
    }

    static double Qpc() { long v, f; QueryPerformanceCounter(out v); QueryPerformanceFrequency(out f); return (double)v / f; }

    /// Cycle counters tick at the TSC rate: measured, so CPU can be expressed in time.
    public static double TscHz() {
        ulong c0, c1; double t0 = Qpc(); QueryThreadCycleTime(GetCurrentThread(), out c0);
        while (Qpc() - t0 < 0.25) { }
        QueryThreadCycleTime(GetCurrentThread(), out c1);
        return (c1 - c0) / (Qpc() - t0);
    }

    /// Percent of all logical processors busy over `ms`.
    public static double SystemBusy(int ms) {
        long i0, k0, u0, i1, k1, u1;
        GetSystemTimes(out i0, out k0, out u0); Thread.Sleep(ms); GetSystemTimes(out i1, out k1, out u1);
        long total = (k1 - k0) + (u1 - u0), idle = i1 - i0;
        return total == 0 ? 0 : 100.0 * (total - idle) / total;
    }

    /// The kernel's own bookkeeping for one process, incl. every thread's context switches.
    public static Snap Snapshot(int pid) {
        int size = 1 << 21; IntPtr buf = Marshal.AllocHGlobal(size);
        try {
            int ret;
            int st;
            while ((st = NtQuerySystemInformation(5, buf, size, out ret)) == unchecked((int)0xC0000004)) {
                Marshal.FreeHGlobal(buf); size *= 2; buf = Marshal.AllocHGlobal(size);
            }
            if (st != 0) throw new Win32Exception(st);
            int off = 0;
            while (true) {
                IntPtr p = buf + off;
                if (Marshal.ReadIntPtr(p, 80).ToInt64() == pid) {
                    var s = new Snap();
                    s.T = Qpc();
                    int n = Marshal.ReadInt32(p, 4);
                    s.Threads = n;
                    s.PrivateWS = Marshal.ReadInt64(p, 8);
                    s.HardFaults = (uint)Marshal.ReadInt32(p, 16);
                    s.UserTicks = Marshal.ReadInt64(p, 40);
                    s.KernelTicks = Marshal.ReadInt64(p, 48);
                    s.Handles = Marshal.ReadInt32(p, 96);
                    s.PageFaults = (uint)Marshal.ReadInt32(p, 128);
                    s.PeakWS = Marshal.ReadInt64(p, 136);
                    s.WS = Marshal.ReadInt64(p, 144);
                    s.Commit = Marshal.ReadInt64(p, 200);
                    for (int i = 0; i < n; i++) {
                        long sw = (uint)Marshal.ReadInt32(p, 256 + i * 80 + 64);
                        s.CtxSwitches += sw;
                        s.ByThread[Marshal.ReadInt64(p, 256 + i * 80 + 48)] = sw;
                    }
                    IntPtr h = OpenProcess(QUERY, false, pid);
                    if (h != IntPtr.Zero) {
                        QueryProcessCycleTime(h, out s.Cycles);
                        s.Gdi = GetGuiResources(h, 0); s.User = GetGuiResources(h, 1);
                        s.GdiPeak = GetGuiResources(h, 2); s.UserPeak = GetGuiResources(h, 4);
                        CloseHandle(h);
                    }
                    return s;
                }
                int next = Marshal.ReadInt32(p, 0);
                if (next == 0) return null;
                off += next;
            }
        } finally { Marshal.FreeHGlobal(buf); }
    }

    /// Context switches between two snapshots, over the threads alive in
    /// both: the loader's worker threads exit a few seconds after startup.
    public static long Switches(Snap a, Snap b) {
        long sum = 0;
        foreach (var kv in b.ByThread) { long before; if (a.ByThread.TryGetValue(kv.Key, out before)) sum += kv.Value - before; }
        return sum;
    }

    // ---- process start ----

    const uint STARTF_USESTDHANDLES = 0x100, INFINITE = 0xFFFFFFFF, CREATE_UNICODE_ENVIRONMENT = 0x400;
    const int HANDLE_FLAG_INHERIT = 1;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct STARTUPINFO {
        public int cb; public string reserved, desktop, title;
        public int x, y, xs, ys, xc, yc, fill, flags; public short show, r2; public IntPtr r3, stdin, stdout, stderr;
    }
    [StructLayout(LayoutKind.Sequential)] struct PI { public IntPtr proc, thread; public int pid, tid; }
    [StructLayout(LayoutKind.Sequential)] struct SA { public int len; public IntPtr desc; public int inherit; }

    [DllImport("kernel32", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CreateProcessW(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PI pi);
    [DllImport("kernel32", SetLastError = true)] static extern bool CreatePipe(out IntPtr r, out IntPtr w, ref SA sa, uint size);
    [DllImport("kernel32", SetLastError = true)] static extern bool SetHandleInformation(IntPtr h, int mask, int flags);
    [DllImport("kernel32", SetLastError = true)] static extern bool WriteFile(IntPtr h, byte[] b, int n, out int written, IntPtr o);
    [DllImport("kernel32", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr CreateFileW(string name, uint access, uint share, ref SA sa, uint disp, uint flags, IntPtr t);
    [DllImport("kernel32")] static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32")] static extern bool GetExitCodeProcess(IntPtr h, out uint code);
    [DllImport("user32")] static extern uint WaitForInputIdle(IntPtr h, uint ms);

    static IntPtr Nul() {
        var sa = new SA { len = Marshal.SizeOf(typeof(SA)), inherit = 1 };
        return CreateFileW("NUL", 0x40000000, 3, ref sa, 3, 0, IntPtr.Zero);
    }

    static IntPtr Pipe(byte[] data, out IntPtr writer) {
        var sa = new SA { len = Marshal.SizeOf(typeof(SA)), inherit = 1 };
        IntPtr r, w;
        if (!CreatePipe(out r, out w, ref sa, (uint)Math.Max(data.Length + 64, 65536))) throw new Win32Exception();
        SetHandleInformation(w, HANDLE_FLAG_INHERIT, 0);
        if (data.Length > 0) { int n; if (!WriteFile(w, data, data.Length, out n, IntPtr.Zero)) throw new Win32Exception(); }
        writer = w;
        return r;
    }

    /// Runs `exe args` with `stdin` already written and closed, as PLAN §1
    /// defines the budget: process start to exit.
    public static HookResult RunHook(string exe, string args, byte[] stdin, string cwd) {
        IntPtr w, r = Pipe(stdin, out w), nul = Nul();
        CloseHandle(w);
        var si = new STARTUPINFO { cb = Marshal.SizeOf(typeof(STARTUPINFO)), flags = (int)STARTF_USESTDHANDLES, stdin = r, stdout = nul, stderr = nul };
        var cmd = new StringBuilder("\"" + exe + "\" " + args);
        PI pi;
        double t0 = Qpc();
        if (!CreateProcessW(exe, cmd, IntPtr.Zero, IntPtr.Zero, true, CREATE_UNICODE_ENVIRONMENT, IntPtr.Zero, cwd, ref si, out pi)) throw new Win32Exception();
        WaitForSingleObject(pi.proc, INFINITE);
        double t1 = Qpc();
        var res = new HookResult { Ms = (t1 - t0) * 1000 };
        GetExitCodeProcess(pi.proc, out res.Exit);
        long c, e, k, u; GetProcessTimes(pi.proc, out c, out e, out k, out u);
        res.CpuMs = (k + u) / 10000.0;
        PMC m; if (GetProcessMemoryInfo(pi.proc, out m, Marshal.SizeOf(typeof(PMC)))) { res.PageFaults = m.PageFaultCount; res.PeakWS = m.PeakWS.ToInt64(); }
        CloseHandle(pi.proc); CloseHandle(pi.thread); CloseHandle(r); CloseHandle(nul);
        return res;
    }

    /// The DLLs a hook has loaded by the time it's waiting on stdin (stdin
    /// stays open for a moment, so it can be inspected).
    public static string[] ModulesWhileWaiting(string exe, string args, string cwd) {
        IntPtr w, r = Pipe(new byte[0], out w), nul = Nul();
        var si = new STARTUPINFO { cb = Marshal.SizeOf(typeof(STARTUPINFO)), flags = (int)STARTF_USESTDHANDLES, stdin = r, stdout = nul, stderr = nul };
        var cmd = new StringBuilder("\"" + exe + "\" " + args);
        PI pi;
        if (!CreateProcessW(exe, cmd, IntPtr.Zero, IntPtr.Zero, true, CREATE_UNICODE_ENVIRONMENT, IntPtr.Zero, cwd, ref si, out pi)) throw new Win32Exception();
        Thread.Sleep(300);
        var names = Process.GetProcessById(pi.pid).Modules.Cast<ProcessModule>().Select(m => m.ModuleName).ToArray();
        CloseHandle(w);
        WaitForSingleObject(pi.proc, 5000);
        CloseHandle(pi.proc); CloseHandle(pi.thread); CloseHandle(r); CloseHandle(nul);
        return names;
    }

    public static int Spawn(string exe, string args, string cwd) {
        IntPtr nul = Nul();
        var si = new STARTUPINFO { cb = Marshal.SizeOf(typeof(STARTUPINFO)), flags = (int)STARTF_USESTDHANDLES, stdin = nul, stdout = nul, stderr = nul };
        var cmd = new StringBuilder("\"" + exe + "\" " + args);
        PI pi;
        if (!CreateProcessW(exe, cmd, IntPtr.Zero, IntPtr.Zero, true, CREATE_UNICODE_ENVIRONMENT, IntPtr.Zero, cwd, ref si, out pi)) throw new Win32Exception();
        CloseHandle(pi.proc); CloseHandle(pi.thread); CloseHandle(nul);
        return pi.pid;
    }

    delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32")] static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32")] static extern uint GetWindowThreadProcessId(IntPtr h, out int pid);
    [DllImport("user32", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32")] static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);

    public static IntPtr PetWindow(int pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            int p; GetWindowThreadProcessId(h, out p);
            if (p == pid) {
                var s = new StringBuilder(64); GetClassNameW(h, s, 64);
                if (s.ToString() == "ClawdPetWindow") { found = h; return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    [DllImport("kernel32")] static extern uint SetThreadExecutionState(uint flags);
    /// A long run spoiled by the laptop going to sleep is hours lost. Ends with this script.
    public static void StayAwake(bool on) { SetThreadExecutionState(on ? 0x80000001u : 0x80000000u); }

    [DllImport("user32")] static extern IntPtr GetForegroundWindow();
    /// Which program is in front: a busy one wakes Clawd through the foreground hook.
    public static string ForegroundName() {
        int p; GetWindowThreadProcessId(GetForegroundWindow(), out p);
        try { return Process.GetProcessById(p).ProcessName; } catch (Exception) { return "?"; }
    }

    public static void Post(IntPtr h, uint msg, long w, long l) { PostMessageW(h, msg, new IntPtr(w), new IntPtr(l)); }

    /// Start to the window being on screen (`visible`) or to the message
    /// loop going idle, in ms; [-1,-1] on a timeout.
    public static double[] TimeToUp(string exe, string args, string cwd, bool waitVisible) {
        IntPtr nul = Nul();
        var si = new STARTUPINFO { cb = Marshal.SizeOf(typeof(STARTUPINFO)), flags = (int)STARTF_USESTDHANDLES, stdin = nul, stdout = nul, stderr = nul };
        var cmd = new StringBuilder("\"" + exe + "\" " + args);
        PI pi;
        double t0 = Qpc();
        if (!CreateProcessW(exe, cmd, IntPtr.Zero, IntPtr.Zero, true, CREATE_UNICODE_ENVIRONMENT, IntPtr.Zero, cwd, ref si, out pi)) throw new Win32Exception();
        double visible = -1, idle = -1;
        var deadline = t0 + 5;
        if (waitVisible) {
            while (Qpc() < deadline) {
                IntPtr w = PetWindow(pi.pid);
                if (w != IntPtr.Zero && IsWindowVisible(w)) { visible = (Qpc() - t0) * 1000; break; }
                Thread.SpinWait(200);
            }
        }
        if (WaitForInputIdle(pi.proc, 5000) == 0) idle = (Qpc() - t0) * 1000;
        CloseHandle(pi.proc); CloseHandle(pi.thread); CloseHandle(nul);
        return new[] { visible, idle, pi.pid };
    }

    // ---- UDP ----

    public static void Udp(int port, string json) {
        using (var c = new UdpClient()) { var b = Encoding.UTF8.GetBytes(json); c.Send(b, b.Length, new IPEndPoint(IPAddress.Loopback, port)); }
    }

    /// Stands in for the pet: counts what the hooks really send.
    public sealed class Sink : IDisposable {
        readonly Socket s;
        public Sink(int port) {
            s = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
            s.ReceiveBufferSize = 8 * 1024 * 1024;
            s.Bind(new IPEndPoint(IPAddress.Loopback, port));
            s.Blocking = false;
        }
        public int Drain() {
            int n = 0; var b = new byte[2048];
            while (s.Poll(0, SelectMode.SelectRead)) { s.Receive(b); n++; }
            return n;
        }
        public void Dispose() { s.Dispose(); }
    }

    [DllImport("kernel32", SetLastError = true, CharSet = CharSet.Unicode)] static extern IntPtr CreateMutexW(IntPtr a, bool owner, string name);
    /// Stands in for a running pet, so a SessionStart takes the usual path.
    public static IntPtr HoldMutex(string name) { return CreateMutexW(IntPtr.Zero, false, name); }
    public static void Close(IntPtr h) { CloseHandle(h); }

    /// A real input stream: press, then `hz` moves a second along a sine, release.
    public static int Drag(IntPtr[] hwnds, int hz, double seconds, int x0, int y0) {
        const uint DOWN = 0x201, MOVE = 0x200;
        Func<double, long> at = t => {
            int x = x0 + (int)(300 * Math.Sin(t * 2.0));
            int y = y0 + (int)(80 * Math.Cos(t * 3.1));
            return (long)(uint)((x & 0xFFFF) | ((y & 0xFFFF) << 16));
        };
        foreach (var h in hwnds) Post(h, DOWN, 1, at(0));
        double t0 = Qpc(), step = 1.0 / hz; int n = 0;
        while (true) {
            double t = Qpc() - t0;
            if (t >= seconds) break;
            if (t >= n * step) {
                // Take turns, so neither pet always gets its message first.
                for (int k = 0; k < hwnds.Length; k++) Post(hwnds[n % 2 == 0 ? k : hwnds.Length - 1 - k], MOVE, 1, at(t));
                n++;
            }
            else if (n * step - t > 0.003) Thread.Sleep(1);
        }
        return n;
    }
    public static void Release(IntPtr hwnd, int x, int y) { Post(hwnd, 0x202, 0, (long)(uint)((x & 0xFFFF) | ((y & 0xFFFF) << 16))); }

    // ---- the working set, page by page ----

    static long ReadPtr(IntPtr proc, long addr) {
        IntPtr b = Marshal.AllocHGlobal(8), read;
        try { return ReadProcessMemory(proc, new IntPtr(addr), b, new IntPtr(8), out read) ? Marshal.ReadInt64(b) : 0; }
        finally { Marshal.FreeHGlobal(b); }
    }
    static int ReadInt(IntPtr proc, long addr) {
        IntPtr b = Marshal.AllocHGlobal(4), read;
        try { return ReadProcessMemory(proc, new IntPtr(addr), b, new IntPtr(4), out read) ? Marshal.ReadInt32(b) : 0; }
        finally { Marshal.FreeHGlobal(b); }
    }
    static long PebOf(IntPtr h) {
        IntPtr b = Marshal.AllocHGlobal(48);
        try { int r; return NtQueryInformationProcess(h, 0, b, 48, out r) == 0 ? Marshal.ReadInt64(b, 8) : 0; }
        finally { Marshal.FreeHGlobal(b); }
    }

    /// Every resident page, attributed to the DLL or region it belongs to.
    /// Sizes in KB. `Shareable` could be shared; `SharedNow` has another user.
    public static List<WsRow> WorkingSetMap(int pid) {
        IntPtr h = OpenProcess(QUERY | VMREAD, false, pid);
        if (h == IntPtr.Zero) throw new Win32Exception();
        var rows = new Dictionary<string, WsRow>();
        int bytes = 8 + 8 * 1 << 20;
        IntPtr buf = Marshal.AllocHGlobal(bytes);
        try {
            if (!QueryWorkingSet(h, buf, bytes)) throw new Win32Exception();
            long n = Marshal.ReadInt64(buf);
            var pages = new SortedDictionary<long, long>();
            for (long i = 0; i < n; i++) { long e = Marshal.ReadInt64(buf, (int)(8 + i * 8)); pages[e & ~0xFFFL] = e; }

            long peb = PebOf(h);
            var heaps = new HashSet<long>();
            int nh = ReadInt(h, peb + 0xE8); long ha = ReadPtr(h, peb + 0xF0);
            for (int i = 0; i < nh; i++) heaps.Add(ReadPtr(h, ha + i * 8));
            var stacks = new List<long[]>();
            foreach (ProcessThread t in Process.GetProcessById(pid).Threads) {
                IntPtr th = OpenThread(0x0040, false, t.Id);
                if (th == IntPtr.Zero) continue;
                IntPtr b = Marshal.AllocHGlobal(48);
                int r;
                if (NtQueryInformationThread(th, 0, b, 48, out r) == 0) {
                    long teb = Marshal.ReadInt64(b, 8);
                    stacks.Add(new[] { ReadPtr(h, teb + 0x1478), ReadPtr(h, teb + 8), teb });
                }
                Marshal.FreeHGlobal(b); CloseHandle(th);
            }

            IntPtr addr = IntPtr.Zero; MBI m; int mbi = Marshal.SizeOf(typeof(MBI));
            var sb = new StringBuilder(600);
            while (VirtualQueryEx(h, addr, out m, new IntPtr(mbi)) != IntPtr.Zero) {
                long start = m.BaseAddress.ToInt64(), size = m.RegionSize.ToInt64();
                if (m.State == 0x1000) {
                    string name = null, kind = null;
                    if (m.Type == 0x1000000 || m.Type == 0x40000) {
                        sb.Clear();
                        int len = GetMappedFileNameW(h, m.AllocationBase, sb, 600);
                        string f = len > 0 ? Path.GetFileName(sb.ToString()) : "(unnamed)";
                        if (m.Type == 0x1000000) { name = f.ToLowerInvariant(); kind = "image"; }
                        else { name = len > 0 ? "mapped: " + f : "(pagefile section)"; kind = "mapped"; }
                    } else {
                        long ab = m.AllocationBase.ToInt64();
                        if (heaps.Contains(ab)) { name = "(heap " + ab.ToString("x") + ")"; kind = "heap"; }
                        foreach (var s in stacks) {
                            if (start >= s[0] && start < s[1]) { name = "(stack)"; kind = "stack"; break; }
                            if (start <= s[2] && s[2] < start + size) { name = "(TEB/PEB)"; kind = "other"; }
                        }
                        if (name == null) { name = start <= peb && peb < start + size ? "(TEB/PEB)" : "(private " + ab.ToString("x") + ")"; kind = "other"; }
                    }
                    WsRow row;
                    if (!rows.TryGetValue(name, out row)) rows[name] = row = new WsRow { Name = name, Kind = kind };
                    row.Region += size / 1024;
                    for (long p = start; p < start + size; p += 4096) {
                        long e;
                        if (!pages.TryGetValue(p, out e)) continue;
                        row.Total += 4;
                        uint prot = m.Protect & 0xFF;
                        if (prot == 0x10 || prot == 0x20 || prot == 0x40 || prot == 0x80) row.Exec += 4;
                        else if (prot == 0x02) row.ReadOnly += 4;
                        else row.Writable += 4;
                        if ((e & 0x100) != 0) { row.Shareable += 4; if (((e >> 5) & 7) > 1) row.SharedNow += 4; }
                    }
                }
                long next = start + size;
                if (next <= start) break;
                addr = new IntPtr(next);
            }
        } finally { Marshal.FreeHGlobal(buf); CloseHandle(h); }
        return rows.Values.Where(r => r.Total > 0).OrderByDescending(r => r.Total).ToList();
    }

    [DllImport("kernel32")] static extern bool SetProcessWorkingSetSize(IntPtr h, IntPtr min, IntPtr max);
    /// Empties another process's working set, to see what it really keeps touching.
    public static bool Trim(int pid) {
        IntPtr h = OpenProcess(QUERY | 0x0100, false, pid);
        try { return SetProcessWorkingSetSize(h, new IntPtr(-1), new IntPtr(-1)); } finally { CloseHandle(h); }
    }
}
'@

if ($InspectPid) {
    $s = [Perf]::Snapshot($InspectPid)
    if (-not $s) { throw "No process $InspectPid" }
    Write-Host ("PID {0}: WS {1:N1} MB, private WS {2:N1} MB, commit {3:N1} MB, peak WS {4:N1} MB, faults {5}, threads {6}, GDI {7} (peak {8}), USER {9} (peak {10}), handles {11}" -f $InspectPid, ($s.WS / 1MB), ($s.PrivateWS / 1MB), ($s.Commit / 1MB), ($s.PeakWS / 1MB), $s.PageFaults, $s.Threads, $s.Gdi, $s.GdiPeak, $s.User, $s.UserPeak, $s.Handles)
    $map = [Perf]::WorkingSetMap($InspectPid)
    $map | Select-Object Name, Kind, Total, Exec, ReadOnly, Writable, Shareable, SharedNow, Region | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
    foreach ($k in 'image', 'mapped', 'heap', 'stack', 'other') { Write-Host ("  {0,-7} {1,6:N0} KB" -f $k, [double](($map | Where-Object Kind -EQ $k | Measure-Object Total -Sum).Sum)) }
    return
}

# ---- results ----

$script:metrics = [ordered]@{}
function Note([string]$key, $value) { $script:metrics[$key] = $value }

function Stat([double[]]$xs) {
    $s = $xs | Sort-Object
    [pscustomobject]@{
        Min    = [math]::Round($s[0], 2)
        Median = [math]::Round($s[[int]($s.Count / 2)], 2)
        P95    = [math]::Round($s[[int][math]::Min($s.Count - 1, [math]::Floor($s.Count * 0.95))], 2)
        Max    = [math]::Round($s[-1], 2)
        Mean   = [math]::Round(($s | Measure-Object -Average).Average, 2)
    }
}

function Section([string]$t) { Write-Host ''; Write-Host "== $t ==" -ForegroundColor Cyan }

function Wants([string]$s) { ($Only -contains 'all' -and $s -ne 'soak') -or ($Only -contains $s) }

# ---- an isolated pet ----

if ($PinCpus -gt 0) { [Diagnostics.Process]::GetCurrentProcess().ProcessorAffinity = [IntPtr]((1L -shl $PinCpus) - 1) }

$tmpRoot = Join-Path ([IO.Path]::GetTempPath()) ("clawd-perf-" + [guid]::NewGuid().ToString('n').Substring(0, 8))
New-Item -ItemType Directory $tmpRoot | Out-Null
$cwd = Split-Path -Parent $Exe

$saved = @{ APPDATA = $env:APPDATA; TEMP = $env:TEMP; TMP = $env:TMP; CLAWD_PET_PORT = $env:CLAWD_PET_PORT; CLAWD_PET_HIDDEN = $env:CLAWD_PET_HIDDEN }

function Use-Sandbox([hashtable]$config = @{}) {
    $dir = Join-Path $tmpRoot ([guid]::NewGuid().ToString('n').Substring(0, 8))
    New-Item -ItemType Directory (Join-Path $dir 'clawd-pet') -Force | Out-Null
    $cfg = @{ launchWithClaude = $false } + $config
    ($cfg | ConvertTo-Json -Depth 5) | Set-Content -Encoding Ascii (Join-Path $dir 'clawd-pet\config.json')
    $env:APPDATA = $dir; $env:TEMP = $dir; $env:TMP = $dir
    $env:CLAWD_PET_PORT = "$Port"; $env:CLAWD_PET_HIDDEN = '1'
    $dir
}

function Restore-Env { foreach ($k in $saved.Keys) { Set-Item "env:$k" $saved[$k] } }

function Stop-Pet([int]$petId) {
    if (-not (Get-Process -Id $petId -ErrorAction SilentlyContinue)) { return }
    $h = [Perf]::PetWindow($petId)
    if ($h -ne [IntPtr]::Zero) { [Perf]::Post($h, 0x0010, 0, 0) } # WM_CLOSE: removes the tray icon
    for ($i = 0; $i -lt 40 -and (Get-Process -Id $petId -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 50 }
    Stop-Process -Id $petId -Force -ErrorAction SilentlyContinue
}

function Start-Pet([string]$flags, [string]$which = $Exe) {
    $petId = [Perf]::Spawn($which, $flags, $cwd)
    for ($i = 0; $i -lt 100; $i++) {
        if ([Perf]::PetWindow($petId) -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 20
    }
    $petId
}

function Send([string]$e, [string]$tool, [hashtable]$more = @{}) {
    $m = [ordered]@{ v = 1; e = $e; s = 'perf'; ts = 0 }
    if ($tool) { $m.t = $tool }
    foreach ($k in $more.Keys) { $m[$k] = $more[$k] }
    $json = $m | ConvertTo-Json -Compress
    # With -Against, whichever pet gets the datagram first wakes first, which
    # read 16-20 % higher at 20 events/s. Take turns.
    $ports = $script:sendPorts
    $script:sendFlip = -not $script:sendFlip
    if ($script:sendFlip -and $ports.Count -gt 1) { $ports = $ports[($ports.Count - 1)..0] }
    foreach ($p in $ports) { [Perf]::Udp($p, $json) }
}

function Read-Stats([string]$dir, [switch]$AlreadyAsked) {
    if (-not $AlreadyAsked) { Send '__stats'; Start-Sleep -Milliseconds 120 }
    $log = Join-Path $dir 'clawd-pet.log'
    if (-not (Test-Path $log)) { return $null }
    $line = Get-Content $log | Where-Object { $_ -match ' stats msgs=' } | Select-Object -Last 1
    if (-not $line) { return $null }
    $o = @{ timers = @{} }
    foreach ($kv in ($line -split ' ')) {
        if ($kv -match '^(\w+)=(.*)$') {
            if ($Matches[1] -eq 'timers') {
                foreach ($t in ($Matches[2] -split ',')) { if ($t -match '(\d+):(\d+)') { $o.timers[[int]$Matches[1]] = [int]$Matches[2] } }
            } else { $o[$Matches[1]] = [int]$Matches[2] }
        }
    }
    $o
}

$script:sendPorts = @($Port)
$script:sendFlip = $false
$timerNames = @{ 1 = 'save'; 2 = 'tick'; 3 = 'art'; 4 = 'hold'; 5 = 'phys'; 6 = 'drag'; 7 = 'cursor'; 8 = 'ride'; 9 = 'fade'; 10 = 'walk'; 11 = 'pulse' }

try {
    [Perf]::StayAwake($true)
    Section 'Machine'
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    $logical = [Environment]::ProcessorCount
    # Other work skews every timing here, so give it a moment to die down.
    $busy = 100
    for ($i = 0; $i -lt 30 -and $busy -gt 6; $i++) { $busy = [Perf]::SystemBusy(2000) }
    $hz = [Perf]::TscHz()
    Write-Host ("{0} ({1} logical), system busy {2:N1} % over 2 s" -f $cpu.Name.Trim(), $logical, $busy)
    $power = (Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | Select-Object -First 1).BatteryStatus
    if ($power -eq 1) { Write-Warning 'Running on battery: expect slower, noisier timings than on mains.' }
    if ($busy -gt 8) { Write-Warning "The machine isn't quiet: $([math]::Round($busy,1)) % busy. Timings will be noisy." }
    Note 'machine.busy_pct' ([math]::Round($busy, 2))
    Note 'machine.logical' $logical
    Write-Host "Exe: $Exe"

    # ---- static ----
    if (Wants 'static') {
        Section 'Binary (budget < 1.5 MB)'
        $len = (Get-Item $Exe).Length
        Write-Host ("Size: {0:N0} bytes = {1:N1} KB" -f $len, ($len / 1024))
        Note 'exe.bytes' $len
        $b = [IO.File]::ReadAllBytes($Exe)
        $pe = [BitConverter]::ToInt32($b, 0x3C)
        $nsec = [BitConverter]::ToUInt16($b, $pe + 6)
        $optSize = [BitConverter]::ToUInt16($b, $pe + 20)
        $secBase = $pe + 24 + $optSize
        $secs = 0..($nsec - 1) | ForEach-Object {
            $o = $secBase + 40 * $_
            [pscustomobject]@{
                Name = [Text.Encoding]::ASCII.GetString($b, $o, 8).TrimEnd([char]0)
                KB   = [math]::Round([BitConverter]::ToUInt32($b, $o + 16) / 1024, 1)
                VA   = [BitConverter]::ToUInt32($b, $o + 12)
                Raw  = [BitConverter]::ToUInt32($b, $o + 20)
                VSz  = [BitConverter]::ToUInt32($b, $o + 8)
            }
        }
        $secs | Select-Object Name, KB | Format-Table -AutoSize | Out-String | Write-Host
        function Rva([uint32]$v) { foreach ($s in $secs) { if ($v -ge $s.VA -and $v -lt $s.VA + [math]::Max($s.VSz, 1)) { return [int]($v - $s.VA + $s.Raw) } } return -1 }
        function CStr([int]$o) { $e = $o; while ($b[$e] -ne 0) { $e++ }; [Text.Encoding]::ASCII.GetString($b, $o, $e - $o) }
        function ImportTable([int]$dirIndex, [int]$entry, [int]$nameAt, [int]$thunkAt) {
            $dir = $pe + 24 + 112 + 8 * $dirIndex
            $rva = [BitConverter]::ToUInt32($b, $dir)
            if ($rva -eq 0) { return @() }
            $o = Rva $rva
            $out = @()
            while ($true) {
                $n = [BitConverter]::ToUInt32($b, $o + $nameAt)
                if ($n -eq 0) { break }
                $t = Rva ([BitConverter]::ToUInt32($b, $o + $thunkAt))
                $c = 0
                while ([BitConverter]::ToUInt64($b, $t + 8 * $c) -ne 0) { $c++ }
                $out += [pscustomobject]@{ Dll = (CStr (Rva $n)); Functions = $c }
                $o += $entry
            }
            $out
        }
        $imports = ImportTable 1 20 12 16
        $delay = ImportTable 13 32 4 16
        Write-Host "Load-time imports ($($imports.Count) DLLs; every hook run pays for these):"
        $imports | Sort-Object Dll | Format-Table -AutoSize | Out-String | Write-Host
        if ($delay) { Write-Host "Delay-loaded:"; $delay | Sort-Object Dll | Format-Table -AutoSize | Out-String | Write-Host }
        Note 'exe.import_dlls' $imports.Count
    }

    # ---- startup ----
    if (Wants 'startup') {
        Section 'Startup (budget < 150 ms to visible)'
        $dir = Use-Sandbox
        $vis = @(); $idle = @()
        $supportsOffscreen = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($Exe)) -match '--offscreen'
        # The first run of a new exe waits for the antivirus; not what's being measured here.
        $r = [Perf]::TimeToUp($Exe, '--hidden --offscreen', $cwd, $supportsOffscreen)
        Stop-Pet ([int]$r[2])
        Write-Host "First run, not counted (a fresh exe gets scanned): $([math]::Round($r[1], 1)) ms to idle loop"
        for ($i = 0; $i -lt $StartupRuns; $i++) {
            $r = [Perf]::TimeToUp($Exe, '--hidden --offscreen', $cwd, $supportsOffscreen)
            if ($r[0] -ge 0) { $vis += $r[0] }
            if ($r[1] -ge 0) { $idle += $r[1] }
            Stop-Pet ([int]$r[2])
        }
        if ($vis.Count) { $v = Stat $vis; Write-Host "Start to window up:   median $($v.Median) ms, worst $($v.Max) ms ($($vis.Count) runs)"; Note 'startup.visible_median_ms' $v.Median; Note 'startup.visible_max_ms' $v.Max }
        else { Write-Host 'This exe has no --offscreen: only the time to an idle message loop.' }
        $v = Stat $idle
        Write-Host "Start to idle loop:   median $($v.Median) ms, worst $($v.Max) ms ($($idle.Count) runs)"
        Note 'startup.idle_median_ms' $v.Median
        Note 'startup.idle_max_ms' $v.Max
    }

    # ---- hook ----
    if (Wants 'hook') {
        Section 'Hook, stdin ready (budget < 15 ms)'
        $null = Use-Sandbox
        $held = [Perf]::HoldMutex("Local\ClawdPet-$Port")
        $fx = Join-Path $repo 'tests\fixtures\hooks'
        $read = [IO.File]::ReadAllBytes((Join-Path $fx 'pre_tool_use_read.json'))
        $start = [IO.File]::ReadAllBytes((Join-Path $fx 'session_start_resume.json'))
        $bash = [IO.File]::ReadAllBytes((Join-Path $fx 'pre_tool_use_bash.json'))
        $bigResponse = 'x' * 200000
        $big = [Text.Encoding]::UTF8.GetBytes('{"hook_event_name":"PostToolUse","session_id":"s","cwd":"C:\\p","tool_name":"Read","tool_input":{"file_path":"a"},"tool_response":{"type":"text","file":{"content":"' + $bigResponse + '"}}}')
        $bigInput = [Text.Encoding]::UTF8.GetBytes('{"hook_event_name":"PreToolUse","session_id":"s","cwd":"C:\\p","tool_name":"Write","tool_input":{"file_path":"a","content":"' + $bigResponse + '"}}')
        $payloads = [ordered]@{
            'PreToolUse(Read)'    = $read
            'PreToolUse(Bash)'    = $bash
            'SessionStart'        = $start
            'PostToolUse 200 KB'  = $big
            'PreToolUse(Write) 200 KB' = $bigInput
        }
        $sink = New-Object Perf+Sink $Port
        $rows = @()
        foreach ($name in $payloads.Keys) {
            $null = [Perf]::RunHook($Exe, '--hook', $payloads[$name], $cwd) # warm the file cache
            $null = $sink.Drain()
            $ms = New-Object double[] $HookRuns
            $cpu = New-Object double[] $HookRuns
            $faults = New-Object double[] $HookRuns
            $bad = 0
            for ($i = 0; $i -lt $HookRuns; $i++) {
                $r = [Perf]::RunHook($Exe, '--hook', $payloads[$name], $cwd)
                $ms[$i] = $r.Ms; $cpu[$i] = $r.CpuMs; $faults[$i] = $r.PageFaults
                if ($r.Exit -ne 0) { $bad++ }
            }
            Start-Sleep -Milliseconds 50
            $got = $sink.Drain()
            $s = Stat $ms
            $c = Stat $cpu
            $f = Stat $faults
            $rows += [pscustomobject]@{ Payload = $name; 'Median ms' = $s.Median; 'p95' = $s.P95; 'Worst' = $s.Max; 'Min' = $s.Min; 'CPU ms' = $c.Median; 'Page faults' = $f.Median; 'Datagrams' = "$got/$HookRuns"; 'Non-zero exits' = $bad }
            $key = ($name -replace '[^A-Za-z0-9]', '_').Trim('_').ToLower()
            Note "hook.$key.median_ms" $s.Median
            Note "hook.$key.p95_ms" $s.P95
            Note "hook.$key.max_ms" $s.Max
            Note "hook.$key.faults" $f.Median
        }
        $sink.Dispose()
        [Perf]::Close($held)
        $rows | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
        try {
            $mods = [Perf]::ModulesWhileWaiting($Exe, '--hook', $cwd)
            Write-Host "$($mods.Count) modules loaded while waiting on stdin: $($mods -join ', ')"
            Note 'hook.modules_loaded' $mods.Count
        } catch { Write-Host 'Modules: the process had already exited.' }
    }

    # ---- memory ----
    if (Wants 'memory') {
        Section 'Memory (budget < 10 MB working set)'
        $dir = Use-Sandbox
        $pet = Start-Pet '--hidden --offscreen --debug-log'
        Start-Sleep -Seconds 3
        $s0 = [Perf]::Snapshot($pet)
        Write-Host ("Fresh, 3 s after start:    WS {0:N1} MB, private WS {1:N1} MB, commit {2:N1} MB, peak WS {3:N1} MB, faults {4} (hard {5}), {6} threads" -f ($s0.WS / 1MB), ($s0.PrivateWS / 1MB), ($s0.Commit / 1MB), ($s0.PeakWS / 1MB), $s0.PageFaults, $s0.HardFaults, $s0.Threads)
        Note 'memory.ws_fresh_mb' ([math]::Round($s0.WS / 1MB, 2))
        Note 'memory.private_ws_fresh_mb' ([math]::Round($s0.PrivateWS / 1MB, 2))
        Note 'memory.commit_fresh_mb' ([math]::Round($s0.Commit / 1MB, 2))
        Note 'memory.peak_ws_mb' ([math]::Round($s0.PeakWS / 1MB, 2))
        Note 'memory.faults_fresh' $s0.PageFaults
        # A little of everything the pet does, so the numbers aren't only startup.
        foreach ($t in 'Bash', 'Read', 'Edit') { Send 'PreToolUse' $t; Start-Sleep -Milliseconds 800; Send 'PostToolUse' $t; Start-Sleep -Milliseconds 300 }
        Send 'PermissionRequest' 'Bash'; Start-Sleep -Milliseconds 800; Send 'Stop'; Start-Sleep -Seconds 2
        $s1 = [Perf]::Snapshot($pet)
        Write-Host ("After some activity:       WS {0:N1} MB, private WS {1:N1} MB, commit {2:N1} MB, faults {3}" -f ($s1.WS / 1MB), ($s1.PrivateWS / 1MB), ($s1.Commit / 1MB), $s1.PageFaults)
        Note 'memory.ws_active_mb' ([math]::Round($s1.WS / 1MB, 2))
        Note 'memory.private_ws_active_mb' ([math]::Round($s1.PrivateWS / 1MB, 2))
        $map = [Perf]::WorkingSetMap($pet)
        $total = ($map | Measure-Object Total -Sum).Sum
        Write-Host "Working set by owner ($([math]::Round($total/1024,1)) MB; KB. Shareable = could be shared with other processes, SharedNow = another process has it too):"
        $map | Select-Object Name, Kind, Total, Exec, ReadOnly, Writable, Shareable, SharedNow, Region | Format-Table -AutoSize | Out-String -Width 200 | Write-Host
        foreach ($k in 'image', 'mapped', 'heap', 'stack', 'other') {
            $sum = ($map | Where-Object Kind -EQ $k | Measure-Object Total -Sum).Sum
            Write-Host ("  {0,-7} {1,6:N0} KB" -f $k, [double]$sum)
            Note "memory.kind.$k.kb" ([int]$sum)
        }
        $exeRow = $map | Where-Object Name -EQ 'clawd-pet.exe'
        Note 'memory.exe_pages_kb' ([int]$exeRow.Total)
        $trimmed = [Perf]::Trim($pet)
        Start-Sleep -Seconds 1
        $s2 = [Perf]::Snapshot($pet)
        Send 'PreToolUse' 'Bash'; Start-Sleep -Seconds 3; Send 'Stop'; Start-Sleep -Seconds 3
        $s3 = [Perf]::Snapshot($pet)
        Write-Host ("Trimmed from outside ({0}): WS {1:N1} MB right after, {2:N1} MB after 6 s of use, +{3} faults (what it really keeps touching)" -f $trimmed, ($s2.WS / 1MB), ($s3.WS / 1MB), ($s3.PageFaults - $s2.PageFaults))
        Note 'memory.ws_hot_mb' ([math]::Round($s3.WS / 1MB, 2))
        Write-Host ("GDI {0} (peak {1}), USER {2} (peak {3}), handles {4}, threads {5}" -f $s3.Gdi, $s3.GdiPeak, $s3.User, $s3.UserPeak, $s3.Handles, $s3.Threads)
        Note 'handles.gdi' $s3.Gdi
        Note 'handles.user' $s3.User
        Note 'handles.kernel' $s3.Handles
        Stop-Pet $pet
    }

    # ---- states ----
    if (Wants 'states') {
        Section 'CPU and wake-ups by state'
        # Fast clock for the sleepy state: a pretend 3 am (40 % delays), with the shortest allowed sleep.
        $asleepWait = 150
        $all = @(
            @{ N = 'asleep'; Flags = '--hidden --offscreen --debug-log'; Cfg = @{ sleepAfterMinutes = 1 }; Setup = { Send '__clock' '3,2,6'; Start-Sleep -Seconds $asleepWait } },
            @{ N = 'hidden'; Flags = '--hidden --debug-log'; Setup = { Start-Sleep -Seconds 2 } },
            @{ N = 'hidden-working'; Flags = '--hidden --debug-log'; Setup = { Send 'PreToolUse' 'Bash'; Start-Sleep -Seconds 2 }; Every = 20; Keep = { Send 'PreToolUse' 'Bash' } },
            @{ N = 'fullscreen'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send '__fullscreen' '1'; Start-Sleep -Seconds 2 } },
            @{ N = 'fullscreen-working'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send '__fullscreen' '1'; Send 'PreToolUse' 'Bash'; Start-Sleep -Seconds 2 }; Every = 20; Keep = { Send 'PreToolUse' 'Bash' } },
            @{ N = 'idle'; Flags = '--hidden --offscreen --debug-log'; Setup = { Start-Sleep -Seconds 2 } },
            @{ N = 'idle-cursor-far'; Flags = '--hidden --offscreen --debug-log'; Setup = { Start-Sleep -Seconds 1; foreach ($x in @($pet, $petB)) { if ($x) { [Perf]::Post([Perf]::PetWindow($x), 0x0200, 0, 0) } }; Start-Sleep -Seconds 1 } },
            @{ N = 'idle-no-eyes'; Flags = '--hidden --offscreen --debug-log'; Cfg = @{ behaviours = @{ eyesFollowCursor = $false } }; Setup = { Start-Sleep -Seconds 2 } },
            @{ N = 'thinking'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send 'UserPromptSubmit'; Start-Sleep -Seconds 2 }; Every = 30; Keep = { Send 'UserPromptSubmit' } },
            @{ N = 'bash-8fps'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send 'PreToolUse' 'Bash'; Start-Sleep -Seconds 2 }; Every = 30; Keep = { Send 'PreToolUse' 'Bash' } },
            @{ N = 'typing-10fps'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send 'PreToolUse' 'Edit'; Start-Sleep -Seconds 2 }; Every = 30; Keep = { Send 'PreToolUse' 'Edit' } },
            @{ N = 'attention'; Flags = '--hidden --offscreen --debug-log'; Cfg = @{ permissionAnsweredAfterSeconds = 0; impatientAfterSeconds = 0 }; Setup = { Send 'PermissionRequest' 'Bash'; Start-Sleep -Seconds 2 } },
            @{ N = 'subagents'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send 'PreToolUse' 'Agent'; Start-Sleep -Seconds 2; 1..3 | ForEach-Object { Send 'PreToolUse' 'Read' @{ a = 'general-purpose'; g = "agent$_" } } }; Every = 30; Keep = { 1..3 | ForEach-Object { Send 'PreToolUse' 'Read' @{ a = 'general-purpose'; g = "agent$_" } } } },
            @{ N = 'walking'; Flags = '--hidden --offscreen --debug-log'; Cfg = @{ behaviours = @{ walk = $true } }; Setup = { Send '__walk'; Start-Sleep -Seconds 1 }; Every = 1; Keep = { Send '__walk' } },
            @{ N = 'flying'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send '__drop' '900,0'; Start-Sleep -Milliseconds 300 }; Every = 3; Keep = { Send '__drop' '900,0' } },
            @{ N = 'dragging-125hz'; Flags = '--hidden --offscreen --debug-log'; Drag = 125 },
            @{ N = 'dragging-1000hz'; Flags = '--hidden --offscreen --debug-log'; Drag = 1000 },
            @{ N = 'events-20-per-s'; Flags = '--hidden --offscreen --debug-log'; Setup = { Send 'PreToolUse' 'Read'; Start-Sleep -Seconds 2 }; Events = 20 }
        )
        $chosen = if ($State) { $all | Where-Object { $State -contains $_.N } } else { $all }
        $rows = @()
        $abRows = @()
        foreach ($st in $chosen) {
            $cfgB = if ($st.Cfg) { $st.Cfg } else { @{} }
            $petB = $null
            if ($Against) {
                $dirB = Use-Sandbox $cfgB
                $env:CLAWD_PET_PORT = "$($Port + 1)"
                $petB = Start-Pet $st.Flags $Against
                $script:sendPorts = @($Port, ($Port + 1))
            }
            $dir = Use-Sandbox $cfgB
            $pet = Start-Pet $st.Flags
            Start-Sleep -Milliseconds 500
            if ($st.Setup) { & $st.Setup }
            $h = [Perf]::PetWindow($pet)
            $hs = @($h)
            if ($petB) { $hs += [Perf]::PetWindow($petB) }
            $stats0 = Read-Stats $dir
            $aB = if ($petB) { [Perf]::Snapshot($petB) }
            # Explorer draws the tray, so a changed tooltip costs CPU over there too.
            $explorerId = (Get-Process explorer -ErrorAction SilentlyContinue | Sort-Object WorkingSet64 -Descending | Select-Object -First 1).Id
            $ea = if ($explorerId) { [Perf]::Snapshot($explorerId) }
            $a = [Perf]::Snapshot($pet)
            if ($st.Drag) {
                $null = [Perf]::Drag([IntPtr[]]$hs, $st.Drag, [double]$Seconds, 800, 400)
            } elseif ($st.Events) {
                $sw = [Diagnostics.Stopwatch]::StartNew(); $i = 0
                while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
                    if ($sw.Elapsed.TotalSeconds * $st.Events -ge $i) {
                        $ev = if ($i % 2) { 'PostToolUse' } else { 'PreToolUse' }
                        Send $ev 'Read'
                        $i++
                    } else { Start-Sleep -Milliseconds 2 }
                }
            } else {
                $sw = [Diagnostics.Stopwatch]::StartNew()
                $next = if ($st.Every) { $st.Every } else { [double]::MaxValue }
                while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
                    $left = [math]::Min($next, $Seconds) - $sw.Elapsed.TotalSeconds
                    if ($left -gt 0) { Start-Sleep -Milliseconds ([int][math]::Ceiling($left * 1000)) }
                    if ($st.Keep -and $sw.Elapsed.TotalSeconds -ge $next -and $sw.Elapsed.TotalSeconds -lt $Seconds) { & $st.Keep; $next += $st.Every }
                }
            }
            $b = [Perf]::Snapshot($pet)
            $bB = if ($petB) { [Perf]::Snapshot($petB) }
            $eb = if ($explorerId) { [Perf]::Snapshot($explorerId) }
            $stats = Read-Stats $dir
            if ($st.Drag) { foreach ($x in $hs) { [Perf]::Release($x, 800, 400) }; Start-Sleep -Milliseconds 100 }
            Stop-Pet $pet
            if ($petB) {
                $statsB = Read-Stats $dirB -AlreadyAsked
                Stop-Pet $petB
                $script:sendPorts = @($Port)
                $env:CLAWD_PET_PORT = "$Port"
                $dtB = $bB.T - $aB.T
                $coreA = ($b.Cycles - $a.Cycles) / ($hz * ($b.T - $a.T)) * 100
                $coreB = ($bB.Cycles - $aB.Cycles) / ($hz * $dtB) * 100
                $abRows += [pscustomobject][ordered]@{
                    State = $st.N
                    'A %core' = [math]::Round($coreA, 3)
                    'B %core' = [math]::Round($coreB, 3)
                    'A-B'     = [math]::Round($coreA - $coreB, 3)
                    'A-B %'   = if ($coreB -gt 0) { '{0:+0.0;-0.0}' -f (100 * ($coreA - $coreB) / $coreB) } else { '' }
                    'A CtxSw/s' = [math]::Round([Perf]::Switches($a, $b) / ($b.T - $a.T), 1)
                    'B CtxSw/s' = [math]::Round([Perf]::Switches($aB, $bB) / $dtB, 1)
                    'A Compose/s' = if ($stats) { [math]::Round($stats.composed / ($b.T - $a.T), 1) }
                    'B Compose/s' = if ($statsB) { [math]::Round($statsB.composed / $dtB, 1) }
                }
                Note ("ab." + ($st.N -replace '[^a-z0-9]', '_') + ".a_pct_core") ([math]::Round($coreA, 3))
                Note ("ab." + ($st.N -replace '[^a-z0-9]', '_') + ".b_pct_core") ([math]::Round($coreB, 3))
            }

            $dt = $b.T - $a.T
            $cores = ($b.Cycles - $a.Cycles) / ($hz * $dt) * 100
            $sw = [Perf]::Switches($a, $b) / $dt
            $r = [ordered]@{
                State      = $st.N
                'CPU %core' = [math]::Round($cores, 3)
                'CPU %all'  = [math]::Round($cores / $logical, 4)
                'CtxSw/s'   = [math]::Round($sw, 1)
            }
            if ($ea -and $eb) { $r['Explorer %core'] = [math]::Round(($eb.Cycles - $ea.Cycles) / ($hz * ($eb.T - $ea.T)) * 100, 3) }
            if ($stats) {
                $tt = ($stats.timers.GetEnumerator() | Sort-Object Name | ForEach-Object { "{0}:{1:N1}" -f $timerNames[[int]$_.Name], ($_.Value / $dt) }) -join ' '
                $r['Timers/s'] = if ($tt) { $tt } else { '-' }
                $r['Msgs/s'] = [math]::Round($stats.msgs / $dt, 1)
                $r['Compose/s'] = [math]::Round($stats.composed / $dt, 1)
                $r['Draw/s'] = [math]::Round($stats.drawn / $dt, 1)
                $r['Move/s'] = [math]::Round($stats.moved / $dt, 1)
                $r['WinEvt/s'] = [math]::Round($stats.win_events / $dt, 1)
            }
            $r['In front'] = [Perf]::ForegroundName()
            $rows += [pscustomobject]$r
            $k = $st.N -replace '[^a-z0-9]', '_'
            Note "state.$k.cpu_pct_core" $r['CPU %core']
            Note "state.$k.cpu_pct_all" $r['CPU %all']
            Note "state.$k.ctxsw_s" $r['CtxSw/s']
            if ($null -ne $r['Explorer %core']) { Note "state.$k.explorer_pct_core" $r['Explorer %core'] }
            if ($stats) { Note "state.$k.draw_s" $r['Draw/s']; Note "state.$k.compose_s" $r['Compose/s']; Note "state.$k.msgs_s" $r['Msgs/s']; Note "state.$k.winevt_s" $r['WinEvt/s'] }
        }
        # -Property *: without it Format-Table silently drops columns after the tenth.
        $rows | Format-Table -Property * -AutoSize | Out-String -Width 300 | Write-Host
        if ($Against) {
            Write-Host "Side by side, same seconds. A = $Exe"
            Write-Host "                            B = $Against"
            $abRows | Format-Table -Property * -AutoSize | Out-String -Width 200 | Write-Host
        }
    }

    # ---- extras ----
    if (Wants 'extras') {
        Section 'Tooltip and chime, once used'
        $dir = Use-Sandbox
        $pet = Start-Pet '--hidden --offscreen --debug-log'
        Start-Sleep -Seconds 3
        $a = [Perf]::Snapshot($pet)
        Send '__tip'
        Start-Sleep -Seconds 2
        $b = [Perf]::Snapshot($pet)
        $mods = (Get-Process -Id $pet).Modules | ForEach-Object ModuleName
        Stop-Pet $pet
        Write-Host ("Before: WS {0:N1} MB, private {1:N1} MB, GDI {2}, USER {3}, handles {4}" -f ($a.WS / 1MB), ($a.PrivateWS / 1MB), $a.Gdi, $a.User, $a.Handles)
        Write-Host ("After:  WS {0:N1} MB, private {1:N1} MB, GDI {2}, USER {3}, handles {4}" -f ($b.WS / 1MB), ($b.PrivateWS / 1MB), $b.Gdi, $b.User, $b.Handles)
        Write-Host ("Cost:   WS +{0:N0} KB, private +{1:N0} KB, GDI +{2}, USER +{3}, handles +{4}" -f (($b.WS - $a.WS) / 1KB), (($b.PrivateWS - $a.PrivateWS) / 1KB), ($b.Gdi - $a.Gdi), ($b.User - $a.User), ($b.Handles - $a.Handles))
        Write-Host "Loaded: comctl32 $($mods -contains 'COMCTL32.dll'), winmm $($mods -contains 'WINMM.dll')"
        Note 'extras.tip_ws_kb' ([int](($b.WS - $a.WS) / 1KB))
        Note 'extras.tip_private_kb' ([int](($b.PrivateWS - $a.PrivateWS) / 1KB))
        Note 'extras.tip_gdi' ($b.Gdi - $a.Gdi)
        Note 'extras.tip_user' ($b.User - $a.User)
    }

    # ---- soak ----
    if ($Only -contains 'soak') {
        Section "Soak: $SoakMinutes min of constant activity"
        $dir = Use-Sandbox @{ behaviours = @{ walk = $true } }
        $pet = Start-Pet '--hidden --offscreen --debug-log'
        Start-Sleep -Seconds 3
        $h = [Perf]::PetWindow($pet)
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $samples = New-Object System.Collections.ArrayList
        $i = 0
        $nextSample = 0
        while ($sw.Elapsed.TotalMinutes -lt $SoakMinutes) {
            $sid = "soak-$($i % 40)"
            $more = @{ pp = $PID }
            $tool = @('Read', 'Bash', 'Edit', 'WebFetch', 'Agent', 'mcp__x__y')[$i % 6]
            Send 'SessionStart' '' (@{ n = 'startup'; s = $sid } + $more)
            Send 'UserPromptSubmit' '' (@{ s = $sid } + $more)
            Send 'PreToolUse' $tool @{ s = $sid }
            Send 'PreToolUse' 'Read' @{ s = $sid; a = 'general-purpose'; g = "ag$($i % 7)" }
            Send 'PermissionRequest' $tool @{ s = $sid }
            Send 'PostToolUse' $tool @{ s = $sid }
            Send 'SubagentStop' '' @{ s = $sid; a = 'general-purpose'; g = "ag$($i % 7)" }
            Send 'Stop' '' @{ s = $sid }
            if ($i % 3 -eq 0) { Send '__walk' }
            if ($i % 5 -eq 0) { Send '__drop' "$(200 + $i % 900),100" }
            if ($i % 7 -eq 0) { Send '__goto' }
            if ($i % 11 -eq 0) { Send '__tip' }
            if ($i % 13 -eq 0) { [Perf]::Post($h, 0x02E0, (96 + 24 * ($i % 3)), 0) } # WM_DPICHANGED: rebuilds the surface
            if ($i % 17 -eq 0) { $null = Send 'SessionEnd' '' @{ s = $sid; n = 'other' } }
            if ($i % 19 -eq 0) { Send '__fullscreen' '1'; Start-Sleep -Milliseconds 200; Send '__fullscreen' '0' }
            if ($i % 23 -eq 0) { [Perf]::Drag([IntPtr[]]@($h), 125, 0.5, 800, 400) | Out-Null; [Perf]::Release($h, 800, 400) }
            $i++
            Start-Sleep -Milliseconds 120
            if ($sw.Elapsed.TotalSeconds -ge $nextSample) {
                $s = [Perf]::Snapshot($pet)
                $null = $samples.Add([pscustomobject]@{ Min = [math]::Round($sw.Elapsed.TotalMinutes, 2); WS = $s.WS; Commit = $s.Commit; PrivateWS = $s.PrivateWS; Handles = $s.Handles; Gdi = $s.Gdi; User = $s.User; Threads = $s.Threads })
                $nextSample += 30
            }
        }
        $last = [Perf]::Snapshot($pet)
        Stop-Pet $pet
        $warm = $samples | Where-Object { $_.Min -ge 2 }
        $f = $warm[0]; $l = $warm[-1]
        Write-Host "$i rounds of events. From minute $($f.Min) (warm) to the end:"
        Write-Host ("  Commit  {0:N0} → {1:N0} KB ({2:N1} KB/hour)" -f ($f.Commit / 1KB), ($l.Commit / 1KB), (($l.Commit - $f.Commit) / 1KB / (($l.Min - $f.Min) / 60)))
        Write-Host ("  WS      {0:N0} → {1:N0} KB" -f ($f.WS / 1KB), ($l.WS / 1KB))
        Write-Host ("  GDI     {0} → {1}, USER {2} → {3}, handles {4} → {5}" -f $f.Gdi, $l.Gdi, $f.User, $l.User, $f.Handles, $l.Handles)
        Note 'soak.commit_kb_per_hour' ([math]::Round(($l.Commit - $f.Commit) / 1KB / (($l.Min - $f.Min) / 60), 1))
        Note 'soak.gdi_delta' ($l.Gdi - $f.Gdi)
        Note 'soak.user_delta' ($l.User - $f.User)
        Note 'soak.handles_delta' ($l.Handles - $f.Handles)
        $samples | Where-Object { [int]($_.Min * 2) % 10 -eq 0 } | Select-Object Min, @{ N = 'Commit KB'; E = { [int]($_.Commit / 1KB) } }, @{ N = 'WS KB'; E = { [int]($_.WS / 1KB) } }, @{ N = 'Private WS KB'; E = { [int]($_.PrivateWS / 1KB) } }, Gdi, User, Handles, Threads | Format-Table -AutoSize | Out-String | Write-Host
        if ($Json) { $samples | Export-Csv -NoTypeInformation ($Json + '.soak.csv') }
    }

    # ---- budgets ----
    Section 'Against the budgets (PLAN.md §1)'
    function Check($what, $budget, $value, $limit, $ofOneCore) {
        if ($null -ne $value) {
            $ok = [double]$value -lt $limit
            [pscustomobject]@{ Metric = $what; Budget = $budget; Measured = $value; 'Of one core' = $ofOneCore; Result = if ($ok) { 'ok' } else { 'OVER' } }
        }
    }
    $m = $script:metrics
    $hookWorst = @($m.Keys | Where-Object { $_ -match '^hook\.pretooluse_read\.median_ms$' } | ForEach-Object { $m[$_] })[0]
    @(
        Check 'Binary (KB)' '< 1536' $(if ($m['exe.bytes']) { [math]::Round($m['exe.bytes'] / 1KB, 1) }) 1536
        Check 'Working set, active (MB)' '< 10' $m['memory.ws_active_mb'] 10
        # CPU budgets are % of the whole machine (PLAN §1). "~0" stays as strict as 0.02 % of one core on 20 CPUs.
        Check 'CPU asleep (% of the machine)' '~0' $m['state.asleep.cpu_pct_all'] 0.001 $m['state.asleep.cpu_pct_core']
        Check 'CPU hidden (% of the machine)' '~0' $m['state.hidden.cpu_pct_all'] 0.001 $m['state.hidden.cpu_pct_core']
        Check 'CPU idle (% of the machine)' '< 0.1' $m['state.idle.cpu_pct_all'] 0.1 $m['state.idle.cpu_pct_core']
        Check 'CPU animating 8 fps (% of the machine)' '< 0.5' $m['state.bash_8fps.cpu_pct_all'] 0.5 $m['state.bash_8fps.cpu_pct_core']
        Check 'CPU dragging 125 Hz (% of the machine)' '< 2' $m['state.dragging_125hz.cpu_pct_all'] 2 $m['state.dragging_125hz.cpu_pct_core']
        Check 'Hook median (ms)' '< 15' $hookWorst 15
        Check 'Startup to visible (ms)' '< 150' $m['startup.visible_median_ms'] 150
    ) | Where-Object { $_ } | Format-Table -AutoSize | Out-String | Write-Host

    if ($Compare -and (Test-Path $Compare)) {
        Section "Compared with $Compare"
        $old = Get-Content $Compare -Raw | ConvertFrom-Json
        $m.Keys | ForEach-Object {
            $o = $old.$_
            if ($null -ne $o -and "$($m[$_])" -ne "$o") {
                [pscustomobject]@{ Metric = $_; Before = $o; After = $m[$_]; Change = $(if ([double]$o -ne 0) { '{0:+0.0;-0.0}%' -f (100.0 * ($m[$_] - $o) / $o) } else { '' }) }
            }
        } | Format-Table -AutoSize | Out-String -Width 160 | Write-Host
    }

    if ($Json) { $m | ConvertTo-Json -Depth 3 | Set-Content -Encoding UTF8 $Json; Write-Host "Saved to $Json" }
}
finally {
    [Perf]::StayAwake($false)
    Restore-Env
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $tmpRoot -ErrorAction SilentlyContinue
}
