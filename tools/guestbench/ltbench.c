/* LT Bench: microbenchmarks for the guest paths an emulator makes expensive.
 *
 * A game's frame rate mixes everything; this splits it into the paths QEMU
 * handles outside straight-line translated code: system calls and exceptions,
 * context switches between threads and between address spaces, page faults,
 * TLB reach, indirect branches and returns, and plain compute and memory as the
 * baseline. Each prints ns per operation (best and median of several rounds) to
 * the engine log through the QEMU_CALL log channel and on screen.
 * See docs/GUEST-BENCHMARKS.md.
 *
 * iPhone OS 3.1.3, ARMv6 ARM state, no SDK: public libSystem/UIKit calls are
 * declared here with their ARM32 ABI (the pattern of
 * Linux/tests/guest-install-probe/probe.c).
 * SPDX-License-Identifier: GPL-2.0-or-later
 */
typedef unsigned long size_t;
typedef unsigned long long u64;
typedef unsigned int u32;
typedef void *Object;
typedef void *Selector;
typedef struct { float x, y; } Point;
typedef struct { float width, height; } Size;
typedef struct { Point origin; Size size; } Rect;

extern void *dlopen(const char *, int);
extern void *dlsym(void *, const char *);
extern long write(int, const void *, size_t);
extern long read(int, void *, size_t);
extern int close(int);
extern int pipe(int *);
extern int fork(void);
extern int waitpid(int, int *, int);
extern int getppid(void);
extern void _exit(int);
extern void *mmap(void *, size_t, int, int, int, long long);
extern int munmap(void *, size_t);
extern void *malloc(size_t);
extern void free(void *);
extern void *memcpy(void *, const void *, size_t);
extern void *memset(void *, int, size_t);
extern int usleep(u32);
extern u64 mach_absolute_time(void);
extern int mach_timebase_info(u32 *);
extern int pthread_create(void **, const void *, void *(*)(void *), void *);
extern int pthread_join(void *, void **);
extern u32 bootstrap_port;
extern int bootstrap_look_up(u32, const char *, u32 *);
extern int open(const char *, int, ...);
extern int fcntl(int, int, ...);
extern int fsync(int);
extern long pread(int, void *, size_t, long long);
extern long pwrite(int, const void *, size_t, long long);
extern int unlink(const char *);
extern char *getenv(const char *);

/* Integer division: ARMv6 has no divide instruction, and the helpers clang calls for it
 * (__udivdi3 and friends) are in iPhone OS 3's libgcc_s, not libSystem: imported from
 * libSystem they fail to bind at their first call. Defined here, the link binds them locally. */
static u64 udivmod64(u64 n, u64 d, u64 *rem)
{
    u64 q = 0, r = 0;
    for (int i = 63; i >= 0; i--) {
        r = r << 1 | (n >> i & 1);
        if (r >= d) { r -= d; q |= 1ull << i; }
    }
    if (rem) *rem = r;
    return q;
}
u64 __udivdi3(u64 n, u64 d) { return udivmod64(n, d, 0); }
u64 __umoddi3(u64 n, u64 d) { u64 r; udivmod64(n, d, &r); return r; }
u32 __udivsi3(u32 n, u32 d) { return (u32)udivmod64(n, d, 0); }
u32 __umodsi3(u32 n, u32 d) { u64 r; udivmod64(n, d, &r); return (u32)r; }

/* ---------------------------------------------------------------- output --- */

typedef struct __attribute__((packed)) {
    u32 call_number;
    struct __attribute__((packed)) { u32 slot, ctx, argc, spill, args[4]; } gles;   /* 32 bytes */
    long long retval, error;
} QemuCall;
typedef char qemu_call_is_52_bytes[sizeof(QemuCall) == 52 ? 1 : -1];
#define QC_GLES 0x140
#define GLES_OP_LOG 0x1002

static u32 slen(const char *s) { u32 n = 0; while (s[n]) n++; return n; }

/* One line to the engine log (the host prints lines that repeat only a few times,
 * so every result line carries its numbers) and to stderr. */
static void emit(const char *s)
{
    volatile QemuCall q;
    u32 n = slen(s);
    write(2, s, n);
    memset((void *)&q, 0, sizeof(q));
    q.call_number = QC_GLES;
    q.gles.slot = GLES_OP_LOG;
    q.gles.argc = 2;
    q.gles.args[0] = (u32)s;
    q.gles.args[1] = n;
    __asm__ volatile("mcr p15, 3, %0, c15, c15, 0" : : "r"(&q) : "memory");
}

typedef struct { char b[160]; u32 n; } Line;
static void put(Line *l, const char *s) { while (*s && l->n < sizeof(l->b) - 1) l->b[l->n++] = *s++; l->b[l->n] = 0; }
static void putu(Line *l, u64 v)
{
    char t[24], *p = t + 23;
    *p = 0;
    do { *--p = '0' + (char)(v % 10); v /= 10; } while (v);
    put(l, p);
}
static void putx(Line *l, u32 v)
{
    char t[11] = "0x", *p = t + 2;
    for (int i = 7; i >= 0; i--) *p++ = "0123456789abcdef"[(v >> (i * 4)) & 15];
    *p = 0;
    put(l, t);
}
/* v in tenths, printed with one decimal */
static void putd(Line *l, u64 tenths) { char d[2] = {(char)('0' + tenths % 10), 0}; putu(l, tenths / 10); put(l, "."); put(l, d); }

/* --------------------------------------------------------------- timing --- */

static u32 tb[2];
static u64 now_ns(void) { return mach_absolute_time() * tb[0] / tb[1]; }

#ifndef ROUNDS
#define ROUNDS 5
#endif
typedef u64 (*Bench)(u32 n);   /* runs n operations, returns elapsed ns */
typedef struct { const char *name, *what; Bench run; u32 n; } Test;

static volatile u32 sink;

/* Eight independent accumulators model the scalar VFP2 unrolling in libvDSP.
 * Exercise all four non-fused forms; no per-instruction host timers. The
 * non-exact product quickly establishes the sticky inexact flag used in audio.
 * n is a multiple of eight in the table below. */
