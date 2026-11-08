# Changelog

## 0.2.2 — 2026-11-08

### Floating readouts
- The CPU readout can show every logical CPU: right-click it and choose **Logical CPUs**. Enlarge the readout to give them room. The separate CPU Cores readout is still there.

### Lighter on the GPU
- **Paced redraws.** A scrolling graph now redraws once it has moved about one pixel, rather than every quarter pixel. All graphs in a window share one frame grid, so the window and the compositor redraw less often too. Every step covers the same distance, so motion stays even. At the default 1-minute scale on a 4K screen, the Overview's graphs redraw about 30 times a second instead of 60; narrower graphs and longer time scales redraw far less. Preferences → Performance → **Graph motion** chooses Smoothest (the previous behaviour), Balanced (the default) or Battery saver.
- **Shading only under the curves.** The graph shader no longer runs on every pixel. The framebuffer is cleared, grid lines are drawn as thin quads, and the curve shader runs only on a strip that covers the area under the curves, in 8-pixel blocks. Output matches the previous renderer: grid-line pixels differ by at most 2/255.
- **Graphs scrolled out of view do no work** until they're scrolled back.
- In a software-rendered test (Mesa llvmpipe, 1× scale, Overview), Plume's CPU use fell from 79 % to 23 % of a core.

### Measuring on your own machine
- `plume --bench` runs the Overview through each renderer variant, from 0.2.1 to the current defaults. For each it prints the GPU time of Plume and of the compositor (from the kernel's per-client GPU accounting, as `intel_gpu_top` uses), the window's redraw rate, graph draws per second and Plume's CPU use. Quit Plume first.
- `PLUME_PROFILE=1 plume` also prints Plume's and gnome-shell's GPU busy time every 5 s.
- About → Troubleshooting shows which renderer GTK uses (Vulkan or OpenGL) alongside the graph renderer.

### Fixed
- If the display time stood still while samples kept arriving, a graph's incremental update could request an invalid texture upload. Empty update ranges are now ignored.

## 0.2.1 — 2026-11-04

### Floating readouts
- **Resize** a readout by dragging its bottom-right corner. The grip is invisible; the pointer turns into a resize arrow over it. The compositor runs the resize, and each readout remembers its size.
- Readouts can be much smaller than before (about 120 × 64 px for CPU and GPU). When the title no longer fits beside the value, the title is hidden rather than shown as "…".
- Double-clicking a readout no longer also asks the window manager to maximize it; it only opens the main window, as intended.

### Overview
- **Reorder cards** by dragging a card's header (the pointer shows an open hand). Nothing moves while you drag. The card dims, an outline marks the slot it will take, and the cards rearrange once you let go. Near the top or bottom edge, the page scrolls. The order is remembered, and a newly detected GPU card appears after its neighbour in the default order.

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
