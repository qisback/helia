# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Shared pieces of the benchmark harnesses (the same file is kept in Helia,
Valerian and Annora): timing a child process with its peak memory, the load the
rest of the machine put on the CPU while it ran, and video memory."""
import glob
import os
import platform
import re
import shutil
import subprocess
import threading
import time

LVP = next(iter(sorted(glob.glob("/usr/share/vulkan/icd.d/lvp_icd*.json"))), "/usr/share/vulkan/icd.d/lvp_icd.json")
BUSY_LIMIT = float(os.environ.get("BENCH_BUSY_LIMIT", "0.5"))  # cores of foreign load a run may share
MODE_NAME = {"cpu": "CPU", "gpu": "GPU", "sw": "software GPU adapter"}


def cpu_times():
    """(busy, total) jiffies of the whole machine."""
    with open("/proc/stat") as f:
        v = [int(x) for x in f.readline().split()[1:]]
    return sum(v) - v[3] - v[4], sum(v)


def machine():
    info = {"os": platform.platform(), "arch": platform.machine(), "cores": os.cpu_count()}
    try:
        txt = open("/proc/cpuinfo").read()
        m = re.search(r"model name\s*:\s*(.+)", txt) or re.search(r"Model\s*:\s*(.+)", txt)
        info["cpu"] = m.group(1).strip() if m else ""
        if not info["cpu"]:
            lsc = subprocess.run(["lscpu"], capture_output=True, text=True).stdout
            m = re.search(r"Model name:\s*(.+)", lsc)
            info["cpu"] = m.group(1).strip() if m else platform.processor()
        m = re.search(r"MemTotal:\s+(\d+) kB", open("/proc/meminfo").read())
        info["ram_gb"] = round(int(m.group(1)) / 1048576, 1)
    except OSError:
        pass
    if shutil.which("lscpu"):
        m = re.search(r"Thread\(s\) per core:\s*(\d+)", subprocess.run(["lscpu"], capture_output=True, text=True).stdout)
        if m:
            info["threads_per_core"] = int(m.group(1))
            info["cpu"] = f"{info.get('cpu', '')} ({int(info['cores']) // int(m.group(1))} cores, {m.group(1)} threads each)"
    if shutil.which("nvidia-smi"):
        out = subprocess.run(["nvidia-smi", "--query-gpu=index,name,driver_version", "--format=csv,noheader"],
                             capture_output=True, text=True).stdout
        info["gpus"] = [l.strip() for l in out.splitlines()]
    if os.path.exists(LVP):
        info["software_adapter"] = "Mesa lavapipe"
    return info


def load1():
    return float(open("/proc/loadavg").read().split()[0])


def run_timed(cmd, env=None, threads=0, gpu_index=None, vram=False):
    """Run cmd; return wall, peak RSS, own CPU seconds, foreign load, VRAM peak and
    the output as a list of (seconds since start, line)."""
    env = dict(os.environ if env is None else env)
    if threads:
        env["RAYON_NUM_THREADS"] = str(threads)
        cmd = ["taskset", "-c", f"0-{threads - 1}"] + cmd
    box, base, stop, th = [0], [0], threading.Event(), None
    if vram and gpu_index is not None and shutil.which("nvidia-smi"):
        q = ["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits", "-i", str(gpu_index)]
        base[0] = int(subprocess.run(q, capture_output=True, text=True).stdout.strip() or 0)

        def sample():
            p = subprocess.Popen(q + ["-lms", "50"], stdout=subprocess.PIPE, text=True)
            for line in p.stdout:
                box[0] = max(box[0], int(line.strip()))
                if stop.is_set():
                    break
            p.kill()
        th = threading.Thread(target=sample)
        th.start()
        time.sleep(0.3)
    b0, t0 = cpu_times()
    start = time.perf_counter()
    p = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    lines = [(time.perf_counter() - start, l.rstrip("\n")) for l in p.stdout]
    _, status, ru = os.wait4(p.pid, 0)
    wall = time.perf_counter() - start
    b1, t1 = cpu_times()
    if th:
        stop.set()
        th.join()
    hz = os.sysconf("SC_CLK_TCK")
    own = ru.ru_utime + ru.ru_stime
    other = max(0.0, (b1 - b0) / hz - own) / wall
    return {"ok": status == 0, "wall": round(wall, 3), "rss_mb": ru.ru_maxrss // 1024, "cpu_s": round(own, 2),
            "other_cores": round(other, 2), "loaded": other > BUSY_LIMIT, "busy_limit": BUSY_LIMIT,
            "vram_mb": (box[0] - base[0]) if th else None, "lines": lines}


def env_for(mode, gpu_index):
    env = dict(os.environ)
    env.pop("HELIA_GPU_DEVICE", None)
    if mode == "sw":
        env["VK_ICD_FILENAMES"] = LVP
    elif mode == "gpu" and gpu_index is not None:
        env["HELIA_GPU_DEVICE"] = str(gpu_index)
    return env


def gate(mode, force, patience=10800):
    """A GPU run on a shared host waits until the one-minute load average is under 8."""
    start = time.time()
    while True:
        l = load1()
        if mode != "gpu" or force or l < 8:
            return l
        if time.time() - start > patience:
            raise SystemExit(f"giving up on a GPU run: load average stayed at {l} (8 or more) for {patience} s")
        time.sleep(30)


def foreign_cores(seconds=3.0):
    """Cores' worth of CPU the machine is spending right now."""
    b0, t0 = cpu_times()
    time.sleep(seconds)
    b1, t1 = cpu_times()
    hz = os.sysconf("SC_CLK_TCK")
    return (b1 - b0) / hz / seconds


def ci_jobs():
    """Number of CI job containers running on this host (0 when docker is absent)."""
    if not shutil.which("docker"):
        return 0
    out = subprocess.run(["docker", "ps", "--format", "{{.Names}}"], capture_output=True, text=True).stdout
    return sum(1 for n in out.split() if n.startswith("FORGEJO-ACTIONS"))


def wait_quiet(limit=None, patience=1200):
    """Wait until the machine has under `limit` cores busy; give up after `patience` s."""
    if os.environ.get("BENCH_NO_WAIT"):
        return False
    limit = BUSY_LIMIT * 0.6 if limit is None else limit
    start = time.time()
    while time.time() - start < patience:
        if ci_jobs() == 0 and foreign_cores() < limit:
            return True
    return False


def collect(fn, reps, tries=None):
    """Call fn() until `reps` runs succeeded on a quiet machine, or `tries` attempts were made.
    Loaded runs are kept in the result (marked) and left out of the plots."""
    out, good, n = [], 0, 0
    tries = tries or reps * 3
    while good < reps and n < tries:
        wait_quiet()
        r = fn()
        n += 1
        out.append(r)
        good += 1 if r["ok"] and not r["loaded"] else 0
    return out