static u64 b_vfp_mla(u32 n)
{
    static const float values[10] = {1, 2, 3, 4, 5, 6, 7, 8, .1f, .3f};
    u32 loops = n / 8, result;
    u64 start = now_ns();
    __asm__ volatile(
        "vldmia %2, {s0-s9}\n"
        "1:\n"
        "vmla.f32 s0, s8, s9\n"
        "vmls.f32 s1, s8, s9\n"
        "vnmla.f32 s2, s8, s9\n"
        "vnmls.f32 s3, s8, s9\n"
        "vmla.f32 s4, s8, s9\n"
        "vmls.f32 s5, s8, s9\n"
        "vnmla.f32 s6, s8, s9\n"
        "vnmls.f32 s7, s8, s9\n"
        "subs %0, %0, #1\n"
        "bne 1b\n"
        "vmov %1, s0\n"
        : "+r"(loops), "=r"(result) : "r"(values)
        : "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "cc", "memory");
    u64 elapsed = now_ns() - start;
    sink = result;
    return elapsed;
}

/* ------------------------------------------------------------- objc/UIKit --- */

static Object (*get_class)(const char *);
static Selector (*get_selector)(const char *);
static void *send;
static Object window, label, delegate_object;

static Object call0(Object object, const char *name) {
    return ((Object (*)(Object, Selector))send)(object, get_selector(name));
}
static Object call1(Object object, const char *name, Object value) {
    return ((Object (*)(Object, Selector, Object))send)(object, get_selector(name), value);
}

/* ------------------------------------------------------------------- GL --- */

/* The GL tests draw through OpenGLES.framework as a game does: an EAGLContext on a full-screen
 * CAEAGLLayer (set up at launch on the main thread, made current on the bench thread), ES 1.1,
 * client-side vertex arrays. Each run ends with glFinish, so calls the guest shim queues are
 * counted when they are done, not when they are queued. The entry points come through dlsym, like
 * everything else here (contrib/it-gles/glapp.c in qemu-ios does the same setup). */
#define GL_TRIANGLE_STRIP 5
#define GL_FLOAT 0x1406
#define GL_FIXED 0x140C
#define GL_TRIANGLES 4
#define GL_UNSIGNED_SHORT 0x1403
#define GL_UNSIGNED_BYTE 0x1401
#define GL_RGBA 0x1908
#define GL_BLEND 0x0BE2
#define GL_TEXTURE_2D 0x0DE1
#define GL_VERTEX_ARRAY 0x8074
#define GL_TEXTURE_COORD_ARRAY 0x8078
#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_PROJECTION 0x1701
#define GL_MODELVIEW 0x1700
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_LINEAR 0x2601
#define GL_SRC_ALPHA 0x0302
#define GL_ONE_MINUS_SRC_ALPHA 0x0303
#define GL_RENDERBUFFER_OES 0x8D41
#define GL_FRAMEBUFFER_OES 0x8D40
#define GL_COLOR_ATTACHMENT0_OES 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE_OES 0x8CD5
#define GL_W 320
#define GL_H 480
#define SPRITES 64      /* distinct quads, cycled: 8 x 8 grid of 32 x 32 sprites */
#define UPLOAD_SIDE 64  /* gl-upload: a 64 x 64 RGBA sub-image, 16 KiB */
/* gl-mesh: a 3D game's level geometry, one interleaved array drawn in small indexed pieces.
 * Each piece is a 2 x 25 strip of vertices, 48 triangles (144 indices). */
#define MESH_VERTS 16384
#define MESH_PIECE 50
#define MESH_PIECES (MESH_VERTS / MESH_PIECE)
#define MESH_INDICES 144

static struct {
    void (*Finish)(void), (*Flush)(void);
    unsigned (*GetError)(void);
    void (*Enable)(unsigned), (*Disable)(unsigned), (*EnableClientState)(unsigned);
    void (*BlendFunc)(unsigned, unsigned);
    void (*Viewport)(int, int, int, int), (*MatrixMode)(unsigned), (*LoadIdentity)(void);
    void (*Orthof)(float, float, float, float, float, float);
    void (*ClearColor)(float, float, float, float), (*Clear)(unsigned);
    void (*VertexPointer)(int, unsigned, int, const void *), (*TexCoordPointer)(int, unsigned, int, const void *);
    void (*DrawArrays)(unsigned, int, int), (*DrawElements)(unsigned, int, unsigned, const void *);
    void (*GenTextures)(int, unsigned *), (*BindTexture)(unsigned, unsigned);
    void (*TexParameteri)(unsigned, unsigned, int);
    void (*TexImage2D)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *);
    void (*TexSubImage2D)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *);
    void (*GenRenderbuffersOES)(int, unsigned *), (*BindRenderbufferOES)(unsigned, unsigned);
    void (*GenFramebuffersOES)(int, unsigned *), (*BindFramebufferOES)(unsigned, unsigned);
    void (*FramebufferRenderbufferOES)(unsigned, unsigned, unsigned, unsigned);
    unsigned (*CheckFramebufferStatusOES)(unsigned);
} gl;
static Object gl_view, gl_context;
static void *objc_runtime;
static unsigned gl_texture[2], gl_framebuffer, gl_renderbuffer;
static float sprite_xy[SPRITES][8], sprite_uv[8] = {0, 0, 1, 0, 0, 1, 1, 1};
static unsigned char upload_pixels[UPLOAD_SIDE * UPLOAD_SIDE * 4];
static struct { float x, y, z; int u, v; } mesh[MESH_VERTS];   /* GL_FIXED texture coordinates */
static unsigned short mesh_index[MESH_PIECES][MESH_INDICES];
static const char *gl_problem = "not set up";

/* +[EAGLContext setCurrentContext:] (BOOL): per thread. */
static int make_current(Object context)
{
    return ((signed char (*)(Object, Selector, Object))send)(get_class("EAGLContext"),
            get_selector("setCurrentContext:"), context) != 0;
}

/* +layerClass of the GL view: UIView asks for it while building the view's layer. */
static Object gl_layer_class(Object self, Selector selector) { (void)self; (void)selector; return get_class("CAEAGLLayer"); }

/* Main thread, at launch: the view (under the label), the context, the drawable and the textures.
 * Leaves gl_problem set (and the GL tests unavailable) if any of it fails. */
