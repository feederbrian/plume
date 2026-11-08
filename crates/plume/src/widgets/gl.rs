//! Minimal OpenGL access for the graph renderer.
//!
//! GTK already links libepoxy, which exports every GL entry point as a
//! dispatch *pointer variable* (`epoxy_glFoo`). We look those variables up once
//! and call through them, exactly like C code that includes <epoxy/gl.h>. That
//! keeps Plume free of extra GL crates and works for desktop GL and GLES, on
//! EGL (Wayland) and GLX alike.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

pub const TEXTURE_2D: u32 = 0x0DE1;
pub const TEXTURE0: u32 = 0x84C0;
pub const RGBA: u32 = 0x1908;
pub const RGBA32F: i32 = 0x8814;
pub const FLOAT: u32 = 0x1406;
pub const NEAREST: i32 = 0x2600;
pub const TEXTURE_MIN_FILTER: u32 = 0x2801;
pub const TEXTURE_MAG_FILTER: u32 = 0x2800;
pub const TEXTURE_WRAP_S: u32 = 0x2802;
pub const TEXTURE_WRAP_T: u32 = 0x2803;
pub const CLAMP_TO_EDGE: i32 = 0x812F;
pub const UNPACK_ALIGNMENT: u32 = 0x0CF5;
pub const VERTEX_SHADER: u32 = 0x8B31;
pub const FRAGMENT_SHADER: u32 = 0x8B30;
pub const COMPILE_STATUS: u32 = 0x8B81;
pub const LINK_STATUS: u32 = 0x8B82;
pub const TRIANGLES: u32 = 0x0004;
pub const TRIANGLE_STRIP: u32 = 0x0005;
pub const COLOR_BUFFER_BIT: u32 = 0x0000_4000;
pub const ONE: u32 = 1;
pub const ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
pub const BLEND: u32 = 0x0BE2;
pub const DEPTH_TEST: u32 = 0x0B71;
pub const SCISSOR_TEST: u32 = 0x0C11;
pub const VIEWPORT: u32 = 0x0BA2;

macro_rules! gl_api {
    ($($field:ident = $sym:literal : $ty:ty;)*) => {
        /// Addresses of libepoxy's dispatch pointer variables.
        pub struct Gl { $($field: *const *const c_void,)* }
        // The variables live for the whole process and are only used on the GTK thread.
        unsafe impl Send for Gl {}
        unsafe impl Sync for Gl {}
        impl Gl {
            fn load(h: *mut c_void) -> Option<Gl> {
                Some(Gl { $($field: {
                    let p = unsafe { libc::dlsym(h, concat!($sym, "\0").as_ptr() as *const c_char) };
                    if p.is_null() { return None; }
                    p as *const *const c_void
                },)* })
            }
            $(
                #[inline]
                #[allow(clippy::missing_safety_doc)]
                pub fn $field(&self) -> $ty {
                    // Read the dispatch pointer at call time: epoxy rewrites it
                    // to the resolved driver function after the first call.
                    unsafe { std::mem::transmute::<*const c_void, $ty>(*self.$field) }
                }
            )*
        }
    };
}

