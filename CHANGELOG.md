# Changelog

## 0.1.0 — 2026-11-04

First release.

### Process management
- CPU, Memory, Disk and Network tabs in the style of macOS Activity Monitor: the process list with resource-specific columns, plus a live summary panel (CPU load with system/user split, memory and I/O pressure, IOPS, packets).
- Shared search, filter (all / mine / active) and selection across tabs. Alternating row colours. Sortable, resizable, reorderable columns.
- End, kill, stop, continue and change priority, guarded against PID reuse through pidfd. Administrator escalation through pkexec for other users' processes.
- A live process inspector: command line, executable, working directory, systemd unit, cgroup, memory breakdown, I/O, TCP traffic, open files, context switches, scheduling.
- Per-process network traffic (TCP) without root, via the kernel's socket diagnostics.

### Live readouts
- Graphs that scroll at the display's frame rate. Curves are drawn once per sample on the GPU and slid into place each frame, with sharp mitred lines and pixel-exact placement.
- Time scales from 30 seconds to 1 hour: header-bar picker, Ctrl+scroll over a graph, or Ctrl+− / Ctrl+=. Each series keeps 10 Hz samples for about 5 minutes and 1-second means for an hour.
- An Overview dashboard with CPU (combined or per core), memory, every GPU, network, disk and filesystems.
- Floating readouts for CPU, CPU cores, memory, each GPU, network and disk, each with its own time scale and opacity. The bundled Plume Floats GNOME Shell extension keeps them on top on Wayland and remembers their positions.

### Hardware
- Intel graphics (i915 / xe) utilisation without root, from the RC6 / idle residency counters.
- NVIDIA through NVML, gated on the PCI runtime power state, so an Optimus laptop's sleeping GPU stays asleep.
- AMD through sysfs. CPU frequency, package temperature and pressure stall information.

### Appearance
- Fifteen themes: System (follows light/dark), Adwaita Light/Dark, Tokyo Night (Night, Storm, Day), Solarized Light/Dark, Nord, Gruvbox Light/Dark, Dracula, Catppuccin Mocha/Latte, One Dark.

### Footprint
- About 1 % of one core for four live graphs at 60 fps. The sampler uses 0.8 % of one core with the process list open. Plume adds about 15–20 MB on top of the GTK/libadwaita baseline. Details in [docs/DESIGN.md](docs/DESIGN.md).