static void gl_setup(Object under)
{
    void *runtime = objc_runtime;
    void *h = dlopen("/System/Library/Frameworks/OpenGLES.framework/OpenGLES", 2);
    if (!h) { gl_problem = "no OpenGLES"; return; }
#define GLSYM(name) if (!(*(void **)&gl.name = dlsym(h, "gl" #name))) { gl_problem = "missing gl" #name; return; }
    GLSYM(Finish) GLSYM(Flush) GLSYM(GetError) GLSYM(Enable) GLSYM(Disable) GLSYM(EnableClientState)
    GLSYM(BlendFunc) GLSYM(Viewport) GLSYM(MatrixMode) GLSYM(LoadIdentity) GLSYM(Orthof) GLSYM(ClearColor)
    GLSYM(Clear) GLSYM(VertexPointer) GLSYM(TexCoordPointer) GLSYM(DrawArrays) GLSYM(DrawElements) GLSYM(GenTextures)
    GLSYM(BindTexture) GLSYM(TexParameteri) GLSYM(TexImage2D) GLSYM(TexSubImage2D) GLSYM(GenRenderbuffersOES)
    GLSYM(BindRenderbufferOES) GLSYM(GenFramebuffersOES) GLSYM(BindFramebufferOES)
    GLSYM(FramebufferRenderbufferOES) GLSYM(CheckFramebufferStatusOES)
#undef GLSYM
    Object (*allocate_class)(Object, const char *, u32) = dlsym(runtime, "objc_allocateClassPair");
    int (*add_method)(Object, Selector, void *, const char *) = dlsym(runtime, "class_addMethod");
    void (*register_class)(Object) = dlsym(runtime, "objc_registerClassPair");
    Object (*class_of)(Object) = dlsym(runtime, "object_getClass");
    Object view_class = allocate_class(get_class("UIView"), "LTBenchGLView", 0);
    if (!class_of || !view_class || !add_method(class_of(view_class), get_selector("layerClass"), gl_layer_class, "#@:")) {
        gl_problem = "no GL view class";
        return;
    }
    register_class(view_class);
    Rect full = {{0, 0}, {GL_W, GL_H}};
    gl_view = ((Object (*)(Object, Selector, Rect))send)(call0(get_class("LTBenchGLView"), "alloc"),
              get_selector("initWithFrame:"), full);
    ((void (*)(Object, Selector, int))send)(gl_view, get_selector("setOpaque:"), 1);
    call1(under, "addSubview:", gl_view);
    gl_context = ((Object (*)(Object, Selector, u32))send)(call0(get_class("EAGLContext"), "alloc"),
                 get_selector("initWithAPI:"), 1);
    if (!gl_context || !make_current(gl_context)) { gl_problem = "no EAGL context"; return; }
    gl.GenRenderbuffersOES(1, &gl_renderbuffer);
    gl.BindRenderbufferOES(GL_RENDERBUFFER_OES, gl_renderbuffer);
    if (!((signed char (*)(Object, Selector, u32, Object))send)(gl_context, get_selector("renderbufferStorage:fromDrawable:"),
                                                        GL_RENDERBUFFER_OES, call0(gl_view, "layer"))) {
        gl_problem = "no drawable";
        return;
    }
    gl.GenFramebuffersOES(1, &gl_framebuffer);
    gl.BindFramebufferOES(GL_FRAMEBUFFER_OES, gl_framebuffer);
    gl.FramebufferRenderbufferOES(GL_FRAMEBUFFER_OES, GL_COLOR_ATTACHMENT0_OES, GL_RENDERBUFFER_OES, gl_renderbuffer);
    if (gl.CheckFramebufferStatusOES(GL_FRAMEBUFFER_OES) != GL_FRAMEBUFFER_COMPLETE_OES) { gl_problem = "framebuffer incomplete"; return; }
    for (u32 i = 0; i < SPRITES; i++) {
        float x = (float)(i % 8) * 40 + 0, y = (float)(i / 8) * 40 + 80;
        float q[8] = {x, y, x + 32, y, x, y + 32, x + 32, y + 32};
        for (u32 k = 0; k < 8; k++) sprite_xy[i][k] = q[k];
    }
    for (u32 i = 0; i < sizeof(upload_pixels); i++) upload_pixels[i] = (unsigned char)(i * 7);
    for (u32 i = 0; i < MESH_VERTS; i++) {   /* pieces of one-pixel quads, so fill costs nothing */
        u32 k = i % MESH_PIECE;
        mesh[i].x = (float)(10 + k % 25); mesh[i].y = (float)(10 + k / 25); mesh[i].z = 0;
        mesh[i].u = (int)(k % 25) << 12; mesh[i].v = (int)(k / 25) << 16;
    }
    for (u32 p = 0; p < MESH_PIECES; p++) {
        u32 base = p * MESH_PIECE, n = 0;
        for (u32 q = 0; q < 24; q++) {
            u32 a = base + q, b = a + 1, c = a + 25, d = c + 1;
            unsigned short t[6] = {a, b, c, b, d, c};
            for (u32 k = 0; k < 6; k++) mesh_index[p][n++] = t[k];
        }
    }
    gl.GenTextures(2, gl_texture);
    for (u32 t = 0; t < 2; t++) {
        gl.BindTexture(GL_TEXTURE_2D, gl_texture[t]);
        gl.TexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        gl.TexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        gl.TexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, UPLOAD_SIDE, UPLOAD_SIDE, 0, GL_RGBA, GL_UNSIGNED_BYTE, upload_pixels);
    }
    gl.Viewport(0, 0, GL_W, GL_H);
    gl.MatrixMode(GL_PROJECTION); gl.LoadIdentity(); gl.Orthof(0, GL_W, 0, GL_H, -1, 1);
    gl.MatrixMode(GL_MODELVIEW); gl.LoadIdentity();
    gl.EnableClientState(GL_VERTEX_ARRAY); gl.EnableClientState(GL_TEXTURE_COORD_ARRAY);
    gl.Enable(GL_TEXTURE_2D); gl.Enable(GL_BLEND); gl.BlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    gl.Finish();
    make_current(0);
    gl_problem = 0;
}

static int gl_present(void)
{
    gl.BindRenderbufferOES(GL_RENDERBUFFER_OES, gl_renderbuffer);
    return ((signed char (*)(Object, Selector, u32))send)(gl_context, get_selector("presentRenderbuffer:"), GL_RENDERBUFFER_OES) != 0;
}
/* The bench thread, before and after the GL tests. After: a plain frame, so the results label
 * (over the GL view) reads as it does without GL. */
static int gl_begin(void)
{
    return !gl_problem && make_current(gl_context);
}
static void gl_end(void)
{
    if (gl_problem) return;
    gl.ClearColor(0.25f, 0.25f, 0.25f, 1); gl.Clear(GL_COLOR_BUFFER_BIT);
    gl_present();
    gl.Finish();
    make_current(0);
}

/* glGetError: a call that returns a value, so the guest waits for the host each time. */
static u64 b_gl_get(u32 n)
{
    if (!gl_begin()) return 0;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) sink += gl.GetError();
    return now_ns() - t;
}
/* A state change with no reply (the shim may queue it): glEnable/glDisable(GL_BLEND) in turn. */
static u64 b_gl_state(u32 n)
{
    if (!gl_begin()) return 0;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) (i & 1 ? gl.Enable : gl.Disable)(GL_BLEND);
    gl.Enable(GL_BLEND);
    gl.Finish();
    return now_ns() - t;
}
/* The same textured 32 x 32 quad drawn again: the draw call alone. */
static u64 b_gl_draw(u32 n)
{
    if (!gl_begin()) return 0;
    gl.BindTexture(GL_TEXTURE_2D, gl_texture[0]);
    gl.VertexPointer(2, GL_FLOAT, 0, sprite_xy[0]);
    gl.TexCoordPointer(2, GL_FLOAT, 0, sprite_uv);
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) gl.DrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    gl.Finish();
    return now_ns() - t;
}
/* One sprite as a 2D game of the era draws it: bind its texture, point at its vertices and
 * texture coordinates, draw (Jetpack Joyride's title makes ~230 such draws a frame). */
static u64 b_gl_sprite(u32 n)
{
    if (!gl_begin()) return 0;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        gl.BindTexture(GL_TEXTURE_2D, gl_texture[i & 1]);
        gl.VertexPointer(2, GL_FLOAT, 0, sprite_xy[i % SPRITES]);
        gl.TexCoordPointer(2, GL_FLOAT, 0, sprite_uv);
        gl.DrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    }
    gl.Finish();
    return now_ns() - t;
}
/* One piece of a big level mesh: glDrawElements of 144 indices that reach 50 of the array's 16384
 * vertices, a different piece each time (Asphalt 5's races: ~125 a frame, ~70 vertices each). */