gl_api! {
    create_shader = "epoxy_glCreateShader": unsafe extern "C" fn(u32) -> u32;
    shader_source = "epoxy_glShaderSource": unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32);
    compile_shader = "epoxy_glCompileShader": unsafe extern "C" fn(u32);
    get_shader_iv = "epoxy_glGetShaderiv": unsafe extern "C" fn(u32, u32, *mut i32);
    get_shader_info_log = "epoxy_glGetShaderInfoLog": unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char);
    delete_shader = "epoxy_glDeleteShader": unsafe extern "C" fn(u32);
    create_program = "epoxy_glCreateProgram": unsafe extern "C" fn() -> u32;
    attach_shader = "epoxy_glAttachShader": unsafe extern "C" fn(u32, u32);
    link_program = "epoxy_glLinkProgram": unsafe extern "C" fn(u32);
    get_program_iv = "epoxy_glGetProgramiv": unsafe extern "C" fn(u32, u32, *mut i32);
    get_program_info_log = "epoxy_glGetProgramInfoLog": unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char);
    is_program = "epoxy_glIsProgram": unsafe extern "C" fn(u32) -> u8;
    use_program = "epoxy_glUseProgram": unsafe extern "C" fn(u32);
    get_uniform_location = "epoxy_glGetUniformLocation": unsafe extern "C" fn(u32, *const c_char) -> i32;
    uniform1i = "epoxy_glUniform1i": unsafe extern "C" fn(i32, i32);
    uniform1f = "epoxy_glUniform1f": unsafe extern "C" fn(i32, f32);
    uniform2f = "epoxy_glUniform2f": unsafe extern "C" fn(i32, f32, f32);
    uniform1fv = "epoxy_glUniform1fv": unsafe extern "C" fn(i32, i32, *const f32);
    uniform4fv = "epoxy_glUniform4fv": unsafe extern "C" fn(i32, i32, *const f32);
    gen_vertex_arrays = "epoxy_glGenVertexArrays": unsafe extern "C" fn(i32, *mut u32);
    bind_vertex_array = "epoxy_glBindVertexArray": unsafe extern "C" fn(u32);
    delete_vertex_arrays = "epoxy_glDeleteVertexArrays": unsafe extern "C" fn(i32, *const u32);
    gen_textures = "epoxy_glGenTextures": unsafe extern "C" fn(i32, *mut u32);
    delete_textures = "epoxy_glDeleteTextures": unsafe extern "C" fn(i32, *const u32);
    bind_texture = "epoxy_glBindTexture": unsafe extern "C" fn(u32, u32);
    active_texture = "epoxy_glActiveTexture": unsafe extern "C" fn(u32);
    tex_parameteri = "epoxy_glTexParameteri": unsafe extern "C" fn(u32, u32, i32);
    tex_image_2d = "epoxy_glTexImage2D": unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
    tex_sub_image_2d = "epoxy_glTexSubImage2D": unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
    pixel_storei = "epoxy_glPixelStorei": unsafe extern "C" fn(u32, i32);
    draw_arrays = "epoxy_glDrawArrays": unsafe extern "C" fn(u32, i32, i32);
    disable = "epoxy_glDisable": unsafe extern "C" fn(u32);
    enable = "epoxy_glEnable": unsafe extern "C" fn(u32);
    blend_func = "epoxy_glBlendFunc": unsafe extern "C" fn(u32, u32);
    clear_color = "epoxy_glClearColor": unsafe extern "C" fn(f32, f32, f32, f32);
    clear = "epoxy_glClear": unsafe extern "C" fn(u32);
    get_integerv = "epoxy_glGetIntegerv": unsafe extern "C" fn(u32, *mut i32);
    viewport = "epoxy_glViewport": unsafe extern "C" fn(i32, i32, i32, i32);
}

/// The GL entry points, or None if libepoxy can't be found.
pub fn api() -> Option<&'static Gl> {
    static G: OnceLock<Option<Gl>> = OnceLock::new();
    G.get_or_init(|| {
        let h = ["libepoxy.so.0", "libepoxy.so"].iter().find_map(|n| {
            let c = CString::new(*n).ok()?;
            // Already loaded by GTK; NOLOAD avoids a second copy.
            let mut h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD) };
            if h.is_null() {
                h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY) };
            }
            (!h.is_null()).then_some(h)
        })?;
        Gl::load(h)
    })
    .as_ref()
}

