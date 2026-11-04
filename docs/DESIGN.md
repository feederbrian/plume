# How Plume stays light

Plume's brief was a process manager with live 60 fps graphs that uses almost
no CPU or memory. This document explains the decisions behind that, and what
was measured.

## A fresh codebase, not a fork

btop++ and htop were the obvious starting points. Both are terminal programs,
and their data collection is tangled with their drawing code: btop's Linux
collector is one 3,600-line file wired into its config and update loop in over
50 places. A fork would mean carrying a terminal UI we'd delete and rebasing
against upstream forever. Running a terminal program and reading its screen
from a GUI is slower and more fragile than reading `/proc` and `/sys` directly.

The data side turned out to be a few thousand lines of careful parsing. Plume
borrows ideas, not code:

* **btop++** (Apache-2.0): loading NVML with `dlopen`, the AMD sysfs layout,
  the overall shape of the collectors.
* **htop** (GPL-2.0+): opening each process's files relative to one handle on
  `/proc`, and only reading what the visible columns need.

GPL-3.0-or-later keeps code from btop, htop, nvtop and Resources legally
reusable later (for example nvtop's per-process GPU code).

## Layout

```
crates/plume-core   data collection, no GTK dependency
  cpu, mem, net, disk     /proc readers: files kept open, re-read with pread, no allocation per sample
  proc                    process scan: raw getdents64, files opened relative to /proc
  netproc                 per-process TCP traffic via NETLINK_SOCK_DIAG + /proc/<pid>/fd
  gpu/{intel,nvidia,amd}  GPU support, gated on the device's runtime power state
  actions                 signals through a pidfd with a PID-reuse check; renice of every thread
  history                 two-tier ring buffers: 10 Hz for ~5 min, 1 s means for 1 h
  sampler                 two threads (fast 10 Hz, slow 4 Hz), driven by what's on screen
crates/plume        GTK 4 / libadwaita app
  widgets/graph           the graph widget (below)
  pages/processes         CPU / Memory / Disk / Network tabs
  pages/summary           bottom summary panels
  pages/resources         Overview dashboard: GPUs, per-core grid, filesystems
  floats, inspector, prefs, theme, settings
extension/          GNOME Shell extension for floating readouts
packaging/          build-deb.sh
```

## Sampling vs. drawing

The kernel counts CPU time in 10 ms steps. Sampled 60 times a second, a single
core would jump between 0 %, 60 % and 100 % from rounding alone. So Plume
samples at 10 Hz (adjustable) and draws at the display's frame rate. Graphs run
one sample interval behind real time, so there is always real data at the live
edge and the scroll never waits for a sample.

## The graph widget

Rebuilding and rasterising curves every frame is what made GNOME System
Monitor's smooth graphs expensive. Plume splits the work:

1. **Per sample** (10 Hz): build the curve as a GskPath and have the GPU render
   it into a texture slightly wider than the graph.
2. **Per frame**: emit a clip, a few grid rectangles, and that texture shifted
   left. The offset snaps to device pixels and the texture is drawn at exactly
   its own pixel size, so line edges stay crisp instead of being resampled.
3. **Skip identical frames**: if the snapped offset hasn't changed, nothing is
   redrawn. On long time scales, new samples only trigger a rebuild once the
   curve has moved a whole pixel.
4. **Bound the work**: when a time scale holds more points than the graph has
   pixel columns, points are thinned to one per column, keeping each column's
   peak.

Lines are straight segments with mitred joins by default, so peaks stay sharp;
"Smooth curves" switches to a clamped Catmull-Rom spline that never overshoots.

## Only do what's on screen

* The process scan runs only while a process tab or an inspector is open.
* `/proc/<pid>/io` (one extra file per process) is read only on the Disk tab.
* Socket attribution runs only on the Network tab.
* Graph tick callbacks exist only while the graph is mapped, and GTK stops
  sending frames to hidden, minimised or suspended windows. With nothing
  visible, sampling drops to 1 Hz.
* Sampler threads raise their timer slack (1 ms / 5 ms) so the kernel can
  batch their wake-ups with other timers.

## The process list

The list holds N row objects that never move. Row *i* shows whichever process
is currently *i*-th in sort order. A refresh rewrites row contents in place and
touches only visible cells whose text changed, so the scroll position stays put
and nothing is rebuilt. The selection follows the process, and a PID that gets
recycled is never mistaken for the process you selected: actions check
`(pid, start time)` through a pidfd.

Alternating row colours are tagged by item position, which is stable in this
model. CSS `:nth-child` would flicker, because row widgets are recycled while
scrolling.

## GPUs

* **NVIDIA (Optimus / RTD3):** any NVML query keeps a sleeping dGPU awake. Plume
  reads `power/runtime_status` first, which is free and never wakes the device,
  and connects to NVML only when the GPU is already awake. It disconnects after
  3 s at 0 % with no GPU processes, so the GPU can power down again. "Asleep"
  in the UI means the GPU is powered down.
* **Intel:** intel_gpu_top-style engine counters need the perf PMU, which Ubuntu
  restricts to root. Plume derives `busy ≈ 1 − ΔRC6 / Δt` from the
  world-readable RC6 residency counter (i915) or the GT idle residency (xe).
* **AMD:** `gpu_busy_percent`, VRAM and hwmon sensors, also gated on runtime PM.

## Per-process network

Linux has no per-process byte counters. Plume dumps every TCP connection's
`bytes_acked` / `bytes_received` through `NETLINK_SOCK_DIAG` (no privileges
needed), maps connections to processes via the `socket:[inode]` links in
`/proc/<pid>/fd`, caches the result, and walks fd tables only for new
connections, at most 4 ms per scan. UDP (including QUIC / HTTP3) is not
attributed, and other users' processes need root. An eBPF helper would close
that gap.

Kernel counters measure what reaches the machine, not what an application has
read yet. A rate-limited download can briefly show more than its limit while
the socket buffer fills.

## Measurements

Taken in a 4-vCPU cloud VM with software rendering (llvmpipe):

| | cost |
|---|---|
| CPU / memory / network / disk sample | 5.6 / 3.8 / 3.6 / 7.6 µs |
| Process scan, 482 processes | 3.9 ms (≈ 0.4 % of a core at 1 Hz) |
| Whole sampler, 10 Hz + process scan at 1 Hz | 0.8 % of one core, 4.3 MB RSS |
| Graph code, Overview with 4 graphs | ≈ 1 % of one core |
| Graph code, one floating readout at 1 min / 1 h | 1.5 % / 0.18 % |
| Empty libadwaita window (`examples/baseline.rs`) | 116 MB RSS (Cairo) / 176 MB (llvmpipe) |
| Plume, Overview tab | 136 MB RSS (Cairo) / 190 MB (llvmpipe) |

Most of the memory figure is GTK, libadwaita and the GL driver. Plume adds about
15–20 MB on top of an empty window. llvmpipe loads LLVM and runs all GPU work on
the CPU, so expect lower memory and much lower total CPU on real graphics
hardware. Measure your own machine:

```bash
PLUME_PROFILE=1 plume                                 # graph-code cost, every 5 s
cargo run --release -p plume-core --example bench     # collector cost per sample
```

## Screenshots

The README screenshots come from a real workload (xz/gzip compression, a local
HTTP server and downloads, `dd` writes, a 1.4 GiB working set). It ran as an
unprivileged user in private PID and mount namespaces, so only that workload is
visible. The VM has no GPU, so no GPU cards appear.