static u64 b_gl_mesh(u32 n)
{
    if (!gl_begin()) return 0;
    gl.BindTexture(GL_TEXTURE_2D, gl_texture[0]);
    gl.VertexPointer(3, GL_FLOAT, sizeof(mesh[0]), &mesh[0].x);
    gl.TexCoordPointer(2, GL_FIXED, sizeof(mesh[0]), &mesh[0].u);
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++)
        gl.DrawElements(GL_TRIANGLES, MESH_INDICES, GL_UNSIGNED_SHORT, mesh_index[(i * 97) % MESH_PIECES]);
    gl.Finish();
    return now_ns() - t;
}
/* glTexSubImage2D of 16 KiB: texture streaming (guest memory to a host texture). */
static u64 b_gl_upload(u32 n)
{
    if (!gl_begin()) return 0;
    gl.BindTexture(GL_TEXTURE_2D, gl_texture[1]);
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        upload_pixels[i & 4095] = (unsigned char)i;
        gl.TexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, UPLOAD_SIDE, UPLOAD_SIDE, GL_RGBA, GL_UNSIGNED_BYTE, upload_pixels);
    }
    gl.Finish();
    return now_ns() - t;
}
/* A full-screen frame: clear, then -presentRenderbuffer: (the host reads the frame back and
 * hands it to CoreAnimation). Includes any wait for the display, which a game pays too. */
static u64 b_gl_present(u32 n)
{
    if (!gl_begin()) return 0;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        gl.ClearColor((float)(i & 1), 0.2f, 0.4f, 1);
        gl.Clear(GL_COLOR_BUFFER_BIT);
        if (!gl_present()) return 0;
    }
    gl.Finish();
    u64 ns = now_ns() - t;
    gl_end();
    return ns;
}

/* ----------------------------------------------------------- benchmarks --- */

/* Plain integer work: the translated-code baseline everything else is measured against. */
static u64 b_loop(u32 n)
{
    u64 t = now_ns();
    u32 x = 1;
    for (u32 i = 0; i < n; i++) x = x * 1664525u + 1013904223u + (x >> 7);
    sink = x;
    return now_ns() - t;
}

/* Calls through a function pointer cycling over four targets: indirect branches,
 * each ending in a return. */
static u32 f0(u32 x) { return x + 1; }
static u32 f1(u32 x) { return x ^ 3; }
static u32 f2(u32 x) { return x * 3; }
static u32 f3(u32 x) { return x - 7; }
static u32 (*volatile table[4])(u32) = {f0, f1, f2, f3};
static u64 b_icall(u32 n)
{
    u64 t = now_ns();
    u32 x = 0;
    for (u32 i = 0; i < n; i++) x = table[i & 3](x);
    sink = x;
    return now_ns() - t;
}

/* Objective-C dispatch: objc_msgSend to a trivial method (+[NSObject class]), a method-cache hit
 * ending in an indirect branch to the method. */
static u64 b_objc_send(u32 n)
{
    Object cls = get_class("NSObject");
    Selector s_class = get_selector("class");
    Object (*msg)(Object, Selector) = (Object (*)(Object, Selector))send;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) cls = msg(cls, s_class);
    sink = (u32)cls;
    return now_ns() - t;
}

/* One turn of a game loop that polls its run loop: an autorelease pool around
 * [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode beforeDate:[NSDate distantPast]].
 * This thread's run loop has no sources, so each turn is message sends, CoreFoundation's mode
 * lookup and the empty-mode check. Battle Bears: Zombies! spends about half its guest time in
 * this code. */
static u64 b_runloop(u32 n)
{
    void *foundation = dlopen("/System/Library/Frameworks/Foundation.framework/Foundation", 2);
    Object *default_mode = foundation ? dlsym(foundation, "NSDefaultRunLoopMode") : 0;
    if (!default_mode || !*default_mode) return 0;
    Object pool_class = get_class("NSAutoreleasePool"), loop_class = get_class("NSRunLoop");
    Object past = call0(get_class("NSDate"), "distantPast");
    Selector s_alloc = get_selector("alloc"), s_init = get_selector("init"), s_release = get_selector("release");
    Selector s_current = get_selector("currentRunLoop"), s_run = get_selector("runMode:beforeDate:");
    Object (*msg)(Object, Selector) = (Object (*)(Object, Selector))send;
    int (*run_mode)(Object, Selector, Object, Object) = (int (*)(Object, Selector, Object, Object))send;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        Object pool = msg(msg(pool_class, s_alloc), s_init);
        run_mode(msg(loop_class, s_current), s_run, *default_mode, past);
        msg(pool, s_release);
    }
    return now_ns() - t;
}

/* Recursion: returns to many different call sites. */
static u32 fib(u32 k) { return k < 2 ? k : fib(k - 1) + fib(k - 2); }
static u64 b_fib(u32 n)   /* n calls, approximately: fib(k) makes 2*fib(k+1)-1 */
{
    u32 k = 2, calls = 1;
    while (calls < n) { k++; calls = 2 * fib(k + 1) - 1; }
    u64 t = now_ns();
    sink = fib(k);
    return (now_ns() - t) * n / calls;
}

static char *buffer_a, *buffer_b;
#define COPY_BYTES (256 * 1024)
static u64 b_memcpy(u32 n)   /* n KiB */
{
    u64 t = now_ns();
    for (u32 done = 0; done < n; done += COPY_BYTES / 1024) memcpy(buffer_b, buffer_a, COPY_BYTES);
    return now_ns() - t;
}

/* A system call that does nothing: SWI, the exception entry and return, kernel entry. */
static u64 b_syscall(u32 n)
{
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) sink = (u32)getppid();
    return now_ns() - t;
}

/* The time base itself (a Mach trap on this OS). */
static u64 b_time(u32 n)
{
    u64 t = now_ns(), x = 0;
    for (u32 i = 0; i < n; i++) x += mach_absolute_time();
    sink = (u32)x;
    return now_ns() - t;
}

/* Ping-pong one byte over two pipes: n round trips, two switches each. Between
 * threads the address space stays; between processes every switch changes ASID. */
static int ping[2], pong[2];
static u32 rounds_wanted, touch_pages;
static char *touch_area;
static void touch(void)
{
    for (u32 p = 0; p < touch_pages; p++) touch_area[p * 4096 + (p & 63) * 64]++;
}
static void echo(void)
{
    char c;
    for (u32 i = 0; i < rounds_wanted; i++) {
        if (read(ping[0], &c, 1) != 1) break;
        touch();
        write(pong[1], &c, 1);
    }
}
static void *echo_thread(void *unused) { (void)unused; echo(); return 0; }
static u64 pingpong(u32 n)
{
    char c = 'x';
    void *thread = 0;
    if (pipe(ping) || pipe(pong)) return 0;
    rounds_wanted = n;
    pthread_create(&thread, 0, echo_thread, 0);
    write(ping[1], &c, 1); read(pong[0], &c, 1);  /* both sides running */
    u64 t = now_ns();
    for (u32 i = 1; i < n; i++) { write(ping[1], &c, 1); touch(); read(pong[0], &c, 1); }
    u64 e = (now_ns() - t) * n / (n - 1);
    pthread_join(thread, 0);
    close(ping[0]); close(ping[1]); close(pong[0]); close(pong[1]);
    return e;
}
static u64 b_thread_switch(u32 n) { touch_pages = 0; return pingpong(n); }
/* The same, each side touching 64 pages per turn: TLB refills without an address-space
 * switch, for comparison with rpc-64p. */