/// Uniform locations of the graph program.
#[derive(Clone, Copy, Debug)]
pub struct Locs {
    pub data: i32,
    pub texw: i32,
    pub u0: i32,
    pub size: i32,
    pub ymax: i32,
    pub pad: i32,
    pub n: i32,
    pub fill: i32,
    pub line: i32,
    pub hw: i32,
    pub grid: i32,
    pub vgrid: i32,
    pub nv: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct Program {
    pub id: u32,
    pub loc: Locs,
}

/// The graph programs. `strip` + `grid` is the normal path: the framebuffer is
/// cleared, grid lines are drawn as 1 px quads, and the curve shader runs only
/// on a strip that covers the area under the curves. `full` runs the curve and
/// grid shader on every pixel (the 0.2.0–0.2.1 renderer), for comparison.
#[derive(Clone, Copy, Debug)]
pub struct Programs {
    pub full: Program,
    pub strip: Program,
    pub grid: Program,
}

const VERTEX_FULL: &str = r#"
void main() {
    // One triangle that covers the viewport.
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// Pixel columns per strip block. Blocks this wide keep the strip's triangles
/// cheap to set up and efficient to shade (GPUs shade 2×2 pixel quads, so
/// 1 px wide triangles would waste half of every quad).
pub const STRIP_BLOCK: i32 = 8;

/// A triangle strip with a vertex pair at every block boundary: one on the
/// bottom edge, one just above the curves' envelope over the two blocks that
/// meet there. Each pixel column's data column comes from the same formula as
/// in the fragment shader, and the envelope covers that shader's whole
/// 6-column window, so the strip contains every pixel the curve shader can
/// touch (with a pixel to spare), and nothing above it is ever shaded.
const VERTEX_STRIP: &str = r#"
uniform sampler2D u_data;
uniform int u_texw;
uniform float u_u0;
uniform vec2 u_size;
uniform float u_ymax;
uniform float u_pad;
uniform int u_n;
uniform float u_hw;
const int BLOCK = 8;

float top_at(int i) {
    vec4 e = texelFetch(u_data, ivec2(i % u_texw, 1), 0);
    float t = -1.0;
    if (e.y >= 0.0) t = e.y;
    if (u_n > 1 && e.w >= 0.0) t = max(t, e.w);
    return t;
}

void main() {
    float x = min(float((gl_VertexID >> 1) * BLOCK), u_size.x);
    float y = 0.0;
    if ((gl_VertexID & 1) == 1) {
        // Data column of the first pixel column of the block to the left
        // (kept non-negative, see the fragment shader); consecutive pixel
        // columns use consecutive data columns.
        int i = int(floor(u_u0 - u_size.x + x - float(BLOCK) + 0.5 + float(u_texw)));
        float t = -1.0;
        for (int k = 0; k < 2 * BLOCK; k++) t = max(t, top_at(i + k));
        if (t >= 0.0) {
            float h = clamp(t / u_ymax, 0.0, 1.0) * (u_size.y - u_pad);
            y = clamp(h + u_hw + 2.5, 0.0, u_size.y);
        }
    }
    gl_Position = vec4(x / u_size.x * 2.0 - 1.0, y / u_size.y * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// Grid lines as quads, 6 vertices each: three horizontal lines at the
/// quarters, then `u_nv` vertical time lines. Same pixels as the full shader.
const VERTEX_GRID: &str = r#"
uniform vec2 u_size;
uniform float u_vgrid[8];
uniform int u_nv;
void main() {
    int q = gl_VertexID / 6;
    int v = gl_VertexID - q * 6;
    vec2 c = vec2((v == 1 || v == 4 || v == 5) ? 1.0 : 0.0, (v == 2 || v == 3 || v == 5) ? 1.0 : 0.0);
    vec2 lo, hi;
    if (q < 3) {
        float y = floor(u_size.y * 0.25 * float(q + 1));
        lo = vec2(0.0, y);
        hi = vec2(u_size.x, y + 1.0);
    } else {
        float x = floor(u_vgrid[q - 3]);
        lo = vec2(x, 0.0);
        hi = vec2(x + 1.0, u_size.y);
    }
    gl_Position = vec4(mix(lo, hi, c) / u_size * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT_GRID: &str = r#"
uniform vec4 u_grid;
layout(location = 0) out vec4 frag;
void main() { frag = u_grid; }
"#;

/// The whole graph is one fragment shader, specialised for up to two series.
/// Data lives in a 2-row RGBA32F ring texture: one texel per device-pixel
/// column, series in .x/.y of row 0, and [min, max] envelopes of each series
/// over the shader's 6-column window in row 1 (.xy, .zw). Each fragment finds its data coordinate from the scroll uniform,
/// then computes coverage exactly: fill from the signed distance to the curve,
/// line from the distance to the nearby polyline segments. Lines therefore
/// have a precise width at any sub-pixel scroll position, with no resampling
/// blur, and scrolling costs one uniform per frame.
const FRAGMENT: &str = r#"
uniform sampler2D u_data;
uniform int u_texw;
uniform float u_u0;      // data coordinate (column) at the right edge
uniform vec2 u_size;     // viewport in device pixels
uniform float u_ymax;
uniform float u_pad;     // top padding, device px
uniform int u_n;         // number of series: 1 or 2
uniform vec4 u_fill[2];  // premultiplied; alpha 0 = no fill
uniform vec4 u_line[2];  // premultiplied; alpha 0 = no line
uniform float u_hw;      // half line width, device px
uniform vec4 u_grid;     // premultiplied grid colour
uniform float u_vgrid[8];
uniform int u_nv;
layout(location = 0) out vec4 frag;

// Column indices are kept non-negative (see main), because GLSL leaves `%`
// undefined for negative operands.
vec4 col_at(int i) { return texelFetch(u_data, ivec2(i % u_texw, 0), 0); }
vec2 ypix(vec2 v) { return clamp(v / u_ymax, 0.0, 1.0) * (u_size.y - u_pad); }
vec4 over(vec4 dst, vec4 src) { return src + dst * (1.0 - src.a); }

// Distance from p to segment a-b for both series at once (x shared).
vec2 segdist2(vec2 p, float xa, vec2 ya, float xb, vec2 yb) {
    vec2 pax = vec2(p.x - xa), pay = p.y - ya;
    float bax = xb - xa;
    vec2 bay = yb - ya;
    vec2 h = clamp((pax * bax + pay * bay) / max(bax * bax + bay * bay, 1e-6), 0.0, 1.0);
    vec2 dx = pax - bax * h, dy = pay - bay * h;
    return sqrt(dx * dx + dy * dy);
}

// Fills and lines over `col` at pixel p.
vec4 curve(vec2 p, vec4 col) {
    // u_u0 is in [0, texw) and texw > width + 8, so adding texw keeps every
    // index we touch (i0 - 2 .. i0 + 3) non-negative.
    float u = u_u0 - u_size.x + p.x + float(u_texw);
    float fu = floor(u);
    int i0 = int(fu);
    float f = u - fu;
    int k0 = i0 % u_texw;

    // Envelope (min/max over this pixel's 6-column window) for both series.
    vec4 env = texelFetch(u_data, ivec2(k0, 1), 0);
    bvec2 has = bvec2(env.y >= 0.0, u_n > 1 && env.w >= 0.0);
    vec2 ytop = ypix(vec2(env.y, env.w));
    vec2 ybot = ypix(vec2(env.x, env.z));
    float slack = u_hw + 1.5;
    float top = max(has.x ? ytop.x : -1e9, has.y ? ytop.y : -1e9);
    if (p.y > top + slack) return col;   // empty space above the curves

    // Fills: flat inside, exact coverage only along the boundary.
    vec4 c2 = col_at(i0), c3 = col_at(i0 + 1);
    vec2 y2 = ypix(c2.xy), y3 = ypix(c3.xy);
    vec2 slope = y3 - y2;
    vec2 sd = (mix(y2, y3, f) - p.y) * inversesqrt(1.0 + slope * slope);
    vec2 fcov = clamp(sd + 0.5, 0.0, 1.0);
    if (has.x && u_fill[0].a > 0.0 && c2.x >= 0.0 && c3.x >= 0.0) col = over(col, u_fill[0] * fcov.x);
    if (has.y && u_fill[1].a > 0.0 && c2.y >= 0.0 && c3.y >= 0.0) col = over(col, u_fill[1] * fcov.y);

    // Lines: exact distance to the 5 nearby segments, only near the curve.
    bvec2 near = bvec2(has.x && u_line[0].a > 0.0 && p.y <= ytop.x + slack && p.y >= ybot.x - slack,
                       has.y && u_line[1].a > 0.0 && p.y <= ytop.y + slack && p.y >= ybot.y - slack);
    if (near.x || near.y) {
        vec4 c0 = col_at(i0 - 2), c1 = col_at(i0 - 1), c4 = col_at(i0 + 2), c5 = col_at(i0 + 3);
        vec2 y0 = ypix(c0.xy), y1 = ypix(c1.xy), y4 = ypix(c4.xy), y5 = ypix(c5.xy);
        float x0 = p.x - f - 2.0;
        vec2 big = vec2(1e9);
        vec2 d = big;
        d = min(d, mix(big, segdist2(p, x0,       y0, x0 + 1.0, y1), vec2(greaterThanEqual(min(c0.xy, c1.xy), vec2(0.0)))));
        d = min(d, mix(big, segdist2(p, x0 + 1.0, y1, x0 + 2.0, y2), vec2(greaterThanEqual(min(c1.xy, c2.xy), vec2(0.0)))));
        d = min(d, mix(big, segdist2(p, x0 + 2.0, y2, x0 + 3.0, y3), vec2(greaterThanEqual(min(c2.xy, c3.xy), vec2(0.0)))));
        d = min(d, mix(big, segdist2(p, x0 + 3.0, y3, x0 + 4.0, y4), vec2(greaterThanEqual(min(c3.xy, c4.xy), vec2(0.0)))));
        d = min(d, mix(big, segdist2(p, x0 + 4.0, y4, x0 + 5.0, y5), vec2(greaterThanEqual(min(c4.xy, c5.xy), vec2(0.0)))));
        vec2 lcov = clamp(u_hw + 0.5 - d, 0.0, 1.0);
        if (near.x) col = over(col, u_line[0] * lcov.x);
        if (near.y) col = over(col, u_line[1] * lcov.y);
    }
    return col;
}
"#;

/// Every pixel: grid lines, then the curves.
const MAIN_FULL: &str = r#"
void main() {
    vec2 p = gl_FragCoord.xy;
    vec4 col = vec4(0.0);
    if (u_grid.a > 0.0) {
        float hy = u_size.y * 0.25;
        for (int k = 1; k < 4; k++) {
            if (abs(p.y - (floor(hy * float(k)) + 0.5)) < 0.5) col = over(col, u_grid);
        }
        for (int k = 0; k < 8; k++) {
            if (k >= u_nv) break;
            if (abs(p.x - (floor(u_vgrid[k]) + 0.5)) < 0.5) col = over(col, u_grid);
        }
    }
    frag = curve(p, col);
}
"#;

/// Strip pixels only: the curves, blended over the grid already drawn.
const MAIN_STRIP: &str = r#"
void main() { frag = curve(gl_FragCoord.xy, vec4(0.0)); }
"#;

fn compile(gl: &Gl, kind: u32, header: &str, body: &str) -> Result<u32, String> {
    let src = CString::new(format!("{header}{body}")).map_err(|e| e.to_string())?;
    unsafe {
        let sh = (gl.create_shader())(kind);
        let ptr = src.as_ptr();
        (gl.shader_source())(sh, 1, &ptr, std::ptr::null());
        (gl.compile_shader())(sh);
        let mut ok = 0;
        (gl.get_shader_iv())(sh, COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut buf = vec![0 as c_char; 4096];
            let mut len = 0;
            (gl.get_shader_info_log())(sh, buf.len() as i32, &mut len, buf.as_mut_ptr());
            (gl.delete_shader())(sh);
            return Err(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
        }
        Ok(sh)
    }
}

/// Compiles and links the graph programs for the current context.
pub fn build_programs(gl: &Gl, gles: bool) -> Result<Programs, String> {
    let header = if gles {
        "#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;\n"
    } else {
        "#version 330 core\n"
    };
    Ok(Programs {
        full: link(gl, header, VERTEX_FULL, &format!("{FRAGMENT}{MAIN_FULL}"))?,
        strip: link(gl, header, VERTEX_STRIP, &format!("{FRAGMENT}{MAIN_STRIP}"))?,
        grid: link(gl, header, VERTEX_GRID, FRAGMENT_GRID)?,
    })
}

fn link(gl: &Gl, header: &str, vertex: &str, fragment: &str) -> Result<Program, String> {
    let vs = compile(gl, VERTEX_SHADER, header, vertex)?;
    let fs = match compile(gl, FRAGMENT_SHADER, header, fragment) {
        Ok(fs) => fs,
        Err(e) => {
            unsafe { (gl.delete_shader())(vs) };
            return Err(e);
        }
    };
    unsafe {
        let p = (gl.create_program())();
        (gl.attach_shader())(p, vs);
        (gl.attach_shader())(p, fs);
        (gl.link_program())(p);
        (gl.delete_shader())(vs);
        (gl.delete_shader())(fs);
        let mut ok = 0;
        (gl.get_program_iv())(p, LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut buf = vec![0 as c_char; 4096];
            let mut len = 0;
            (gl.get_program_info_log())(p, buf.len() as i32, &mut len, buf.as_mut_ptr());
            return Err(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
        }
        let loc = |n: &str| {
            let c = CString::new(n).unwrap();
            (gl.get_uniform_location())(p, c.as_ptr())
        };
        Ok(Program {
            id: p,
            loc: Locs {
                data: loc("u_data"),
                texw: loc("u_texw"),
                u0: loc("u_u0"),
                size: loc("u_size"),
                ymax: loc("u_ymax"),
                pad: loc("u_pad"),
                n: loc("u_n"),
                fill: loc("u_fill"),
                line: loc("u_line"),
                hw: loc("u_hw"),
                grid: loc("u_grid"),
                vgrid: loc("u_vgrid"),
                nv: loc("u_nv"),
            },
        })
    }
}
