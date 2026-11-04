# Changelog

## 0.2.0 — 2026-11-04

Smoothness and efficiency, from a profiling pass on real-hardware feedback
(i7-9750H, UHD 630 + RTX 2080 Max-Q, 4K OLED).

### Graphs: new GPU renderer
- Graphs are now drawn by a small OpenGL fragment shader through `GtkGLArea`. Each new sample uploads a few hundred bytes; each frame sets one scroll uniform and issues one draw call. The old renderer re-rasterised and re-uploaded every curve ten times a second; its texture renders could stall the UI thread and drop frames, which was the graph jitter.
- Lines are computed per pixel at their exact sub-pixel position: perfectly even motion, no snapping or resampling blur.
- **Line width** is now set in screen pixels (Hairline 1 px, Fine 1.5 px default, Medium 2 px, Bold 3 px). The previous default was 3 device pixels on a 2× display.
- Auto-scaled axes (network, disk) ease smoothly every frame instead of stepping.
- Graphs skip frames that wouldn't change and share one GL context per window.
- The previous renderer remains as an automatic fallback without OpenGL.

### Layout stability
- The Overview uses a fixed one- or two-column layout chosen from the window width alone. Value labels have fixed widths, and stats sit in a fixed grid. Changing numbers no longer resize cards or the graphs inside them (measured: 0 graph resizes under load).

### Process list
- Refreshing the list now costs about 1 ms instead of 34–60 ms (213 ms worst case) with 300+ processes. Only on-screen rows re-render, off-screen rows catch up when scrolled to, and tooltips are produced on hover.

### GPU accounting
- Intel and AMD GPUs report real **engine utilisation** from the kernel's per-client DRM accounting (`fdinfo`), without root. The previous Intel figure measured time out of the RC6 sleep state. That figure is still shown as "Awake", but it overstates load: every frame anything draws wakes the GPU briefly.
- New **% GPU** column on the CPU tab, per process (Intel/AMD).

### Appearance
- **OLED Black** theme: true black surfaces, softened text, vivid accents.
- **Pure black backgrounds** switch: makes any dark theme OLED-friendly. In OLED mode, cards use hairline borders instead of blurred shadows.

### Frame pacing
- Separate frame-rate caps for the focused window (60 fps) and for floating readouts and background windows (30 fps).
- `PLUME_PROFILE=1` now also reports frame pacing (late frames, gaps, phase times) and process-list refresh cost.

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