static u64 b_thread_switch_touch(u32 n) { touch_pages = 64; return pingpong(n); }

/* A Mach RPC to launchd and back (bootstrap_look_up of a name it serves): two
 * address-space switches plus launchd's work. An app cannot fork (the sandbox denies
 * it), and talking to other processes this way is what iOS apps do all the time. */
static u64 rpc(u32 n)
{
    u32 port;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        if (bootstrap_look_up(bootstrap_port, "com.apple.system.notification_center", &port)) return 0;
        touch();
    }
    return now_ns() - t;
}
static u64 b_rpc(u32 n) { touch_pages = 0; return rpc(n); }
/* The same with 64 of this process's pages written between calls: what an address-space
 * switch costs this process in TLB refills (QEMU's TLB is not ASID-tagged). */
static u64 b_rpc_touch(u32 n) { touch_pages = 64; return rpc(n); }

static u64 b_touch(u32 n) { touch_pages = 64; u64 t = now_ns(); for (u32 i = 0; i < n; i++) touch(); return now_ns() - t; }

/* Anonymous memory: n page faults (mapping 256 pages, writing each, unmapping). */
static u64 b_fault(u32 n)
{
    u64 total = 0;
    for (u32 done = 0; done < n; done += 256) {
        u64 t = now_ns();
        char *p = mmap(0, 256 * 4096, 3, 0x1002, -1, 0);
        if (p == (char *)-1) return 0;
        for (u32 i = 0; i < 256; i++) p[i * 4096] = 1;
        munmap(p, 256 * 4096);
        total += now_ns() - t;
    }
    return total;
}

/* The audio converter uses STMIA r1!, {r8,r9} even when its output is only
 * 2-byte aligned. ARM1176 traps on that instruction; iOS emulates the store
 * and returns. Time whole loops with the original faults/handler intact.
 * Both cases cycle through the same 2 KiB, pre-touched before timing. */
static unsigned char stm_area[2064] __attribute__((aligned(4096)));
static u64 stm_pairs(u32 n, u32 offset)
{
    unsigned char *start = stm_area + 8 + offset;
    unsigned char *end = start + 2048;
    register unsigned char *dst __asm__("r1") = start;
    u32 left = n;
    if (!n) return 0;
    memset(stm_area, 0xcc, sizeof(stm_area));
    u64 t = now_ns();
    __asm__ volatile(
        "mov r8, #0x5a\n\t"
        "mov r9, #0xa5\n\t"
        "1: stmia %0!, {r8, r9}\n\t"
        "cmp %0, %3\n\t"
        "moveq %0, %2\n\t"
        "subs %1, %1, #1\n\t"
        "bne 1b"
        : "+&r"(dst), "+&r"(left)
        : "r"(start), "r"(end)
        : "r8", "r9", "cc", "memory");
    u64 elapsed = now_ns() - t;
    /* Check bytes, guards and writeback after timing, using byte accesses
     * so the check itself never adds an unaligned word load. */
    u32 bytes = (n < 256 ? n : 256) * 8;
    if (dst != start + (n % 256) * 8) return 0;
    for (u32 i = 0; i < sizeof(stm_area); i++) {
        unsigned char expected = 0xcc;
        if (i >= 8 + offset && i < 8 + offset + bytes) {
            u32 lane = (i - 8 - offset) & 7;
            expected = lane == 0 ? 0x5a : lane == 4 ? 0xa5 : 0;
        }
        if (stm_area[i] != expected) return 0;
    }
    return elapsed;
}
static u64 b_stm_aligned(u32 n) { return stm_pairs(n, 0); }
static u64 b_stm_unaligned(u32 n) { return stm_pairs(n, 2); }

/* mmap + munmap of one page, never touched: the VM system calls alone. */
static u64 b_mmap(u32 n)
{
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        void *p = mmap(0, 4096, 3, 0x1002, -1, 0);
        if (p == (void *)-1) return 0;
        munmap(p, 4096);
    }
    return now_ns() - t;
}

/* Dependent loads chasing a pointer through `pages` pages in a scrambled order:
 * within QEMU's TLB reach, then well beyond it. */
static char *chase_area;
static u64 chase(u32 n, u32 pages)
{
    u32 order[1024];
    for (u32 i = 0; i < pages; i++) order[i] = i;
    u32 seed = 12345;
    for (u32 i = pages - 1; i > 0; i--) {
        seed = seed * 1103515245u + 12345u;
        u32 j = (seed >> 8) % (i + 1), x = order[i];
        order[i] = order[j]; order[j] = x;
    }
    for (u32 i = 0; i < pages; i++) {
        char *from = chase_area + order[i] * 4096 + (order[i] & 63) * 64;
        u32 k = order[(i + 1) % pages];
        *(char **)from = chase_area + k * 4096 + (k & 63) * 64;
    }
    char *p = chase_area + order[0] * 4096 + (order[0] & 63) * 64;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) p = *(char **)p;
    sink = (u32)p;
    return now_ns() - t;
}
static u64 b_tlb32(u32 n) { return chase(n, 32); }
static u64 b_tlb1024(u32 n) { return chase(n, 1024); }

/* fork, the child exits at once, wait: address-space creation and teardown. */
static u64 b_fork(u32 n)   /* 0: not allowed (an app's sandbox denies fork) */
{
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        int pid = fork();
        if (pid < 0) return 0;
        if (pid == 0) _exit(0);
        waitpid(pid, 0, 0);
    }
    return now_ns() - t;
}

/* Storage, as a game of the era loads: a file in the app's Documents/ written and read with the guest's
 * buffer cache bypassed (F_NOCACHE), so every byte crosses the emulated flash controller.
 * fs-write: 64 KiB writes, then fsync. fs-read: 64 KiB reads in order. fs-read-16k: 16 KiB at
 * scattered offsets, as an engine pulls textures out of a pack file. Per KiB for the first two. */
#define IO_FILE_KIB 8192
#define IO_CHUNK (64 * 1024)
#define O_RDONLY 0
#define O_RDWR 2
#define O_CREAT 0x200
#define O_TRUNC 0x400
#define F_NOCACHE 48
static char io_path[256], io_path_w[256], *io_buffer;
static int io_ready, io_w_ready;
static int io_open_at(const char *path, int flags);
static int io_open(int flags)
{
    if (!io_path[0]) {
        const char *home = getenv("HOME");
        u32 n = 0;
        if (!home) return -1;
        while (home[n] && n < sizeof(io_path) - 20) io_path[n] = home[n], n++;
        const char *tail = "/Documents/ltb-io.dat";
        for (u32 i = 0; tail[i]; i++) io_path[n++] = tail[i];
        io_path[n] = 0;
    }
    if (!io_buffer && !(io_buffer = malloc(IO_CHUNK))) return -1;
    return io_open_at(io_path, flags);
}
static int io_open_at(const char *path, int flags)
{
    int fd = open(path, flags, 0644);
    if (fd >= 0) fcntl(fd, F_NOCACHE, 1);
    return fd;
}
static u64 b_fs_write(u32 n)   /* n KiB */
{
    u64 t = now_ns();
    int fd = io_open(O_RDWR | O_CREAT | O_TRUNC);
    if (fd < 0) return 0;
    for (u32 done = 0; done < n; done += IO_CHUNK / 1024) {
        for (u32 i = 0; i < IO_CHUNK; i += 4096) io_buffer[i] = (char)(done + i);
        if (write(fd, io_buffer, IO_CHUNK) != IO_CHUNK) { close(fd); return 0; }
    }
#ifdef IO_FULLSYNC   /* durability tests: F_FULLFSYNC, the only sync HFS+ makes survive power loss */
    if (fcntl(fd, 51, 0) != 0) { close(fd); return 0; }
#else
    if (fsync(fd) != 0) { close(fd); return 0; }
#endif
    close(fd);
    io_ready = n >= IO_FILE_KIB;
    return now_ns() - t;
}
static int io_prepare(void) { return io_ready || b_fs_write(IO_FILE_KIB) != 0; }
static u64 b_fs_read(u32 n)    /* n KiB, at most the file */
{
    if (!io_prepare()) return 0;
    u64 t = now_ns();
    int fd = io_open(O_RDONLY);
    if (fd < 0) return 0;
    for (u32 done = 0; done < n; done += IO_CHUNK / 1024) {
        if (read(fd, io_buffer, IO_CHUNK) != IO_CHUNK) { close(fd); return 0; }
        for (u32 i = 0; i < IO_CHUNK; i += 4096)   /* what fs-write wrote: a corrupt write path fails the test */
            if (io_buffer[i] != (char)(done + i)) { close(fd); return 0; }
    }
    close(fd);
    return now_ns() - t;
}
/* The file a previous launch's fs-write left, read and checked without writing it first: survives a reboot? */
static u64 b_fs_check(u32 n)
{
    io_ready = 1;
    return b_fs_read(n);
}
static u64 b_fs_read16k(u32 n)
{
    if (!io_prepare()) return 0;
    int fd = io_open(O_RDONLY);
    if (fd < 0) return 0;
    u32 seed = 2468;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        seed = seed * 1103515245u + 12345u;
        long long at = (long long)((seed >> 8) % (IO_FILE_KIB / 16)) * 16384;
        if (pread(fd, io_buffer, 16384, at) != 16384) { close(fd); return 0; }
    }
    u64 elapsed = now_ns() - t;
    close(fd);
    return elapsed;
}

/* Write patterns beyond fs-write. fs-write-big: 16 MiB uncached in 256 KiB writes, then fsync (a sustained
 * stream, past any write buffer). fs-write-cached: 8 MiB through the buffer cache in 64 KiB writes, then fsync
 * (how an app saves). fs-rand-4k: 4 KiB uncached pwrite at a scattered 4 KiB slot of the 8 MiB file, one fsync at
 * the end. fs-fsync-4k / fs-fullfsync-4k: 4 KiB pwrite then fsync / F_FULLFSYNC each time (a database commit; the
 * full one also asks the flash to flush). fs-create: create, write 16 KiB, close a new file (an unpack's small
 * files); the files are removed afterwards, untimed. */
#define IO_BIG_KIB 16384
#define IO_BIG_CHUNK (256 * 1024)
#define F_FULLFSYNC 51
static char *io_big;
/* The write tests' own file (Documents/ltb-w.dat), so fs-check's file keeps what fs-write wrote. */
static int io_open_w(int flags)
{
    if (!io_path_w[0]) {
        int fd = io_open(O_RDONLY | O_CREAT);   /* fills io_path and io_buffer */
        u32 n = 0;
        if (fd < 0) return -1;
        close(fd);
        while (io_path[n]) io_path_w[n] = io_path[n], n++;
        io_path_w[n - 6] = 'w'; io_path_w[n - 5] = '.'; io_path_w[n - 4] = 'd'; io_path_w[n - 3] = 'a';
        io_path_w[n - 2] = 't'; io_path_w[n - 1] = 0;   /* ltb-io.dat -> ltb-w.dat */
    }
    return io_open_at(io_path_w, flags);
}
static u64 io_write_file(u32 n, u32 chunk, int nocache)
{
    if (!io_big && !(io_big = malloc(IO_BIG_CHUNK))) return 0;
    u64 t = now_ns();
    int fd = io_open_w(O_RDWR | O_CREAT | O_TRUNC);
    if (fd < 0) return 0;
    if (!nocache) fcntl(fd, F_NOCACHE, 0);
    for (u32 done = 0; done < n; done += chunk / 1024) {
        for (u32 i = 0; i < chunk; i += 4096) io_big[i] = (char)(done + i);
        if (write(fd, io_big, chunk) != (long)chunk) { close(fd); return 0; }
    }
    if (fsync(fd) != 0) { close(fd); return 0; }
    close(fd);
    io_w_ready = n >= IO_FILE_KIB;
    return now_ns() - t;
}
static int io_w_prepare(void) { return io_w_ready || io_write_file(IO_FILE_KIB, IO_CHUNK, 1) != 0; }
static u64 b_fs_write_big(u32 n) { return io_write_file(n, IO_BIG_CHUNK, 1); }
static u64 b_fs_write_cached(u32 n) { return io_write_file(n, IO_CHUNK, 0); }
static u64 b_fs_rand4k(u32 n)
{
    if (!io_w_prepare()) return 0;
    int fd = io_open_w(O_RDWR);
    if (fd < 0) return 0;
    u32 seed = 1357;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        seed = seed * 1103515245u + 12345u;
        u32 slot = (seed >> 8) % (IO_FILE_KIB / 4);
        for (u32 k = 0; k < 4096; k += 512) io_buffer[k] = (char)(slot * 4096 + k);
        if (pwrite(fd, io_buffer, 4096, (long long)slot * 4096) != 4096) { close(fd); return 0; }
    }
    if (fsync(fd) != 0) { close(fd); return 0; }
    u64 elapsed = now_ns() - t;
    close(fd);
    return elapsed;
}
static u64 io_sync4k(u32 n, int full)
{
    if (!io_w_prepare()) return 0;
    int fd = io_open_w(O_RDWR);
    if (fd < 0) return 0;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        io_buffer[0] = (char)i;
        if (pwrite(fd, io_buffer, 4096, (long long)(i % 64) * 4096) != 4096) { close(fd); return 0; }
        if ((full ? fcntl(fd, F_FULLFSYNC, 0) : fsync(fd)) != 0) { close(fd); return 0; }
    }
    u64 elapsed = now_ns() - t;
    close(fd);
    return elapsed;
}
static u64 b_fs_fsync4k(u32 n) { return io_sync4k(n, 0); }
static u64 b_fs_fullfsync4k(u32 n) { return io_sync4k(n, 1); }
static u64 b_fs_create(u32 n)
{
    char path[300];
    u32 len = 0;
    int probe = io_open_w(O_RDONLY | O_CREAT);   /* fills io_path_w and io_buffer */
    if (probe < 0) return 0;
    close(probe);
    while (io_path_w[len] && len < 250) path[len] = io_path_w[len], len++;
    if (!len || !io_buffer) return 0;
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) {
        u32 m = len;
        path[m++] = '.'; path[m++] = (char)('a' + i / 676 % 26); path[m++] = (char)('a' + i / 26 % 26);
        path[m++] = (char)('a' + i % 26); path[m] = 0;
        int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) return 0;
        for (u32 k = 0; k < 4; k++) if (write(fd, io_buffer, 4096) != 4096) { close(fd); return 0; }
        close(fd);
    }
    u64 elapsed = now_ns() - t;
    for (u32 i = 0; i < n; i++) {
        u32 m = len;
        path[m++] = '.'; path[m++] = (char)('a' + i / 676 % 26); path[m++] = (char)('a' + i / 26 % 26);
        path[m++] = (char)('a' + i % 26); path[m] = 0;
        unlink(path);
    }
    return elapsed;
}

/* usleep(1000): the timer interrupt and wakeup path. Reported as the actual sleep. */
static u64 b_sleep(u32 n)
{
    u64 t = now_ns();
    for (u32 i = 0; i < n; i++) usleep(1000);
    return now_ns() - t;
}

static const Test tests[] = {
    {"loop", "integer loop iteration", b_loop, 4000000},
    {"vfp-mla", "VFP2 non-fused multiply/add", b_vfp_mla, 2000000},
    {"icall", "indirect call + return", b_icall, 2000000},
    {"fib", "recursive call + return", b_fib, 500000},
    {"objc-send", "objc_msgSend, +[NSObject class]", b_objc_send, 500000},
    {"runloop", "pool + runMode:beforeDate: distantPast (no sources) + release", b_runloop, 20000},
    {"memcpy", "KiB copied", b_memcpy, 20480},
    {"syscall", "getppid()", b_syscall, 50000},
    {"time", "mach_absolute_time()", b_time, 50000},
    {"thread-switch", "round trip, 2 thread switches", b_thread_switch, 4000},
    {"thread-switch-64p", "round trip touching 64 pages per side", b_thread_switch_touch, 2000},
    {"rpc", "Mach RPC to launchd and back", b_rpc, 4000},
    {"rpc-64p", "Mach RPC, 64 pages written between calls", b_rpc_touch, 2000},
    {"touch-64p", "writing 64 pages, no switch", b_touch, 20000},
    {"fault", "anonymous page fault (map, write, unmap)", b_fault, 2048},
    {"stm-aligned", "8-byte STMIA, 4-byte aligned", b_stm_aligned, 32768},
    {"stm-unaligned", "8-byte STMIA, 2-mod4 output, architectural trap", b_stm_unaligned, 32768},
    {"mmap", "mmap + munmap, untouched", b_mmap, 4000},
    {"tlb-32p", "dependent load, 32-page working set", b_tlb32, 2000000},
    {"tlb-1024p", "dependent load, 1024-page working set", b_tlb1024, 2000000},
    {"fork", "fork + exit + wait", b_fork, 40},
    {"sleep", "usleep(1000), actual", b_sleep, 100},
    {"fs-write", "KiB written uncached (64 KiB writes, then fsync)", b_fs_write, IO_FILE_KIB},
    {"fs-read", "KiB read uncached (64 KiB reads in order)", b_fs_read, IO_FILE_KIB},
    {"fs-check", "KiB of a previous launch's fs-write file, read and checked", b_fs_check, IO_FILE_KIB},
    {"fs-read-16k", "uncached 16 KiB pread at a scattered offset", b_fs_read16k, 256},
    {"fs-write-big", "KiB written uncached (256 KiB writes, 16 MiB, then fsync)", b_fs_write_big, IO_BIG_KIB},
    {"fs-write-cached", "KiB written through the cache (64 KiB writes, then fsync)", b_fs_write_cached, IO_FILE_KIB},
    {"fs-rand-4k", "uncached 4 KiB pwrite at a scattered slot, one fsync at the end", b_fs_rand4k, 512},
    {"fs-fsync-4k", "4 KiB pwrite + fsync", b_fs_fsync4k, 64},
    {"fs-fullfsync-4k", "4 KiB pwrite + F_FULLFSYNC", b_fs_fullfsync4k, 64},
    {"fs-create", "create, write 16 KiB, close a new file", b_fs_create, 200},
    {"gl-get", "glGetError()", b_gl_get, 2000},
    {"gl-state", "glEnable/glDisable", b_gl_state, 20000},
    {"gl-draw", "glDrawArrays, same 32x32 textured quad", b_gl_draw, 5000},
    {"gl-sprite", "bind texture + 2 array pointers + draw", b_gl_sprite, 5000},
    {"gl-mesh", "glDrawElements, 50-vertex piece of a 16384-vertex mesh", b_gl_mesh, 2000},
    {"gl-upload", "glTexSubImage2D 64x64 RGBA (16 KiB)", b_gl_upload, 400},
    {"gl-present", "full-screen clear + presentRenderbuffer", b_gl_present, 60},
};
#define NTESTS (sizeof(tests) / sizeof(tests[0]))

static char summary[NTESTS * 48 + 64];
static u32 summary_len;
static int running;

static void run_all(void)
{
    static u32 pass;
    Line l = {{0}, 0};
    u64 started = now_ns();
    pass++;
    summary_len = 0;
    put(&l, "[ltbench] start pass "); putu(&l, pass); put(&l, " rounds "); putu(&l, ROUNDS); put(&l, "\n");
    emit(l.b);
    for (u32 i = 0; i < NTESTS; i++) {
        u64 r[ROUNDS];
        const Test *t = &tests[i];
#ifdef ONLY   /* Comma-separated exact names, e.g. -DONLY=\"syscall,fault\". */
        const char *b = ONLY;
        int selected = 0;
        while (*b) {
            const char *a = t->name;
            while (*a && *a == *b) a++, b++;
            if (!*a && (!*b || *b == ',')) selected = 1;
            while (*b && *b != ',') b++;
            if (*b) b++;
        }
        if (!selected) continue;
#endif
        u64 ns = 1;
        for (u32 k = 0; k < ROUNDS && ns; k++) {
            ns = t->run(t->n);
            r[k] = ns * 10 / t->n;   /* tenths of a ns per op */
        }
        if (!ns) {
            l.n = 0;
            put(&l, "[ltbench] "); put(&l, t->name); put(&l, " unavailable\n");
            emit(l.b);
            continue;
        }
        for (u32 a = 0; a < ROUNDS; a++) for (u32 b = a + 1; b < ROUNDS; b++)
            if (r[b] < r[a]) { u64 x = r[a]; r[a] = r[b]; r[b] = x; }
        l.n = 0;
        put(&l, "[ltbench] "); put(&l, t->name); put(&l, " best "); putd(&l, r[0]);
        put(&l, " median "); putd(&l, r[ROUNDS / 2]); put(&l, " ns/op ("); put(&l, t->what);
        put(&l, ", n="); putu(&l, t->n); put(&l, ")\n");
        emit(l.b);
        Line s = {{0}, 0};
        put(&s, t->name); put(&s, " "); putd(&s, r[0]); put(&s, "\n");
        for (u32 c = 0; c < s.n && summary_len < sizeof(summary) - 1; c++) summary[summary_len++] = s.b[c];
        summary[summary_len] = 0;
    }
    l.n = 0;
    put(&l, "[ltbench] done pass "); putu(&l, pass); put(&l, " in "); putu(&l, (now_ns() - started) / 1000000); put(&l, " ms\n");
    emit(l.b);
}

/* ------------------------------------------------------------------ app --- */

static Object string(const char *text) {
    return ((Object (*)(Object, Selector, const char *))send)(get_class("NSString"),
            get_selector("stringWithUTF8String:"), text);
}
static void show(const char *text, const char *color) {
    call1(label, "setText:", string(text));
    call1(window, "setBackgroundColor:", call0(get_class("UIColor"), color));
}

static void finished(Object self, Selector selector, Object unused) {
    (void)self; (void)selector; (void)unused;
    show(summary, "darkGrayColor");
    running = 0;
}
static void *bench_thread(void *unused) {
    (void)unused;
    Object pool = call0(call0(get_class("NSAutoreleasePool"), "alloc"), "init");
    run_all();
    ((void (*)(Object, Selector, Selector, Object, int))send)(delegate_object,
        get_selector("performSelectorOnMainThread:withObject:waitUntilDone:"), get_selector("benchFinished:"), 0, 0);
    call0(pool, "drain");
    return 0;
}
static void start(void) {
    void *thread;
    if (running) return;
    running = 1;
    show("LT Bench\nrunning...\n\nresults go to the engine log", "purpleColor");
    pthread_create(&thread, 0, bench_thread, 0);
}
static void tapped(Object self, Selector selector, Object sender) {
    (void)self; (void)selector; (void)sender;
    start();
}

static void launched(Object self, Selector selector, Object application) {
    (void)selector; (void)application;
    Rect full = {{0, 0}, {320, 480}}, text = {{8, 24}, {304, 448}};
    delegate_object = self;
    emit("[ltbench] launched\n");
    window = ((Object (*)(Object, Selector, Rect))send)(call0(get_class("UIWindow"), "alloc"),
              get_selector("initWithFrame:"), full);
    label = ((Object (*)(Object, Selector, Rect))send)(call0(get_class("UILabel"), "alloc"),
             get_selector("initWithFrame:"), text);
    call1(label, "setTextColor:", call0(get_class("UIColor"), "whiteColor"));
    call1(label, "setBackgroundColor:", call0(get_class("UIColor"), "clearColor"));
    ((void (*)(Object, Selector, int))send)(label, get_selector("setNumberOfLines:"), 0);
    Object font = ((Object (*)(Object, Selector, float))send)(get_class("UIFont"),
                  get_selector("systemFontOfSize:"), 14.0f);
    call1(label, "setFont:", font);
    gl_setup(window);   /* under the label */
    if (gl_problem) { Line g = {{0}, 0}; put(&g, "[ltbench] gl: "); put(&g, gl_problem); put(&g, "\n"); emit(g.b); }
    call1(window, "addSubview:", label);
    Object button = ((Object (*)(Object, Selector, Rect))send)(call0(get_class("UIControl"), "alloc"),
                    get_selector("initWithFrame:"), full);
    ((void (*)(Object, Selector, Object, Selector, u32))send)(button,
        get_selector("addTarget:action:forControlEvents:"), self, get_selector("benchTapped:"), 1u << 6);
    call1(window, "addSubview:", button);
    call0(window, "makeKeyAndVisible");
#ifdef SYMS   /* build.sh with LTB_CFLAGS='-DSYMS=0x33989a1c,0x3386699a': name guest addresses from a profile */
    {
        static const u32 addrs[] = { SYMS };
        typedef struct { const char *fname; void *fbase; const char *sname; void *saddr; } DlInfo;
        int (*dladdr_fn)(const void *, DlInfo *) = dlsym((void *)-2, "dladdr");   /* RTLD_DEFAULT */
        for (u32 i = 0; dladdr_fn && i < sizeof(addrs) / sizeof(addrs[0]); i++) {
            DlInfo info = {0};
            Line l = {{0}, 0};
            put(&l, "[ltbench] sym "); putx(&l, addrs[i]); put(&l, " ");
            if (dladdr_fn((const void *)addrs[i], &info)) {
                const char *f = info.fname ? info.fname : "?", *base = f;
                for (const char *c = f; *c; c++) if (*c == '/') base = c + 1;
                put(&l, base); put(&l, " "); put(&l, info.sname ? info.sname : "?");
                put(&l, "+"); putx(&l, addrs[i] - (u32)info.saddr);
            } else {
                put(&l, "(none)");
            }
            put(&l, "\n");
            emit(l.b);
        }
    }
#endif
    start();
}

int bench_main(int argc, char **argv) {
    mach_timebase_info(tb);
    buffer_a = malloc(COPY_BYTES); buffer_b = malloc(COPY_BYTES);
    touch_area = mmap(0, 64 * 4096, 3, 0x1002, -1, 0);
    chase_area = mmap(0, 1024 * 4096, 3, 0x1002, -1, 0);
    if (!buffer_a || !buffer_b || touch_area == (char *)-1 || chase_area == (char *)-1) { emit("[ltbench] exit: no memory\n"); _exit(10); }
    memset(buffer_a, 1, COPY_BYTES); memset(touch_area, 0, 64 * 4096); memset(chase_area, 0, 1024 * 4096);

    void *runtime = objc_runtime = dlopen("/usr/lib/libobjc.A.dylib", 2);
    void *ui = dlopen("/System/Library/Frameworks/UIKit.framework/UIKit", 2);
    if (!runtime || !ui) { emit("[ltbench] exit: no objc/UIKit\n"); _exit(11); }
    get_class = dlsym(runtime, "objc_getClass");
    get_selector = dlsym(runtime, "sel_registerName");
    send = dlsym(runtime, "objc_msgSend");
    Object (*allocate_class)(Object, const char *, u32) = dlsym(runtime, "objc_allocateClassPair");
    int (*add_method)(Object, Selector, void *, const char *) = dlsym(runtime, "class_addMethod");
    void (*register_class)(Object) = dlsym(runtime, "objc_registerClassPair");
    int (*run)(int, char **, Object, Object) = dlsym(ui, "UIApplicationMain");
    if (!get_class || !get_selector || !send || !allocate_class || !add_method || !register_class || !run) { emit("[ltbench] exit: no runtime symbols\n"); _exit(12); }
    Object pool = call0(call0(get_class("NSAutoreleasePool"), "alloc"), "init");
    Object delegate = allocate_class(get_class("NSObject"), "LTBenchDelegate", 0);
    if (!delegate || !add_method(delegate, get_selector("applicationDidFinishLaunching:"), launched, "v@:@") ||
        !add_method(delegate, get_selector("benchTapped:"), tapped, "v@:@") ||
        !add_method(delegate, get_selector("benchFinished:"), finished, "v@:@")) { emit("[ltbench] exit: no delegate class\n"); _exit(13); }
    register_class(delegate);
    int result = run(argc, argv, (Object)0, string("LTBenchDelegate"));
    call0(pool, "drain");
    return result;
}
