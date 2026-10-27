#include "lighttouch.h"
#include "Network.hpp"
#include <filesystem>
#include <jni.h>
#if defined(__aarch64__)
#include <arm_neon.h>
#endif
#include <android/native_window_jni.h>
#include <android/bitmap.h>
#include <algorithm>
#include <cmath>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
#include <time.h>
#include <sys/stat.h>
#include <unistd.h>
#include <fcntl.h>
#include <cstdio>
#include <cerrno>
#include <cstdlib>
#include <dlfcn.h>
#include <dirent.h>
#include "frame_pacer.h"
#include <optional>
#include "presentation_lifetime.h"
#include "display_lease.h"
#include "frame_cache.h"
#include "touch_mapping_result.h"
#include "experimental_tcg_cache.h"
#include "performance_hint.h"
#include "profile_writer.h"
#include <memory>

namespace {
std::mutex guard;
std::mutex library_guard;
lt_library *library=nullptr;
std::string library_root;
lt_runtime *runtime = nullptr;
lt_device *device = nullptr;
ANativeWindow *window = nullptr;
FrameCopyCache frame_cache;
auto &pixels = frame_cache.pixels;
auto &frame = frame_cache.frame;
lt_pcm_info pcm = LT_INIT(lt_pcm_info);
lt_board_info board = LT_INIT(lt_board_info);
uint64_t sequence = 0;
uint64_t presentation_epoch = 0, surface_request = 0;
DisplayLease display_lease;
constexpr const char *retired_display="Display ownership retired";
uint64_t presented_frames=0,first_present_ns=0,last_present_ns=0,previous_present_ns=0,max_present_gap_ns=0;
std::unique_ptr<ProfileWriter> profile_presentation; // guard owns the producer handle.
uint64_t profile_presentation_started=0;
uint64_t max_present_work_ns=0,window_failures=0,frame_copy_failures=0,surface_generation=0,pcm_failures=0;
uint64_t present_intervals[6]={};
uint64_t frame_sample_pixels=0,frame_nonzero_rgb_samples=0,frame_rgb_sample_sum=0;
int first_lock_error=0,last_lock_error=0,first_post_error=0,last_post_error=0,last_geometry_error=0;
int last_buffer_format=0,last_buffer_width=0,last_buffer_height=0,last_buffer_stride=0;
uint64_t geometry_failures=0,invalid_buffers=0,first_window_error_ns=0;
unsigned window_log_budget=8;
// The runtime's publisher wakes the presenter as each frame lands. Polling on a
// fixed 16 ms sleep added up to a frame of latency and, with the copy and scale
// on top, capped delivery below the guest's 60 Hz and beat against its vsync.
std::mutex frame_wake_mutex;
std::condition_variable frame_wake;
uint64_t frame_wake_sequence=0;
VcpuPerformanceHint vcpu_hint;  // frame publisher thread only
uint64_t monotonic_ns() { timespec value{};clock_gettime(CLOCK_MONOTONIC,&value);return uint64_t(value.tv_sec)*1000000000ull+value.tv_nsec; }
// The guest is 60 Hz content. Saying so (API 30+) lets SurfaceFlinger run the panel at 60 Hz, or
// at 120 with each frame held for exactly two refreshes, instead of treating it as free-running.
void request_guest_frame_rate(ANativeWindow *window) {
    using SetFrameRate=int32_t(*)(ANativeWindow *,float,int8_t);
    static const auto set=reinterpret_cast<SetFrameRate>(dlsym(RTLD_DEFAULT,"ANativeWindow_setFrameRate"));
    constexpr int8_t fixed_source=1;  // ANATIVEWINDOW_FRAME_RATE_COMPATIBILITY_FIXED_SOURCE
    if(set)set(window,60.0f,fixed_source);
}
// A frame rate of 0 withdraws the request, so a covered guest leaves the panel to the UI on top.
void withdraw_guest_frame_rate(ANativeWindow *window) {
    using SetFrameRate=int32_t(*)(ANativeWindow *,float,int8_t);
    static const auto set=reinterpret_cast<SetFrameRate>(dlsym(RTLD_DEFAULT,"ANativeWindow_setFrameRate"));
    if(set)set(window,0.0f,0);
}
// LT_FRAME_PACING=off (debug-env) posts frames on arrival, for A/B runs.
bool frame_pacing_enabled() {
    static const bool enabled=[] { const char *v=getenv("LT_FRAME_PACING"); return !v || strcmp(v,"off")!=0; }();
    return enabled;
}
// Guest frames per 250 ms for the UI's fps graph: the 500 ms status poll alone
// would average away the dips it is there to show. Written by the publisher
// thread only; a reader may see a bucket mid-update, which costs one frame.
constexpr int fps_buckets=256;
constexpr uint64_t fps_bucket_ns=250000000;
std::atomic<uint64_t> fps_bucket_id[fps_buckets];
std::atomic<uint32_t> fps_bucket_frames[fps_buckets];
// Per-frame timeline for the UI's performance overlay and hitch inspector: when each
// guest frame was published, when the presenter posted it, and what presenting it
// cost. One slot per sequence; a slot is valid while its sequence field matches, so
// a reader that races the publisher drops that frame rather than mixing two.
constexpr uint64_t timeline_size=8192;  // ~136 s at 60 fps
struct TimelineSlot { std::atomic<uint64_t> sequence{0},published{0},presented{0},work{0}; };
TimelineSlot timeline[timeline_size];
std::atomic<uint64_t> timeline_latest{0};
void timeline_published(uint64_t sequence,uint64_t now) {
    auto &slot=timeline[sequence%timeline_size];
    slot.sequence.store(0,std::memory_order_relaxed);
    slot.published.store(now,std::memory_order_relaxed);
    slot.presented.store(0,std::memory_order_relaxed);slot.work.store(0,std::memory_order_relaxed);
    slot.sequence.store(sequence,std::memory_order_release);
    timeline_latest.store(sequence,std::memory_order_release);
}
void timeline_presented(uint64_t sequence,uint64_t now,uint64_t work) {
    auto &slot=timeline[sequence%timeline_size];
    if(slot.sequence.load(std::memory_order_acquire)!=sequence)return;
    slot.work.store(work,std::memory_order_relaxed);slot.presented.store(now,std::memory_order_release);
}
// How the guest frame reaches the panel. The compositor modes hand SurfaceFlinger a
// guest-sized buffer (or an integer multiple, for sharp pixels) and let it scale on the
// display hardware; the CPU mode is the old full-resolution nearest-neighbour upscale,
// which wrote every panel pixel (~10 MB a frame at 1080x2400). LT_PRESENT_SCALE=cpu for A/B.
enum PresentScale : int { ScaleSmooth=0, ScaleSharp=1, ScaleCpu=2 };
// False while a full page (Settings, the Store, setup) covers the guest surface. The guest keeps
// running; the presenter stops posting frames nobody can see and withdraws its 60 Hz request,
// which otherwise held the whole panel at 60 Hz under the page's scrolling (Pixel 10).
std::atomic<bool> present_visible{true};
std::atomic<int> present_scale{[] {
    const char *v=getenv("LT_PRESENT_SCALE");
    return v && !strcmp(v,"cpu")?int(ScaleCpu):v && !strcmp(v,"sharp")?int(ScaleSharp):int(ScaleSmooth);
}()};
void frame_published(void *,uint64_t published) {
    vcpu_hint.framePublished();
    timeline_published(published,monotonic_ns());
    const uint64_t id=monotonic_ns()/fps_bucket_ns;auto &slot=fps_bucket_id[id%fps_buckets];
    if(slot.load(std::memory_order_relaxed)!=id){fps_bucket_frames[id%fps_buckets].store(0,std::memory_order_relaxed);slot.store(id,std::memory_order_release);}
    fps_bucket_frames[id%fps_buckets].fetch_add(1,std::memory_order_relaxed);
    {std::lock_guard<std::mutex> lock(frame_wake_mutex);frame_wake_sequence=published;}
    frame_wake.notify_one();
}
// Diagnostic stages only; they neither admit controls nor establish a guest scene.
// This telemetry is lock-free on both supported Android ABIs. The sole presenter
// writes the tuple; a reader samples once and reports overlaps rather than spins.
// GuardWait identifies the last context observed under guard. Outside-guard
// stages identify the retained snapshot, which may differ from the current
// surfaceGeneration/presentationEpoch after a concurrent detach or Resume.
static_assert(std::atomic<uint64_t>::is_always_lock_free, "Presenter telemetry requires lock-free 64-bit atomics");
enum class PresentPhase : uint64_t {
    NotStarted=0, GuardWait=1, InputStatus=2, FrameCopy=3, CopiedCache=4,
    Configure=5, WindowLock=6, Scale=7, WindowPost=8, CommitWait=9,
    PublishCommit=10, WindowRelease=11, IdleNoTarget=12, IdleNoNewFrame=13, Idle=14
};
std::atomic<uint64_t> window_attached{0},present_phase{0},present_phase_entry_ns{0};
std::atomic<uint64_t> present_phase_generation{0},present_phase_epoch{0},present_phase_revision{0};
void present_stage(PresentPhase phase,uint64_t generation,uint64_t epoch) {
    ++present_phase_revision; // Odd during the tuple transition.
    present_phase_generation.store(generation);
    present_phase_epoch.store(epoch);
    present_phase_entry_ns.store(monotonic_ns());
    present_phase.store(static_cast<uint64_t>(phase));
    ++present_phase_revision;
}
bool presenter_started = false;
bool starting = false, startup_stop_requested = false, startup_pause_requested = false;
uint64_t last_rejected_input = 0;
uint64_t ignored_input_rejections=0,last_ignored_input_request=0;
std::string diagnostic;
unsigned graphics_provider_choice=0;
bool graphics_provider_fixed=false;
std::string graphics_provider_failure;
void *angle_provider_handles[3]={};
const char *graphics_provider_name() { return graphics_provider_choice==1?"angle-gl":"platform"; }
std::string engine_log_path;

// Each guest session writes engine-PID.log into the record's state, and nothing removed
// them: 137 (41 MB) on one phone, 271 on another. Keep the newest few for diagnosis.
static void prune_engine_logs(const char *state_path)
{
    constexpr size_t keep=7;  // plus the one this session opens
    std::vector<std::pair<time_t,std::string>> logs;
    if(DIR *dir=opendir(state_path)) {
        while(dirent *entry=readdir(dir)) {
            const std::string name=entry->d_name;
            if(name.rfind("engine-",0)!=0 || name.size()<5 || name.compare(name.size()-4,4,".log")!=0)continue;
            struct stat st;
            const std::string path=std::string(state_path)+"/"+name;
            if(stat(path.c_str(),&st)==0 && S_ISREG(st.st_mode))logs.emplace_back(st.st_mtime,path);
        }
        closedir(dir);
    }
    if(logs.size()<=keep)return;
    std::sort(logs.begin(),logs.end());
    for(size_t i=0;i+keep<logs.size();i++)unlink(logs[i].second.c_str());
}
std::string debug_qmp_path;
bool debug_qmp_armed=false;
bool debug_touch_trace_armed=false,debug_touch_trace_closed=false,debug_touch_trace_configured=false;
bool worker_start_consumed=false,debug_vcpu_accounting_configured=false;
bool experimental_tcg_ir_cache_requested=false;
#ifdef LT_ANDROID_DEBUG
extern "C" int lighttouch_debug_qmp_snapshot(const char *,int);
#endif
void window_error(const char *operation,int code) {
    if(!first_window_error_ns)first_window_error_ns=monotonic_ns();
    if(window_log_budget) {
        --window_log_budget;
        std::fprintf(stderr,"LightTouch Surface %s error=%d generation=%llu format=%d width=%d height=%d stride=%d\n",operation,code,
            static_cast<unsigned long long>(surface_generation),last_buffer_format,last_buffer_width,last_buffer_height,last_buffer_stride);
    }
}
// JNI's own UTF conversions use modified UTF-8: a character outside the BMP (an emoji) becomes two
// encoded surrogates, which strict readers (record.json, the filesystem) reject, and NewStringUTF
// rejects the standard 4-byte form under CheckJNI. Convert through UTF-16 both ways instead.
struct Utf {
    std::string text; const char *value = nullptr;
    Utf(JNIEnv *e, jstring s) {
        if (!s) return;
        const jsize n = e->GetStringLength(s);
        std::vector<jchar> units(n);
        e->GetStringRegion(s, 0, n, units.data());
        for (jsize i = 0; i < n; ++i) {
            uint32_t c = units[i];
            if (c >= 0xd800 && c <= 0xdbff && i + 1 < n && units[i + 1] >= 0xdc00 && units[i + 1] <= 0xdfff)
                c = 0x10000 + ((c - 0xd800) << 10) + (units[++i] - 0xdc00);
            else if (c >= 0xd800 && c <= 0xdfff) c = 0xfffd;  // unpaired surrogate
            if (c < 0x80) text += char(c);
            else if (c < 0x800) { text += char(0xc0 | c >> 6); text += char(0x80 | (c & 0x3f)); }
            else if (c < 0x10000) { text += char(0xe0 | c >> 12); text += char(0x80 | (c >> 6 & 0x3f)); text += char(0x80 | (c & 0x3f)); }
            else { text += char(0xf0 | c >> 18); text += char(0x80 | (c >> 12 & 0x3f)); text += char(0x80 | (c >> 6 & 0x3f)); text += char(0x80 | (c & 0x3f)); }
        }
        value = text.c_str();
    }
};
jstring string(JNIEnv *e, const std::string &s) {
    std::vector<jchar> units;
    units.reserve(s.size());
    for (size_t i = 0; i < s.size();) {
        auto c = static_cast<unsigned char>(s[i]);
        size_t n = c < 0x80 ? 1 : (c & 0xe0) == 0xc0 ? 2 : (c & 0xf0) == 0xe0 ? 3 : (c & 0xf8) == 0xf0 ? 4 : 0;
        uint32_t code = n == 1 ? c : c & (0x7f >> n);
        for (size_t k = 1; n && k < n; ++k)
            if (i + k >= s.size() || (static_cast<unsigned char>(s[i + k]) & 0xc0) != 0x80) n = 0;
            else code = code << 6 | (s[i + k] & 0x3f);
        if (!n) { units.push_back(0xfffd); ++i; continue; }  // malformed byte
        if (code >= 0x10000) { code -= 0x10000; units.push_back(jchar(0xd800 + (code >> 10))); units.push_back(jchar(0xdc00 + (code & 0x3ff))); }
        else units.push_back(jchar(code));
        i += n;
    }
    return e->NewString(units.data(), jsize(units.size()));
}
std::string result(lt_status status, const lt_error &error) {
    return status == LT_OK ? "" : (error.message[0] ? error.message : "Native operation failed");
}
struct Viewport { double left, top, width, height; };
Viewport viewport(int width, int height, const lt_frame_info &source) {
    const double scale = std::min(double(width) / std::max(source.width, 1u), double(height) / std::max(source.height, 1u));
    double w = source.width * scale, h = source.height * scale;
    return {(width-w)/2, (height-h)/2, w, h};
}
Viewport viewport(int width, int height) { return viewport(width, height, frame); }
lt_status open_library(const char *root,lt_error *error) {
    if(!root)return LT_INVALID;
    if(library && library_root==root)return LT_OK;
    if(library)lt_library_close(library);
    library=nullptr;library_root=root;
    lt_library_config config=LT_INIT(lt_library_config);config.root_path=root;
    auto status=lt_library_open(&config,&library,error);
    if(status!=LT_OK)return status;
    uint32_t count=0;status=lt_library_count(library,&count,error);
    if(status!=LT_OK)return status;
    std::string parent=library_root.substr(0,library_root.find_last_of('/'));
    std::string oldbase=parent+"/base",oldstate=parent+"/state",oldrom=parent+"/bootrom.bin",oldguest=parent+"/guest.itpack";
    if(count==0 && access(oldbase.c_str(),F_OK)==0) {
        lt_library_import_config migration=LT_INIT(lt_library_import_config);
        migration.source_path=oldbase.c_str();migration.display_name="Existing device";
        migration.bootrom_path=access(oldrom.c_str(),R_OK)==0?oldrom.c_str():nullptr;
        migration.guest_package_path=access(oldguest.c_str(),R_OK)==0?oldguest.c_str():nullptr;
        migration.max_bytes=64ull*1024*1024*1024;
        lt_record_info record=LT_INIT(lt_record_info);
        status=access(oldstate.c_str(),F_OK)==0?lt_library_adopt_legacy(library,&migration,oldstate.c_str(),&record,error):lt_library_import(library,&migration,&record,error);
    }
    return status;
}
using WindowSnapshot = PresentationWindow<ANativeWindow, ANativeWindow_acquire, ANativeWindow_release>;
using WindowPost = PresentationPost<ANativeWindow, ANativeWindow_unlockAndPost>;
// The guest's frame is BGRA in memory and the window wants RGBA with an opaque alpha. On arm64 four
// pixels are one table lookup (vqtbl1q_u8); a 960x640 frame is 614K pixels on this thread.
// After Un1box's 978bbefe (KlugKlugTG/Un1box).
void convert_row_rgba(uint32_t *dst,const uint8_t *src,int width) {
    int x=0;
#if defined(__aarch64__)
    static const uint8_t swap[16]={2,1,0,3,6,5,4,7,10,9,8,11,14,13,12,15};
    const uint8x16_t table=vld1q_u8(swap);
    const uint8x16_t opaque=vreinterpretq_u8_u32(vdupq_n_u32(0xff000000u));
    for(;x+4<=width;x+=4)
        vst1q_u8(reinterpret_cast<uint8_t *>(dst+x),vorrq_u8(vqtbl1q_u8(vld1q_u8(src+size_t(x)*4),table),opaque));
#endif
    for(;x<width;++x) {
        const uint8_t *p=src+size_t(x)*4;
        dst[x]=uint32_t(p[2])|(uint32_t(p[1])<<8)|(uint32_t(p[0])<<16)|0xff000000u;
    }
}
void present() {
    uint64_t configured_generation = 0;
    int configured_scale=-1;
    uint32_t configured_width=0,configured_height=0;
    uint64_t woken_sequence = 0;
    uint64_t phase_generation=0,phase_epoch=0;
    uint64_t withdrawn_generation=0;
    for (;;) {
        // The snapshot outlives both guarded scopes, so its release is outside
        // the control mutex even when a stale platform operation returns late.
        std::optional<WindowSnapshot> snapshot,covered;
        const bool visible=present_visible.load(std::memory_order_relaxed);
        lt_frame_info source = LT_INIT(lt_frame_info);
        uint64_t begin = 0;
        const uint64_t observe_begin=monotonic_ns();
        uint64_t guard_acquired=0,copy_begin=0;
        present_stage(PresentPhase::GuardWait,phase_generation,phase_epoch);
        {
            std::lock_guard<std::mutex> lock(guard);
            guard_acquired=monotonic_ns();
            phase_generation=surface_generation;phase_epoch=presentation_epoch;
            if (runtime) {
                present_stage(PresentPhase::InputStatus,phase_generation,phase_epoch);
                lt_input_status delivered=LT_INIT(lt_input_status);lt_error error=LT_INIT(lt_error);
                if(lt_runtime_input_status(runtime,&delivered,&error)==LT_OK) {
                    if(delivered.state==2)display_lease.inputCompleted(delivered.request_id);
                    else if(delivered.state==3 && delivered.request_id!=last_rejected_input) {
                        last_rejected_input=delivered.request_id;
                        if(display_lease.reportsRejectedInput(delivered.request_id)) {
                            diagnostic="Guest rejected contact batch "+std::to_string(delivered.request_id)+" (engine result "+std::to_string(delivered.engine_result)+")";
                            if(display_lease.cancelsRejectedInput(delivered.request_id)) {
                                diagnostic += "; contact release requested";
                                if(lt_runtime_cancel_input(runtime,&error)!=LT_OK)diagnostic += "; input release failed: "+std::string(error.message);
                                else display_lease.contactsReleased();
                            } else {
                                diagnostic += "; no owned contacts require release";
                                display_lease.inputCompleted(delivered.request_id);
                            }
                        } else {
                            ++ignored_input_rejections;last_ignored_input_request=delivered.request_id;
                        }
                    }
                }
            }
            if (window && !visible && withdrawn_generation!=surface_generation)
                covered.emplace(window, surface_generation, presentation_epoch);
            if (runtime && window && visible) {
                copy_begin=monotonic_ns();
                present_stage(PresentPhase::FrameCopy,phase_generation,phase_epoch);
                lt_error error = LT_INIT(lt_error);
                const auto status = frame_cache.latest(sequence, error,
                    [&](uint64_t since, lt_frame_info &out, lt_error &failure) {
                        return lt_runtime_frame_metadata(runtime, since, &out, &failure);
                    },
                    [&](void *storage, uint64_t capacity, uint64_t &next, lt_frame_info &out, lt_error &failure) {
                        return lt_runtime_copy_frame(runtime, storage, capacity, &next, &out, &failure);
                    },
                    [&] { frame_sample_pixels=frame_nonzero_rgb_samples=frame_rgb_sample_sum=0; });
                if (status == LT_OK && frame.width && frame.height && frame.stride >= frame.width * 4 && frame.bytes <= pixels.size()) {
                    begin=monotonic_ns();
                    source=frame;
                    snapshot.emplace(window, surface_generation, presentation_epoch);
                    frame_sample_pixels=frame_nonzero_rgb_samples=frame_rgb_sample_sum=0;
                    // A fixed 16x16 grid diagnoses the copied guest frame without
                    // another full image pass or claiming every pixel is black.
                    for(uint32_t gy=0;gy<16;++gy)for(uint32_t gx=0;gx<16;++gx) {
                        const uint32_t x=std::min((gx*2+1)*source.width/32,source.width-1);
                        const uint32_t y=std::min((gy*2+1)*source.height/32,source.height-1);
                        const uint8_t *sample=pixels.data()+y*source.stride+x*4;
                        const uint32_t rgb=uint32_t(sample[0])+sample[1]+sample[2];
                        ++frame_sample_pixels;frame_nonzero_rgb_samples+=rgb!=0;frame_rgb_sample_sum+=rgb;
                    }
                    present_stage(PresentPhase::CopiedCache,phase_generation,phase_epoch);
                } else if(status!=LT_AGAIN) ++frame_copy_failures;
            }
            if(!snapshot)present_stage(runtime&&window?PresentPhase::IdleNoNewFrame:PresentPhase::IdleNoTarget,
                phase_generation,phase_epoch);
        }
        if (covered) {
            // Uncovered, the next frame reconfigures the window, which asks for 60 Hz again.
            withdraw_guest_frame_rate(covered->get());
            withdrawn_generation=covered->generation();configured_generation=0;
            covered.reset();
        }
        if (visible) withdrawn_generation=0;
        if (snapshot) {
            ANativeWindow *target_window=snapshot->get();
            int geometry_code=0, geometry_width=0, geometry_height=0, geometry_format=0;
            const int scale=present_scale.load(std::memory_order_relaxed);
            const bool configure=configured_generation!=snapshot->generation() || scale!=configured_scale ||
                source.width!=configured_width || source.height!=configured_height;
            if(configure) {
                present_stage(PresentPhase::Configure,phase_generation,phase_epoch);
                // Only this presenter configures or draws the window. Binder
                // setters only exchange the owning reference under guard.
                // Reset first: buffer dimensions persist on the Surface across
                // attaches, and the reset size is the panel area to scale into.
                geometry_code=ANativeWindow_setBuffersGeometry(target_window,0,0,WINDOW_FORMAT_RGBA_8888);
                geometry_width=ANativeWindow_getWidth(target_window);
                geometry_height=ANativeWindow_getHeight(target_window);
                geometry_format=ANativeWindow_getFormat(target_window);
                if(!geometry_code && scale!=ScaleCpu && source.width && source.height) {
                    const int factor=scale==ScaleSharp?std::max(1,std::min(geometry_width/int(source.width),geometry_height/int(source.height))):1;
                    geometry_code=ANativeWindow_setBuffersGeometry(target_window,int(source.width)*factor,int(source.height)*factor,WINDOW_FORMAT_RGBA_8888);
                }
                request_guest_frame_rate(target_window);
                configured_generation=snapshot->generation();configured_scale=scale;
                configured_width=source.width;configured_height=source.height;
            }
            ANativeWindow_Buffer target{};
            present_stage(PresentPhase::WindowLock,phase_generation,phase_epoch);
            const uint64_t lock_begin=monotonic_ns();
            const int lock_code=ANativeWindow_lock(target_window,&target,nullptr);
            const uint64_t lock_end=monotonic_ns();
            uint64_t scale_end=lock_end,post_begin=lock_end,post_end=lock_end;
            int post_code=0;
            bool valid_buffer=false;
            if(lock_code==0) {
                WindowPost post(target_window);
                valid_buffer=target.bits && target.width>0 && target.height>0 && target.stride>=target.width &&
                    (target.format==WINDOW_FORMAT_RGBA_8888 || target.format==WINDOW_FORMAT_RGBX_8888);
                if(valid_buffer) {
                    present_stage(PresentPhase::Scale,phase_generation,phase_epoch);
                    const auto view=viewport(target.width,target.height,source);
                    // This thread is the sole pixels/frame writer and only
                    // grows storage in its next guarded iteration. Capture reads under guard. Those concurrent
                    // reads are safe; the next write waits until this iteration
                    // and any guarded capture have finished. No runtime escapes.
                    // Nearest-neighbour upscale, same mapping as a per-pixel
                    // divide: the source column of each target column is
                    // computed once per frame, each source row is converted
                    // once, and repeated target rows are copies of it. A
                    // 320x480 guest on a 1080x2400 panel used to cost two
                    // integer divides per target pixel, ~2.6M per frame.
                    static thread_local std::vector<uint32_t> columns;
                    const int x0=std::max(int(view.left),0);
                    int x1=x0;
                    while(x1<view.left+view.width && x1<target.width)++x1;
                    columns.resize(size_t(std::max(x1-x0,0)));
                    for(int x=x0;x<x1;++x)
                        columns[x-x0]=std::min(uint32_t((x-view.left)*source.width/view.width),source.width-1)*4;
                    // Unscaled across (the default Smooth scaling's guest-sized buffer): consecutive
                    // source pixels, so a row converts in one pass with no column table.
                    bool across=x1>x0;
                    for(int x=x0+1;across && x<x1;++x)across=columns[x-x0]==columns[0]+uint32_t(x-x0)*4;
                    const uint32_t *previous=nullptr;
                    uint32_t previous_sy=UINT32_MAX;
                    for(int y=0;y<target.height;++y) {
                        auto *row=static_cast<uint32_t *>(target.bits)+y*target.stride;
                        if(y<view.top || y>=view.top+view.height) {
                            std::fill(row,row+target.width,0xff000000u);
                            continue;
                        }
                        uint32_t sy=std::min(uint32_t((y-view.top)*source.height/view.height),source.height-1);
                        if(previous && sy==previous_sy) {
                            memcpy(row,previous,size_t(target.width)*4);
                            continue;
                        }
                        std::fill(row,row+x0,0xff000000u);
                        std::fill(row+x1,row+target.width,0xff000000u);
                        const uint8_t *src=pixels.data()+sy*source.stride;
                        if(across)convert_row_rgba(row+x0,src+columns[0],x1-x0);
                        else for(int x=x0;x<x1;++x) {
                            const uint8_t *p=src+columns[x-x0];
                            row[x]=uint32_t(p[2])|(uint32_t(p[1])<<8)|(uint32_t(p[0])<<16)|0xff000000u;
                        }
                        previous=row;previous_sy=sy;
                    }
                }
                scale_end=monotonic_ns();
                if(valid_buffer && frame_pacing_enabled()) {
                    static FramePacer pacer;  // this thread only
                    static DisplayVsync display_vsync;
                    static const bool display_vsync_started=(display_vsync.start(),true);
                    (void)display_vsync_started;
                    const uint64_t ready=monotonic_ns();
                    const auto [vsync,period]=display_vsync.get(ready);
                    if(const uint64_t at=pacer.post_at(source.timestamp_ns,ready,vsync,period)) {
                        const timespec until{time_t(at/1000000000ull),long(at%1000000000ull)};
                        clock_nanosleep(CLOCK_MONOTONIC,TIMER_ABSTIME,&until,nullptr);
                    }
                }
                // Exactly one post for every successful lock, including invalid
                // layouts and a window detached while the call was in flight.
                present_stage(PresentPhase::WindowPost,phase_generation,phase_epoch);
                post_begin=monotonic_ns();
                post_code=post.finish();
                post_end=monotonic_ns();
            }
            const auto now=monotonic_ns();
            present_stage(PresentPhase::CommitWait,phase_generation,phase_epoch);
            {
                std::lock_guard<std::mutex> lock(guard);
                present_stage(PresentPhase::PublishCommit,phase_generation,phase_epoch);
                if(snapshot->current(window,surface_generation,presentation_epoch)) {
                    if(configure) {
                        last_buffer_width=geometry_width;last_buffer_height=geometry_height;last_buffer_format=geometry_format;
                        last_geometry_error=geometry_code;
                        if(geometry_code){++geometry_failures;window_error("geometry",geometry_code);}
                    }
                    if(lock_code==0) {
                        last_buffer_format=target.format;last_buffer_width=target.width;last_buffer_height=target.height;last_buffer_stride=target.stride;
                        if(!valid_buffer) {
                            ++invalid_buffers;++window_failures;window_error("buffer layout",-1);sequence=0;
                            if(post_code){last_post_error=post_code;if(!first_post_error)first_post_error=post_code;window_error("post invalid buffer",post_code);}
                        } else if(post_code==0) {
                            if(profile_presentation && observe_begin>=profile_presentation_started) {
                                // Post completion is observable here; physical scanout is not.
                                profile_presentation->append("P %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu\n",
                                    (unsigned long long)now,(unsigned long long)source.sequence,
                                    (unsigned long long)source.timestamp_ns,(unsigned long long)observe_begin,
                                    (unsigned long long)guard_acquired,(unsigned long long)copy_begin,
                                    (unsigned long long)begin,(unsigned long long)lock_begin,
                                    (unsigned long long)lock_end,(unsigned long long)scale_end,
                                    (unsigned long long)post_begin,(unsigned long long)post_end,
                                    (unsigned long long)monotonic_ns());
                            }
                            timeline_presented(source.sequence,now,(scale_end-lock_begin)+(post_end-post_begin)+(begin-copy_begin));
                            ++presented_frames;
                            if(!first_present_ns)first_present_ns=now;
                            if(previous_present_ns) {
                                const auto gap=now-previous_present_ns;max_present_gap_ns=std::max(max_present_gap_ns,gap);
                                const uint64_t limits[]={16666667,33333334,50000000,100000000,250000000};
                                size_t bucket=0;while(bucket<5 && gap>limits[bucket])++bucket;++present_intervals[bucket];
                            }
                            last_present_ns=previous_present_ns=now;max_present_work_ns=std::max(max_present_work_ns,now-begin);
                        } else {++window_failures;sequence=0;last_post_error=post_code;if(!first_post_error)first_post_error=post_code;window_error("post",post_code);}
                    } else {++window_failures;sequence=0;last_lock_error=lock_code;if(!first_lock_error)first_lock_error=lock_code;window_error("lock",lock_code);}
                }
            }
            present_stage(PresentPhase::WindowRelease,phase_generation,phase_epoch);
            snapshot.reset(); // Platform release is outside guard, including detach.
            present_stage(PresentPhase::Idle,phase_generation,phase_epoch);
        }
        // A frame published while this one was drawn is presented at once.
        // The timeout keeps input-status and surface changes serviced while the
        // guest screen is static.
        std::unique_lock<std::mutex> wake(frame_wake_mutex);
        frame_wake.wait_for(wake,std::chrono::milliseconds(16),[&] { return frame_wake_sequence!=woken_sequence; });
        woken_sequence=frame_wake_sequence;
    }
}
}
#define JNI(name) extern "C" JNIEXPORT
// The running import's phase and bytes, read by importProgress while importBase holds the library.
static std::mutex import_progress_guard;
static std::string import_phase;
static uint64_t import_done=0,import_total=0;
static int import_observed(void *,const char *phase,uint64_t done,uint64_t total) {
    std::lock_guard<std::mutex> lock(import_progress_guard);import_phase=phase?phase:"";import_done=done;import_total=total;return 0;
}
JNI(importProgress) jobjectArray JNICALL Java_org_lighttouch_android_Native_importProgress(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(import_progress_guard);
    auto arr=env->NewObjectArray(3,env->FindClass("java/lang/String"),nullptr);
    env->SetObjectArrayElement(arr,0,string(env,import_phase));env->SetObjectArrayElement(arr,1,string(env,std::to_string(import_done)));
    env->SetObjectArrayElement(arr,2,string(env,std::to_string(import_total)));return arr;
}
JNI(importBase) jstring JNICALL Java_org_lighttouch_android_Native_importBase(JNIEnv *env,jobject,jstring source,jstring root,jstring name,jstring rom,jstring guest,jstring sha256) {
    std::lock_guard<std::mutex> lock(library_guard);
    Utf src(env,source),path(env,root),label(env,name),bootrom(env,rom),package(env,guest),hashed(env,sha256);
    lt_error error=LT_INIT(lt_error);auto status=open_library(path.value,&error);
    if(status!=LT_OK)return string(env,result(status,error));
    lt_library_import_config config=LT_INIT(lt_library_import_config);
    config.source_path=src.value;config.display_name=label.value;
    config.bootrom_path=bootrom.value && access(bootrom.value,R_OK)==0?bootrom.value:nullptr;
    config.guest_package_path=package.value && access(package.value,R_OK)==0?package.value:nullptr;
    config.max_bytes=64ull*1024*1024*1024;
    struct stat info;if(!src.value||stat(src.value,&info)!=0)return string(env,"Cannot inspect imported base");
    char checksum[65];
    if(S_ISREG(info.st_mode)) {
        // The app hashes the archive while copying it in; hashing it here again cost another full read.
        if(hashed.value && strlen(hashed.value)==64)snprintf(checksum,sizeof checksum,"%s",hashed.value);
        else if((status=lt_sha256_file(src.value,checksum,&error))!=LT_OK)return string(env,result(status,error));
        config.expected_sha256=checksum;
    }
    lt_record_info record=LT_INIT(lt_record_info);
    import_observed(nullptr,"",0,0);
    status=lt_library_import_observed(library,&config,import_observed,nullptr,&record,&error);
    import_observed(nullptr,"",0,0);
    return string(env,result(status,error));
}
JNI(adoptPrepared) jstring JNICALL Java_org_lighttouch_android_Native_adoptPrepared(JNIEnv *env,jobject,jstring source,jstring root,jstring name,jstring rom) {
    std::lock_guard<std::mutex> lock(library_guard);
    Utf src(env,source),path(env,root),label(env,name),bootrom(env,rom);
    lt_error error=LT_INIT(lt_error);auto status=open_library(path.value,&error);
    if(status!=LT_OK)return string(env,result(status,error));
    lt_library_import_config config=LT_INIT(lt_library_import_config);
    config.source_path=src.value;config.display_name=label.value;
    config.bootrom_path=bootrom.value && access(bootrom.value,R_OK)==0?bootrom.value:nullptr;
    lt_record_info record=LT_INIT(lt_record_info);
    import_observed(nullptr,"",0,0);
    status=lt_library_adopt_prepared(library,&config,import_observed,nullptr,&record,&error);
    import_observed(nullptr,"",0,0);
    return string(env,result(status,error));
}
JNI(setGuestPackage) jstring JNICALL Java_org_lighttouch_android_Native_setGuestPackage(JNIEnv *env,jobject,jstring root,jstring id,jstring guest) {
    std::lock_guard<std::mutex> lock(library_guard);
    Utf path(env,root),record_id(env,id),package(env,guest);
    lt_error error=LT_INIT(lt_error);auto status=open_library(path.value,&error);
    if(status!=LT_OK)return string(env,result(status,error));
    lt_record_info record=LT_INIT(lt_record_info);
    status=lt_library_set_guest_package(library,record_id.value,package.value,&record,&error);
    return string(env,result(status,error));
}
JNI(records) jobjectArray JNICALL Java_org_lighttouch_android_Native_records(JNIEnv *env,jobject,jstring root) {
    std::lock_guard<std::mutex> lock(library_guard);Utf path(env,root);lt_error error=LT_INIT(lt_error);
    auto status=open_library(path.value,&error);uint32_t count=0;
    if(status==LT_OK)status=lt_library_count(library,&count,&error);
    jclass strings=env->FindClass("[Ljava/lang/String;");jclass str=env->FindClass("java/lang/String");
    if(status!=LT_OK) {
        auto rows=env->NewObjectArray(1,strings,nullptr);auto row=env->NewObjectArray(2,str,nullptr);
        env->SetObjectArrayElement(row,0,string(env,""));env->SetObjectArrayElement(row,1,string(env,result(status,error)));
        env->SetObjectArrayElement(rows,0,row);return rows;
    }
    auto rows=env->NewObjectArray(count,strings,nullptr);
    for(uint32_t i=0;i<count;++i) {
        lt_record_info record=LT_INIT(lt_record_info);status=lt_library_record_at(library,i,&record,&error);
        auto row=env->NewObjectArray(11,str,nullptr);
        lt_board_info facts=LT_INIT(lt_board_info);
        if(status==LT_OK)status=lt_engine_board("libqemu-arm.so",record.board,&facts,&error);
        std::vector<std::string> values=status==LT_OK?std::vector<std::string>{record.id,record.display_name,record.board,record.build,std::to_string(record.selected),std::to_string(record.dirty),std::to_string(facts.max_contacts),(facts.capabilities&2)?"1":"0",(facts.capabilities&1)?"1":"0",record.bootrom_path[0] && access(record.bootrom_path,R_OK)==0?"1":"0",record.guest_package_path[0] && access(record.guest_package_path,R_OK)==0?"1":"0"}:std::vector<std::string>{"",result(status,error)};
        for(size_t index=0;index<values.size();++index)env->SetObjectArrayElement(row,index,string(env,values[index]));
        env->SetObjectArrayElement(rows,i,row);env->DeleteLocalRef(row);
    }
    return rows;
}
JNI(select) jstring JNICALL Java_org_lighttouch_android_Native_select(JNIEnv *env,jobject,jstring root,jstring id) {
    std::lock_guard<std::mutex> lock(library_guard);Utf path(env,root),record(env,id);lt_error error=LT_INIT(lt_error);
    auto status=open_library(path.value,&error);if(status==LT_OK)status=lt_library_select(library,record.value,&error);
    return string(env,result(status,error));
}
JNI(rename) jstring JNICALL Java_org_lighttouch_android_Native_rename(JNIEnv *env,jobject,jstring root,jstring id,jstring name) {
    std::lock_guard<std::mutex> lock(library_guard);Utf path(env,root),record(env,id),label(env,name);lt_error error=LT_INIT(lt_error);
    auto status=open_library(path.value,&error);if(status==LT_OK)status=lt_library_rename(library,record.value,label.value,&error);
    return string(env,result(status,error));
}
JNI(remove) jstring JNICALL Java_org_lighttouch_android_Native_remove(JNIEnv *env,jobject,jstring root,jstring id) {
    std::lock_guard<std::mutex> lock(library_guard);Utf path(env,root),record(env,id);lt_error error=LT_INIT(lt_error);
    auto status=open_library(path.value,&error);if(status==LT_OK)status=lt_library_remove(library,record.value,&error);
    return string(env,result(status,error));
}
// The listener and CA live for this worker; route/date/policy can change live. The engine forwards to the socket
// path from boot whatever the mode, and the proxy is made the first time networking is on (at boot or live).
static std::mutex network_guard;
static std::unique_ptr<lt::network::Proxy> network_proxy;
static uint32_t network_mode = 0;
static uint32_t network_local = 0;
static std::string network_socket;
static std::string network_identity;
static std::string network_roots;
static void network_listen(const lt::network::Configuration &config) {
    network_proxy=std::make_unique<lt::network::Proxy>(network_identity,config,network_roots);
    std::filesystem::remove(network_socket);
    try { network_proxy->listen(network_socket); } catch(...) { network_proxy.reset(); throw; }
}
JNI(networkPrepare) jstring JNICALL Java_org_lighttouch_android_Native_networkPrepare(JNIEnv *env,jobject,jstring root,jstring record,jint mode,jstring date,jboolean local,jstring roots) {
    Utf directory(env,root),id(env,record),day(env,date),certificates(env,roots);
    try {
        if(mode<0 || mode>2 || !id.value || std::string(id.value).empty() ||
           std::string(id.value).find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-")!=std::string::npos)
            return string(env,"Invalid network configuration");
        std::lock_guard<std::mutex> lock(network_guard);
        network_proxy.reset();network_mode=uint32_t(mode);network_local=local;
        network_identity=std::string(directory.value)+"/network/"+id.value;network_roots=certificates.value;
        network_socket=std::string(directory.value)+"/network-proxy.sock";
        std::filesystem::remove(network_socket);
        if(mode) {
            lt::network::Configuration config;config.mode=mode==1?lt::network::Mode::direct:lt::network::Mode::archive;
            config.archive_date=day.value;config.local_network=local;
            network_listen(config);
        }
        return string(env,"");
    } catch(const std::exception &e) { return string(env,e.what()); }
}
JNI(networkCertificate) jbyteArray JNICALL Java_org_lighttouch_android_Native_networkCertificate(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(network_guard);
    auto der=network_proxy?network_proxy->certificate():std::string();
    auto bytes=env->NewByteArray(jsize(der.size()));
    if(!der.empty())env->SetByteArrayRegion(bytes,0,jsize(der.size()),reinterpret_cast<const jbyte *>(der.data()));
    return bytes;
}
JNI(networkUpdate) jobjectArray JNICALL Java_org_lighttouch_android_Native_networkUpdate(JNIEnv *env,jobject,jint mode,jstring date,jboolean local) {
    Utf day(env,date);
    lt_error error=LT_INIT(lt_error);
    lt_status status=LT_OK;
    std::string message;
    try {
        const bool valid=mode>=0 && mode<=2 && day.value && lt::network::Configuration::valid_date(day.value);
        lt::network::Configuration config;
        if(valid) {
            config.mode=mode==0?lt::network::Mode::off:mode==1?lt::network::Mode::direct:lt::network::Mode::archive;
            config.archive_date=day.value;config.local_network=local;
        }
        // Turning networking on for the first time this session makes the proxy, outside the presenter's guard
        // (a first identity takes a key generation).
        if(valid && mode) {
            std::lock_guard<std::mutex> lock(network_guard);
            if(!network_proxy && !network_socket.empty())network_listen(config);
        }
        std::scoped_lock lock(guard,network_guard);
        if(!valid) {
            status=LT_INVALID;message="Invalid network configuration";
        } else if(!network_proxy && mode) {
            status=LT_UNSUPPORTED;message="Networking isn't available in this session; stop and start the device";
        } else {
            status=lt_runtime_network_policy(runtime,uint32_t(mode),local,&error);
            if(status==LT_OK) {
                if(network_proxy)network_proxy->configure(std::move(config));
                network_mode=uint32_t(mode);network_local=local;
            } else message=result(status,error);
        }
    } catch(const std::exception &e) { status=LT_IO;message=e.what(); }
    auto output=env->NewObjectArray(2,env->FindClass("java/lang/String"),nullptr);
    env->SetObjectArrayElement(output,0,string(env,std::to_string(status)));
    env->SetObjectArrayElement(output,1,string(env,message));
    return output;
}
JNI(networkStatus) jobjectArray JNICALL Java_org_lighttouch_android_Native_networkStatus(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(network_guard);
    auto activity=network_proxy?network_proxy->activity():lt::network::Activity{};
    auto output=env->NewObjectArray(8,env->FindClass("java/lang/String"),nullptr);
    env->SetObjectArrayElement(output,0,string(env,network_proxy?activity.summary():"Off"));
    env->SetObjectArrayElement(output,1,string(env,network_proxy?network_proxy->fingerprint():""));
    env->SetObjectArrayElement(output,2,string(env,std::to_string(activity.connections)));
    env->SetObjectArrayElement(output,3,string(env,std::to_string(activity.active)));
    env->SetObjectArrayElement(output,4,string(env,std::to_string(activity.completed)));
    env->SetObjectArrayElement(output,5,string(env,std::to_string(activity.failed)));
    env->SetObjectArrayElement(output,6,string(env,activity.last_result));
    env->SetObjectArrayElement(output,7,string(env,activity.last_result_error?"1":"0"));return output;
}

static jstring start_runtime(JNIEnv *env,jstring root,jstring id,bool allow_dirty,bool headphones) {
    bool qmp_requested=false;
    bool touch_trace_requested=false;
    {
        std::lock_guard<std::mutex> lock(guard);
        if(runtime || starting)return string(env,"Worker already consumed; stop and start a fresh worker");
        starting=true;worker_start_consumed=true;diagnostic.clear();qmp_requested=debug_qmp_armed;debug_qmp_armed=false;
        // Ordinary/release starts never inherit an external cache opt-in. The
        // explicit debug selector records only a bool until this consumed start.
        const auto cache_reset=experimental_cache_environment(false,"","");
        if(!cache_reset.empty()) { starting=false;diagnostic=cache_reset;return string(env,diagnostic); }
        touch_trace_requested=debug_touch_trace_armed;debug_touch_trace_closed=true;
        graphics_provider_fixed=true;
        if(!graphics_provider_failure.empty()) {
            starting=false;diagnostic=graphics_provider_failure;return string(env,diagnostic);
        }
        if(setenv("LIGHTTOUCH_GLES_PROVIDER",graphics_provider_name(),1)!=0) {
            starting=false;diagnostic="Cannot select the fixed guest graphics provider";return string(env,diagnostic);
        }
        // Reuse the existing explicit fresh-worker debug arm. Ordinary starts
        // overwrite inherited values so shutdown decision tracing stays off.
        if(setenv("IT_SHUTDOWN_DIAG",qmp_requested?"1":"0",1)!=0) {
            starting=false;diagnostic="Cannot configure shutdown diagnostics";return string(env,diagnostic);
        }
    }
    // Integrity admission can hash many gigabytes. It must not hold the frame,
    // status, input or lifecycle mutex while the library performs that work.
    std::lock_guard<std::mutex> storage_lock(library_guard);
    Utf path(env,root),record(env,id);lt_error error=LT_INIT(lt_error);
    lt_device *prepared_device=nullptr;lt_runtime *prepared_runtime=nullptr;
    lt_board_info prepared_board=LT_INIT(lt_board_info);
    auto status=open_library(path.value,&error);
    if(status==LT_OK)status=lt_library_select(library,record.value,&error);
    lt_record_info selected=LT_INIT(lt_record_info);
    if(status==LT_OK)status=lt_library_selected(library,&selected,&error);
    if(status==LT_OK && experimental_tcg_ir_cache_requested) {
        // The existing explicit debug cache request selects fixed bounded cost
        // counters too. Ordinary starts scrub every inherited cache opt-in.
        const auto cache_error=experimental_cache_environment(true,path.value,selected.id,true);
        if(!cache_error.empty()) {
            status=LT_IO;error.code=LT_IO;
            std::snprintf(error.message,sizeof(error.message),"%s",cache_error.c_str());
        }
    }
    if(status==LT_OK)status=lt_library_device_open(library,record.value,allow_dirty?1u:0u,&prepared_device,&error);
    if(status==LT_OK) {
        prune_engine_logs(selected.state_path);
        const std::string path=std::string(selected.state_path)+"/engine-"+std::to_string(getpid())+".log";
        const int log_fd=open(path.c_str(),O_CREAT|O_WRONLY|O_APPEND|O_CLOEXEC,0600);
        if(log_fd<0 || fchmod(log_fd,0600)!=0 || dup2(log_fd,STDOUT_FILENO)<0 || dup2(log_fd,STDERR_FILENO)<0) {
            status=LT_IO;error.code=LT_IO;std::snprintf(error.message,sizeof(error.message),"Cannot retain private guest diagnostics; check available storage");
        } else {
            std::setvbuf(stdout,nullptr,_IOLBF,0);std::setvbuf(stderr,nullptr,_IONBF,0);
            std::lock_guard<std::mutex> lock(guard);engine_log_path=path;
        }
        if(log_fd>=0)close(log_fd);
    }
    if(status==LT_OK)status=lt_engine_board("libqemu-arm.so",selected.board,&prepared_board,&error);
    std::string serial=std::string(selected.state_path)+"/guest-serial.log";
    lt_runtime_config engine=LT_INIT(lt_runtime_config);
    engine.engine_library="libqemu-arm.so";engine.device=prepared_device;engine.boot=lt_boot_config LT_INIT(lt_boot_config);
    engine.boot.machine=nullptr;engine.boot.bootrom_path=selected.bootrom_path[0]?selected.bootrom_path:nullptr;
    engine.boot.guest_package_path=selected.guest_package_path;engine.boot.serial_path=serial.c_str();engine.boot.audio_enabled=1;
    engine.boot.headphones=headphones && (prepared_board.capabilities&LT_BOARD_HEADPHONES);
    engine.boot.network_mode=network_mode;engine.boot.network_local=network_local;
    engine.boot.network_socket=network_socket.empty()?nullptr:network_socket.c_str();
    if(status==LT_OK)status=lt_runtime_create(&engine,&prepared_runtime,&error);
    if(status==LT_OK)status=lt_runtime_set_frame_listener(prepared_runtime,frame_published,nullptr,&error);
#ifdef LT_ANDROID_DEBUG
    if(status==LT_OK && touch_trace_requested) {
        status=lt_runtime_debug_touch_trace_enable(prepared_runtime,&error);
        if(status==LT_OK){std::lock_guard<std::mutex> lock(guard);debug_touch_trace_configured=true;}
    }
#else
    (void)touch_trace_requested;
#endif
    if(status==LT_OK && qmp_requested) {
        status=lt_runtime_debug_qmp_enable(prepared_runtime,&error);
        char endpoint[1024]={};
        if(status==LT_OK)status=lt_runtime_debug_qmp_path(prepared_runtime,endpoint,sizeof(endpoint),&error);
        if(status==LT_OK){std::lock_guard<std::mutex> lock(guard);debug_qmp_path=endpoint;}
    }
    if(status==LT_OK)status=lt_runtime_start(prepared_runtime,&error);
    if(status!=LT_OK) {
        bool released=!prepared_runtime;
        if(prepared_runtime){lt_error cleanup=LT_INIT(lt_error);lt_runtime_set_frame_listener(prepared_runtime,nullptr,nullptr,&cleanup);
            released=lt_runtime_destroy(prepared_runtime,&cleanup)==LT_OK;}
        if(released && prepared_device)lt_device_close(prepared_device);
        std::lock_guard<std::mutex> lock(guard);starting=false;diagnostic=result(status,error);
        return string(env,diagnostic);
    }
    std::lock_guard<std::mutex> lock(guard);
    runtime=prepared_runtime;device=prepared_device;board=prepared_board;starting=false;
    if(startup_pause_requested && !startup_stop_requested) {
        lt_error pause_error=LT_INIT(lt_error);
        auto paused=lt_runtime_pause(runtime,1,&pause_error);
        if(paused!=LT_OK)diagnostic=result(paused,pause_error);
    }
    if(startup_stop_requested) {
        lt_error stop_error=LT_INIT(lt_error);
        auto stopped=lt_runtime_request_shutdown(runtime,&stop_error);
        if(stopped!=LT_OK)diagnostic=result(stopped,stop_error);
    }
    if(!presenter_started){presenter_started=true;std::thread(present).detach();}
    return string(env,diagnostic);
}

JNI(start) jstring JNICALL Java_org_lighttouch_android_Native_start(JNIEnv *env,jobject,jstring root,jstring id,jboolean headphones) {
    // Ordinary startup must not silently admit an interrupted writable state.
    return start_runtime(env,root,id,false,headphones);
}
JNI(recoverStart) jstring JNICALL Java_org_lighttouch_android_Native_recoverStart(JNIEnv *env,jobject,jstring root,jstring id,jboolean headphones) {
    return start_runtime(env,root,id,true,headphones);
}

JNI(status) jobjectArray JNICALL Java_org_lighttouch_android_Native_status(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(guard);
    const char *states[]={"stopped","starting","running","paused","stopping","failed"};
    lt_runtime_status s=LT_INIT(lt_runtime_status);lt_error poll_error=LT_INIT(lt_error);
    // Poll errors belong to this operation; observations uses its own lt_error.
    // A later successful native call must never clear the actionable poll message.
    std::string detail=diagnostic;bool poll_snapshot_valid=false;
    if(runtime) { auto status=lt_runtime_poll(runtime,&s,&poll_error);poll_snapshot_valid=status==LT_OK;if(status!=LT_OK) { diagnostic=result(status,poll_error);detail=diagnostic; }
        else {
            if(s.state==LT_STOPPED && (s.dirty || !s.guest_shutdown_confirmed))s.state=LT_FAILED;
            if(s.state==LT_FAILED && diagnostic.empty())diagnostic="Guest exited without a confirmed clean shutdown (code "+std::to_string(s.engine_exit)+"); writable state retained. Stop this worker, then explicitly recover in a fresh worker. Diagnostics are retained.";
            // The writable state is dirty for the whole of every session, so it is not news while running.
            detail="frames "+std::to_string(s.frames);
        }
    }
    auto arr=env->NewObjectArray(18,env->FindClass("java/lang/String"),nullptr);
    env->SetObjectArrayElement(arr,0,string(env,states[starting ? (startup_stop_requested ? 4u : 1u) : (!diagnostic.empty() && !runtime ? 5u : std::min(s.state,5u))]));
    env->SetObjectArrayElement(arr,1,string(env,detail));
    env->SetObjectArrayElement(arr,2,string(env,diagnostic));env->SetObjectArrayElement(arr,3,string(env,engine_log_path));env->SetObjectArrayElement(arr,4,string(env,debug_qmp_path));
    env->SetObjectArrayElement(arr,5,string(env,graphics_provider_name()));
    env->SetObjectArrayElement(arr,6,string(env,graphics_provider_fixed?"1":"0"));
    env->SetObjectArrayElement(arr,7,string(env,debug_touch_trace_configured?"1":"0"));
    // Append facts from this same poll snapshot. Invalid/default fields never
    // establish a clean shutdown; the consumer must require index 8 == "1".
    env->SetObjectArrayElement(arr,8,string(env,poll_snapshot_valid?"1":"0"));
    env->SetObjectArrayElement(arr,9,string(env,std::to_string(s.guest_shutdown_confirmed)));
    env->SetObjectArrayElement(arr,10,string(env,std::to_string(s.dirty)));
    env->SetObjectArrayElement(arr,11,string(env,std::to_string(s.storage_failed)));
    env->SetObjectArrayElement(arr,12,string(env,std::to_string(s.engine_exit)));
    env->SetObjectArrayElement(arr,13,string(env,std::to_string(display_lease.inputGeneration(display_lease.currentOwner()))));
    env->SetObjectArrayElement(arr,14,string(env,std::to_string(display_lease.currentOwner())));
    env->SetObjectArrayElement(arr,15,string(env,debug_vcpu_accounting_configured?"1":"0"));
    env->SetObjectArrayElement(arr,16,string(env,experimental_tcg_ir_cache_requested?"1":"0"));
    // Guest frames produced, for the frame-rate readout; -1 without a valid poll.
    env->SetObjectArrayElement(arr,17,string(env,poll_snapshot_valid?std::to_string(s.frames):"-1"));return arr;
}
// Must hold guard. Routine attachment/detachment with no View-owned contacts
// never cancels an unrelated Home/Power pulse. A real held-contact handoff uses
// the existing full cancellation API, which also releases hardware buttons.
void refresh_display_release() {
    // An admitted UP/CANCEL can empty the local ledger while its guest release
    // is still queued. Refresh terminal success before deciding whether that
    // outstanding obligation needs cancellation; do so before minting/retiring.
    if(runtime && display_lease.pendingOwnedRelease()) {
        lt_input_status completed=LT_INIT(lt_input_status);lt_error observed=LT_INIT(lt_error);
        if(lt_runtime_input_status(runtime,&completed,&observed)==LT_OK && completed.state==2)
            display_lease.inputCompleted(completed.request_id);
    }
}
std::string cancel_display_contacts() {
    refresh_display_release();
    if(!display_lease.needsOwnedRelease())return "";
    lt_error error=LT_INIT(lt_error);
    const auto status=runtime?lt_runtime_cancel_input(runtime,&error):LT_OK;
    if(status==LT_OK)display_lease.contactsReleased();
    return result(status,error);
}
JNI(claimDisplay) jlong JNICALL Java_org_lighttouch_android_Native_claimDisplay(JNIEnv *,jobject) {
    ANativeWindow *retired=nullptr;uint64_t token=0;
    {
        std::lock_guard<std::mutex> lock(guard);
        const auto cancelled=cancel_display_contacts();
        if(!cancelled.empty()){diagnostic="Display handoff release failed: "+cancelled;return 0;}
        token=display_lease.claim();
        if(!token)return 0;
        ++surface_request;retired=window;window=nullptr;window_attached.store(0);
        ++surface_generation;++presentation_epoch;previous_present_ns=0;
    }
    if(retired)ANativeWindow_release(retired);
    return jlong(token);
}
JNI(ownsDisplay) jboolean JNICALL Java_org_lighttouch_android_Native_ownsDisplay(JNIEnv *,jobject,jlong lease) {
    std::lock_guard<std::mutex> lock(guard);return display_lease.owns(uint64_t(lease));
}
JNI(displayInputGeneration) jlong JNICALL Java_org_lighttouch_android_Native_displayInputGeneration(JNIEnv *,jobject,jlong lease) {
    std::lock_guard<std::mutex> lock(guard);return jlong(display_lease.inputGeneration(uint64_t(lease)));
}
JNI(attachDisplay) jstring JNICALL Java_org_lighttouch_android_Native_attachDisplay(JNIEnv *env,jobject,jlong lease,jobject surface) {
    DisplayLease::Request owner;uint64_t request;
    {
        std::lock_guard<std::mutex> lock(guard);
        owner=display_lease.begin(uint64_t(lease));
        if(!owner.owner)return string(env,retired_display);
        request=++surface_request;
    }
    // No control/Service lock spans platform conversion or reference release.
    ConvertedWindow<ANativeWindow,ANativeWindow_release> incoming(surface?ANativeWindow_fromSurface(env,surface):nullptr);
    const bool converted=incoming.get()!=nullptr;
    ANativeWindow *retired=nullptr;bool committed=false;
    {
        std::lock_guard<std::mutex> lock(guard);
        if(request==surface_request && display_lease.current(owner) && incoming.get()) {
            retired=window;window=incoming.take();committed=true;
            window_attached.store(1);
            ++surface_generation;++presentation_epoch;previous_present_ns=0;sequence=0;
        }
    }
    if(retired)ANativeWindow_release(retired);
    // incoming's rejected conversion reference is released outside guard.
    return string(env,committed?"":converted?retired_display:"Guest Surface conversion failed");
}
JNI(detachDisplay) void JNICALL Java_org_lighttouch_android_Native_detachDisplay(JNIEnv *,jobject,jlong lease) {
    ANativeWindow *retired=nullptr;
    {
        std::lock_guard<std::mutex> lock(guard);
        if(!display_lease.owns(uint64_t(lease)))return;
        const auto cancelled=cancel_display_contacts();
        if(!cancelled.empty())diagnostic="Display release failed: "+cancelled;
        display_lease.detach(uint64_t(lease));++surface_request;
        retired=window;window=nullptr;window_attached.store(0);
        ++surface_generation;++presentation_epoch;previous_present_ns=0;
    }
    if(retired)ANativeWindow_release(retired);
}
JNI(releaseDisplay) void JNICALL Java_org_lighttouch_android_Native_releaseDisplay(JNIEnv *,jobject,jlong lease) {
    ANativeWindow *retired=nullptr;
    {
        std::lock_guard<std::mutex> lock(guard);
        if(!display_lease.owns(uint64_t(lease)))return;
        const auto cancelled=cancel_display_contacts();
        if(!cancelled.empty())diagnostic="Display release failed: "+cancelled;
        // Failed cancellation must remain retryable before a new claim. Retire
        // only after release admission succeeded; window detachment is separate.
        if(cancelled.empty())display_lease.release(uint64_t(lease));
        else display_lease.detach(uint64_t(lease));
        ++surface_request;
        retired=window;window=nullptr;window_attached.store(0);
        ++surface_generation;++presentation_epoch;previous_present_ns=0;
    }
    if(retired)ANativeWindow_release(retired);
}
JNI(cancelDisplayInput) jstring JNICALL Java_org_lighttouch_android_Native_cancelDisplayInput(JNIEnv *env,jobject,jlong lease,jlong generation) {
    std::lock_guard<std::mutex> lock(guard);
    if(!display_lease.owns(uint64_t(lease)))return string(env,retired_display);
    if(generation && !display_lease.currentInput(uint64_t(lease),uint64_t(generation)))
        return string(env,"Display input generation retired");
    const auto cancelled=cancel_display_contacts();
    if(!cancelled.empty())diagnostic="Display contact release failed: "+cancelled;
    return string(env,cancelled);
}
JNI(backgroundDisplay) jstring JNICALL Java_org_lighttouch_android_Native_backgroundDisplay(JNIEnv *env,jobject,jlong lease) {
    std::lock_guard<std::mutex> lock(guard);
    if(!display_lease.owns(uint64_t(lease)))return string(env,retired_display);
    // Only the actual owner may apply the existing full background cleanup.
    lt_error error=LT_INIT(lt_error);
    if(runtime) {
        const auto cancelled=lt_runtime_cancel_input(runtime,&error);
        if(cancelled!=LT_OK)return string(env,result(cancelled,error));
    }
    display_lease.contactsReleased();
    ++presentation_epoch;previous_present_ns=0;sequence=0;
    if(starting){startup_pause_requested=true;return string(env,"");}
    return string(env,runtime?result(lt_runtime_pause(runtime,1,&error),error):"No active session");
}
JNI(pause) jstring JNICALL Java_org_lighttouch_android_Native_pause(JNIEnv *env,jobject,jboolean paused) {
    std::lock_guard<std::mutex> lock(guard);lt_error error=LT_INIT(lt_error);
    // An old in-flight failure cannot rewind the new epoch's cursor. Replay
    // the cache here so Resume does not depend on a newer guest repaint.
    ++presentation_epoch;previous_present_ns=0;sequence=0;
    if(starting){startup_pause_requested=paused;return string(env,"");}
    return string(env,runtime?result(lt_runtime_pause(runtime,paused,&error),error):"No active session");
}
JNI(stop) jstring JNICALL Java_org_lighttouch_android_Native_stop(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(guard);lt_error error=LT_INIT(lt_error);
    ++presentation_epoch;previous_present_ns=0;
    if(starting)startup_stop_requested=true;
    return string(env,runtime?result(lt_runtime_request_shutdown(runtime,&error),error):"");
}
JNI(button) jstring JNICALL Java_org_lighttouch_android_Native_button(JNIEnv *env,jobject,jint button,jboolean down) {
    std::lock_guard<std::mutex> lock(guard);lt_error error=LT_INIT(lt_error);
    return string(env,runtime?result(lt_runtime_button(runtime,button,down,&error),error):"No active session");
}
JNI(cancelInput) void JNICALL Java_org_lighttouch_android_Native_cancelInput(JNIEnv *,jobject) {
    std::lock_guard<std::mutex> lock(guard);lt_error error=LT_INIT(lt_error);
    if(!runtime || lt_runtime_cancel_input(runtime,&error)==LT_OK)display_lease.contactsReleased();
}
jstring touch_command(JNIEnv *env,uint64_t lease,jlong time,jintArray ids,jintArray phases,jfloatArray xy,jint width,jint height,jint rotation) {
    if(lease && !display_lease.owns(lease))return string(env,retired_display);
    const jsize count=env->GetArrayLength(ids);
    if(count<1 || count>jsize(LT_MAX_CONTACTS) || env->GetArrayLength(phases)!=count || env->GetArrayLength(xy)!=count*2) return string(env,"Invalid contact batch");
    if(!runtime) return string(env,"No active guest session; touch was not admitted");
    auto release_input=[&]() -> std::string {
        if(lease)return cancel_display_contacts();
        lt_error cancelled=LT_INIT(lt_error);
        const auto cancellation=lt_runtime_cancel_input(runtime,&cancelled);
        if(cancellation==LT_OK)display_lease.contactsReleased();
        return result(cancellation,cancelled);
    };
    if(!frame.width || !frame.height) {
        const auto cancellation=release_input();
        std::string failure="No active guest frame; gesture cancelled";
        if(!cancellation.empty())failure += "; input release failed: "+cancellation;
        return string(env,failure);
    }
    jint id[LT_MAX_CONTACTS], phase[LT_MAX_CONTACTS];jfloat coords[LT_MAX_CONTACTS*2];
    env->GetIntArrayRegion(ids,0,count,id);env->GetIntArrayRegion(phases,0,count,phase);env->GetFloatArrayRegion(xy,0,count*2,coords);
    lt_touch_batch batch=LT_INIT(lt_touch_batch);batch.timestamp_ns=time;batch.count=count;
    const auto view=viewport(width,height);lt_error error=LT_INIT(lt_error);
    for(jsize i=0;i<count;++i) {
        batch.contacts[i]=lt_contact LT_INIT(lt_contact);batch.contacts[i].id=id[i];batch.contacts[i].phase=phase[i];
        // A finger past the guest's edge (into a letterbox, or off the glass) holds at the nearest edge instead of
        // cancelling every finger's gesture, as a touchscreen that ends at the guest's border would.
        double px=coords[i*2],py=coords[i*2+1];
        if(std::isfinite(px) && std::isfinite(py)) {
            px=std::clamp(px,view.left,view.left+view.width);py=std::clamp(py,view.top,view.top+view.height);
        }
        auto status=lt_map_contact(px,py,view.left,view.top,view.width,view.height,0,&batch.contacts[i].x,&batch.contacts[i].y);
        if(status!=LT_OK) {
            const auto cancellation=release_input();
            return string(env,touch_mapping_rejection(status,cancellation));
        }
    }
    // Guest portrait is kept upright within Android's rotated view. Mapping receives
    // view-local pixels; applying display rotation again would rotate contacts twice.
    (void)rotation;
    refresh_display_release();
    auto status=lt_runtime_touch(runtime,&batch,&error);
    std::string failure=result(status,error);
    if(status!=LT_OK) {
        const auto cancellation=release_input();
        if(!cancellation.empty())failure += "; input release failed: "+cancellation;
    } else {
        display_lease.beginAdmittedBatch();
        if(lease)for(jsize i=0;i<count;++i)display_lease.admittedContact(id[i],phase[i]);
        // Read the real submitted ID while this JNI guard still serializes all
        // touch admissions. A separate error buffer cannot erase touch failures.
        lt_input_status submitted=LT_INIT(lt_input_status);lt_error identity_error=LT_INIT(lt_error);
        const auto identity=lt_runtime_input_status(runtime,&submitted,&identity_error);
        if(identity==LT_OK && submitted.request_id) {
            display_lease.submittedInput(submitted.request_id,lease);
            if(submitted.state==2)display_lease.inputCompleted(submitted.request_id);
        } else {
            failure="Touch request identity unavailable; gesture cancelled";
            lt_error cancelled=LT_INIT(lt_error);
            const auto cancellation=lt_runtime_cancel_input(runtime,&cancelled);
            if(cancellation==LT_OK)display_lease.contactsReleased();
            else failure += "; input release failed: "+result(cancellation,cancelled);
        }
    }
    if(!failure.empty())diagnostic=failure;
    return string(env,failure);
}
JNI(touch) jstring JNICALL Java_org_lighttouch_android_Native_touch(JNIEnv *env,jobject,jlong time,jintArray ids,jintArray phases,jfloatArray xy,jint width,jint height,jint rotation) {
    std::lock_guard<std::mutex> lock(guard);return touch_command(env,0,time,ids,phases,xy,width,height,rotation);
}
JNI(touchDisplay) jstring JNICALL Java_org_lighttouch_android_Native_touchDisplay(JNIEnv *env,jobject,jlong lease,jlong generation,jlong time,jintArray ids,jintArray phases,jfloatArray xy,jint width,jint height,jint rotation) {
    std::lock_guard<std::mutex> lock(guard);
    if(!display_lease.owns(uint64_t(lease)))return string(env,retired_display);
    // The fresh-DOWN getter is advisory; this guarded check closes the race with
    // any successful full cancel between that getter and Binder admission.
    if(!display_lease.currentInput(uint64_t(lease),uint64_t(generation)))
        return string(env,"Display input generation retired");
    if(runtime) {
        lt_runtime_status state=LT_INIT(lt_runtime_status);lt_error error=LT_INIT(lt_error);
        if(lt_runtime_poll(runtime,&state,&error)==LT_OK && state.state==LT_PAUSED)
            return string(env,"Display input is paused");
    }
    return touch_command(env,uint64_t(lease),time,ids,phases,xy,width,height,rotation);
}
JNI(motion) jstring JNICALL Java_org_lighttouch_android_Native_motion(JNIEnv *env,jobject,jint kind,jlong time,jfloatArray xyz,jint rotation) {
    std::lock_guard<std::mutex> lock(guard);if(!runtime)return string(env,"");
    if(env->GetArrayLength(xyz)!=3)return string(env,"Invalid sensor vector");
    jfloat v[3];env->GetFloatArrayRegion(xyz,0,3,v);
    lt_motion sample=LT_INIT(lt_motion);sample.timestamp_ns=time;sample.kinds=kind;
    double *target=kind==2?sample.angular_velocity:sample.acceleration;
    // Android SensorManager is in natural device axes; shared domain rotates into guest axes.
    for(int i=0;i<3;++i)target[i]=v[i];
    timespec now;clock_gettime(CLOCK_BOOTTIME,&now);
    lt_motion portrait=LT_INIT(lt_motion);lt_error error=LT_INIT(lt_error);
    auto status=lt_map_motion(rotation,time,uint64_t(now.tv_sec)*1000000000+now.tv_nsec,&sample,&portrait,&error);
    if(status==LT_OK)status=lt_runtime_motion(runtime,&portrait,&error);
    // An unavailable guest gyro is a capability, never fabricated as acceleration.
    // Dropping a stale/rate-limited live motion sample is safe; held contacts and
    // lifecycle admissions never use this policy.
    return string(env,status==LT_UNSUPPORTED || status==LT_AGAIN?"":result(status,error));
}
JNI(capture) jobject JNICALL Java_org_lighttouch_android_Native_capture(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(guard);if(!frame.width||!frame.height)return nullptr;
    jclass bitmap=env->FindClass("android/graphics/Bitmap"),config=env->FindClass("android/graphics/Bitmap$Config");
    auto argb=env->GetStaticObjectField(config,env->GetStaticFieldID(config,"ARGB_8888","Landroid/graphics/Bitmap$Config;"));
    auto create=env->GetStaticMethodID(bitmap,"createBitmap","(IILandroid/graphics/Bitmap$Config;)Landroid/graphics/Bitmap;");
    jobject result=env->CallStaticObjectMethod(bitmap,create,jint(frame.width),jint(frame.height),argb);
    if(env->ExceptionCheck()||!result)return nullptr;
    AndroidBitmapInfo info;void *data=nullptr;
    if(AndroidBitmap_getInfo(env,result,&info)!=ANDROID_BITMAP_RESULT_SUCCESS||AndroidBitmap_lockPixels(env,result,&data)!=ANDROID_BITMAP_RESULT_SUCCESS)return nullptr;
    for(uint32_t y=0;y<frame.height;++y)for(uint32_t x=0;x<frame.width;++x) {
        const uint8_t *src=pixels.data()+y*frame.stride+x*4;auto *dst=static_cast<uint8_t *>(data)+y*info.stride+x*4;
        dst[0]=src[2];dst[1]=src[1];dst[2]=src[0];dst[3]=255;
    }
    AndroidBitmap_unlockPixels(env,result);return result;
}
JNI(audio) jint JNICALL Java_org_lighttouch_android_Native_audio(JNIEnv *env,jobject,jshortArray buffer) {
    std::lock_guard<std::mutex> lock(guard);if(!runtime)return 0;
    std::vector<int16_t> data(env->GetArrayLength(buffer));lt_error error=LT_INIT(lt_error);
    const auto status=lt_runtime_pcm(runtime,data.data(),uint32_t(data.size()/2),&pcm,&error);
    if(status==LT_AGAIN)return 0;
    if(status!=LT_OK){++pcm_failures;return -jint(status);}
    if(pcm.channels>2||pcm.channels<1||pcm.frames*pcm.channels>data.size()){++pcm_failures;return -jint(LT_INVALID);}
    const jint count=pcm.frames*pcm.channels;env->SetShortArrayRegion(buffer,0,count,data.data());return count;
}
JNI(audioInfo) jintArray JNICALL Java_org_lighttouch_android_Native_audioInfo(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(guard);jint data[2]={jint(pcm.sample_rate),jint(pcm.channels)};
    jintArray arr=env->NewIntArray(2);env->SetIntArrayRegion(arr,0,2,data);return arr;
}

JNI(profile) jstring JNICALL Java_org_lighttouch_android_Native_profile(JNIEnv *env,jobject,jstring path) {
    std::unique_ptr<ProfileWriter> stopped;
    std::unique_lock<std::mutex> lock(guard);Utf text(env,path);lt_error error=LT_INIT(lt_error);
    const bool start=text.value && *text.value;
    auto status=runtime?lt_runtime_profile(runtime,text.value,&error):LT_AGAIN;
    if(status==LT_OK && start) {
        const std::string capture_path=text.value;
        const std::string present_path=capture_path.substr(0,capture_path.find_last_of('/')+1)+"presentation.tsv";
        FILE *output=fopen(present_path.c_str(),"wx");
        if(!output) {
            lt_error stop_error=LT_INIT(lt_error);lt_runtime_profile(runtime,"",&stop_error);
            return string(env,"Could not create Android presentation capture");
        }
        setvbuf(output,nullptr,_IOFBF,65536);
        const char *sync=getenv("LT_PROFILE_SYNC");
        const bool synchronous=sync && strcmp(sync,"off") && strcmp(sync,"0");
        try { profile_presentation=std::make_unique<ProfileWriter>(output,synchronous); }
        catch(...) {
            fclose(output);
            lt_error stop_error=LT_INIT(lt_error);lt_runtime_profile(runtime,"",&stop_error);
            return string(env,"Could not start Android presentation writer");
        }
        profile_presentation_started=monotonic_ns();
        profile_presentation->append("# P post-complete-ns serial guest-due-ns observe-begin guard-acquired copy-begin copy-end lock-begin lock-end scale-end post-begin post-end commit-ns\n");
    }
    if(!start) stopped=std::move(profile_presentation);
    const auto message=result(status,error);
    lock.unlock();
    if(stopped && !stopped->finish() && status==LT_OK)
        return string(env,"Could not finish Android presentation capture (storage or capture limit)");
    return string(env,message);
}
JNI(profileStatus) jint JNICALL Java_org_lighttouch_android_Native_profileStatus(JNIEnv *,jobject) {
    std::lock_guard<std::mutex> lock(guard);lt_error error=LT_INIT(lt_error);int32_t state=0;
    if(profile_presentation && profile_presentation->failed())return -1;
    if(!runtime || lt_runtime_profile_status(runtime,&state,&error)!=LT_OK)return 0;
    return state;
}

JNI(guestRequest) jobjectArray JNICALL Java_org_lighttouch_android_Native_guestRequest(JNIEnv *env,jobject,jint operation,jstring argument,jint timeout) {
    std::lock_guard<std::mutex> lock(guard);Utf text(env,argument);lt_error error=LT_INIT(lt_error);uint64_t id=0;
    lt_guest_request request=LT_INIT(lt_guest_request);request.operation=operation;request.timeout_ms=timeout;request.argument=text.value;
    auto status=runtime?lt_runtime_guest_request(runtime,&request,&id,&error):LT_AGAIN;
    auto output=env->NewObjectArray(3,env->FindClass("java/lang/String"),nullptr);
    env->SetObjectArrayElement(output,0,string(env,std::to_string(id)));env->SetObjectArrayElement(output,1,string(env,result(status,error)));
    env->SetObjectArrayElement(output,2,string(env,std::to_string(status)));return output;
}
JNI(guestPoll) jobjectArray JNICALL Java_org_lighttouch_android_Native_guestPoll(JNIEnv *env,jobject,jlong id) {
    std::lock_guard<std::mutex> lock(guard);lt_error error=LT_INIT(lt_error);lt_guest_result completed=LT_INIT(lt_guest_result);
    auto status=runtime?lt_runtime_guest_poll(runtime,id,&completed,&error):LT_AGAIN;
    auto output=env->NewObjectArray(4,env->FindClass("java/lang/String"),nullptr);
    std::string values[]={std::to_string(completed.state),std::to_string(completed.guest_status),status==LT_OK?std::string(completed.output,completed.bytes):"",result(status,error)};
    for(int i=0;i<4;++i)env->SetObjectArrayElement(output,i,string(env,values[i]));return output;
}
JNI(guestCancel) void JNICALL Java_org_lighttouch_android_Native_guestCancel(JNIEnv *,jobject,jlong id) {
    std::lock_guard<std::mutex> lock(guard);lt_error error=LT_INIT(lt_error);if(runtime)lt_runtime_guest_cancel(runtime,id,&error);
}

JNI(observations) jlongArray JNICALL Java_org_lighttouch_android_Native_observations(JNIEnv *env,jobject) {
    std::lock_guard<std::mutex> lock(guard);
    lt_runtime_io_stats shared=LT_INIT(lt_runtime_io_stats);lt_error error=LT_INIT(lt_error);
    if(runtime)lt_runtime_io_statistics(runtime,&shared,&error);
    lt_runtime_diagnostics observer=LT_INIT(lt_runtime_diagnostics);
    lt_error observer_error=LT_INIT(lt_error),agent_error=LT_INIT(lt_error);
    const auto observer_status=runtime?lt_runtime_diagnose(runtime,&observer,&observer_error):LT_AGAIN;
    uint32_t live_agent=0;
    const auto agent_status=runtime?lt_runtime_guest_status(runtime,&live_agent,&agent_error):LT_AGAIN;
    // These samples use separate error buffers and never replace poll/control
    // failures. No heartbeat age or socket endpoint exists in this ABI.
    // Existing runtime/control observations still use guard. These added phase
    // loads add no wait, platform query or retry. All fields are atomic; a
    // changed/odd revision marks this one in-flight stage sample unstable.
    const auto phase_revision_before=present_phase_revision.load();
    const auto phase=present_phase.load(),phase_entry=present_phase_entry_ns.load();
    const auto phase_generation=present_phase_generation.load(),phase_epoch=present_phase_epoch.load();
    const auto phase_revision_after=present_phase_revision.load();
    const bool phase_stable=phase_revision_before==phase_revision_after && !(phase_revision_after&1u);
    jlong values[]={jlong(monotonic_ns()),jlong(presented_frames),jlong(first_present_ns),jlong(last_present_ns),
        jlong(max_present_gap_ns),jlong(max_present_work_ns),jlong(window_failures),jlong(frame_copy_failures),
        jlong(surface_generation),jlong(frame.sequence),jlong(frame.timestamp_ns),jlong(pcm_failures),jlong(pcm.dropped_frames),
        jlong(present_intervals[0]),jlong(present_intervals[1]),jlong(present_intervals[2]),jlong(present_intervals[3]),jlong(present_intervals[4]),jlong(present_intervals[5]),jlong(shared.frame_count),jlong(shared.frame_sequence),jlong(shared.frame_timestamp_ns),
        jlong(shared.frame_published_monotonic_ns),jlong(shared.pcm_captured_frames),jlong(shared.pcm_pulled_frames),jlong(shared.pcm_dropped_frames),
        jlong(shared.input_requests),jlong(shared.pcm_queued_packets),first_lock_error,last_lock_error,first_post_error,last_post_error,last_geometry_error,
        jlong(geometry_failures),last_buffer_format,last_buffer_width,last_buffer_height,last_buffer_stride,jlong(invalid_buffers),jlong(first_window_error_ns),jlong(observer_status),jlong(observer.engine_ready),
        jlong(observer.display_sleeping),jlong(observer.backlight_level),jlong(agent_status),jlong(live_agent),
        jlong(observer.shutdown_phase),jlong(observer.shutdown_route),jlong(observer.shutdown_reply_status),
        jlong(observer.shutdown_reply_received),jlong(frame.width),jlong(frame.height),
        jlong(frame_sample_pixels),jlong(frame_nonzero_rgb_samples),jlong(frame_rgb_sample_sum),
        jlong(window_attached.load()),jlong(presentation_epoch),jlong(phase),jlong(phase_entry),
        jlong(phase_generation),jlong(phase_epoch),jlong(phase_revision_after),jlong(phase_stable),
        jlong(ignored_input_rejections),jlong(last_ignored_input_request)};
    auto array=env->NewLongArray(sizeof(values)/sizeof(values[0]));
    env->SetLongArrayRegion(array,0,sizeof(values)/sizeof(values[0]),values);return array;
}

JNI(shutdownGraceMillis) jint JNICALL Java_org_lighttouch_android_Native_shutdownGraceMillis(JNIEnv *,jobject) {
    return jint(LT_SHUTDOWN_GRACE_MS);
}


JNI(debugSelectGraphicsProvider) jstring JNICALL Java_org_lighttouch_android_Native_debugSelectGraphicsProvider(JNIEnv *env,jobject,jint choice) {
#ifndef LT_ANDROID_DEBUG
    return string(env,"Graphics provider diagnosis is unavailable in this build");
#else
    std::lock_guard<std::mutex> lock(guard);
    if(choice!=0 && choice!=1)return string(env,"Unsupported fixed graphics provider");
    if(runtime || starting)return string(env,"Graphics provider selection requires a fresh unused worker");
    if(graphics_provider_fixed)return string(env,!graphics_provider_failure.empty()?graphics_provider_failure:
        unsigned(choice)==graphics_provider_choice?"":"Graphics provider is fixed; use a fresh worker to change it");
#if !defined(__x86_64__) && !defined(__aarch64__)
    if(choice==1)return string(env,"The packaged ANGLE experiment requires ARM64 or x86-64");
#endif
    graphics_provider_choice=unsigned(choice);graphics_provider_fixed=true;
    if(choice==1) {
        // Fixed public SONAMEs resolve inside this app's linker namespace. No
        // caller path, environment variable name or general library loader.
        const char *names[]={"libGLESv2_lighttouch_angle.so","libEGL_lighttouch_angle.so","libGLESv1_CM_lighttouch_angle.so"};
        for(unsigned i=0;i<3;++i) {
            angle_provider_handles[i]=dlopen(names[i],RTLD_NOW|RTLD_LOCAL);
            if(!angle_provider_handles[i]) {
                const char *reason=dlerror();
                graphics_provider_failure="Packaged ANGLE provider is unavailable: "+std::string(reason?reason:"library load failed").substr(0,512)+"; start a fresh worker after correcting the package";
                return string(env,graphics_provider_failure);
            }
        }
        if(setenv("LT_GLES_DRIVER_DIAGNOSTIC","1",1)!=0) {
            graphics_provider_failure="Cannot retain graphics provider diagnostics; use a fresh worker";return string(env,graphics_provider_failure);
        }
    }
    return string(env,"");
#endif
}

JNI(debugEnableTouchTrace) jstring JNICALL Java_org_lighttouch_android_Native_debugEnableTouchTrace(JNIEnv *env,jobject) {
#ifdef LT_ANDROID_DEBUG
    std::lock_guard<std::mutex> lock(guard);
    if(runtime || starting || debug_touch_trace_closed)return string(env,"Touch trace selection requires a fresh unused worker");
    debug_touch_trace_armed=true;return string(env,"");
#else
    return string(env,"Debug diagnosis is unavailable in this build");
#endif
}
// Frames in each of the last 240 complete 250 ms buckets, oldest first (0 = none published).
JNI(guestFrameBuckets) jintArray JNICALL Java_org_lighttouch_android_Native_guestFrameBuckets(JNIEnv *env,jobject) {
    constexpr int n=240;jint values[n];const uint64_t now=monotonic_ns()/fps_bucket_ns;
    for(int i=0;i<n;i++){
        const uint64_t id=now-n+i;
        values[i]=fps_bucket_id[id%fps_buckets].load(std::memory_order_acquire)==id?jint(fps_bucket_frames[id%fps_buckets].load(std::memory_order_relaxed)):0;
    }
    auto array=env->NewIntArray(n);env->SetIntArrayRegion(array,0,n,values);return array;
}
// Frames newer than since as (sequence, published, presented, presenter work) quads,
// oldest first, all CLOCK_MONOTONIC ns. presented is 0 until (unless) it was posted.
JNI(frameTimeline) jlongArray JNICALL Java_org_lighttouch_android_Native_frameTimeline(JNIEnv *env,jobject,jlong since) {
    const uint64_t latest=timeline_latest.load(std::memory_order_acquire);
    uint64_t first=latest>=timeline_size?latest-timeline_size+1:1;
    first=std::max(first,uint64_t(since)+1);
    std::vector<jlong> out;
    if(latest>=first)out.reserve(size_t(latest-first+1)*4);
    for(uint64_t sequence=first;sequence<=latest && sequence;++sequence) {
        auto &slot=timeline[sequence%timeline_size];
        if(slot.sequence.load(std::memory_order_acquire)!=sequence)continue;
        const uint64_t published=slot.published.load(std::memory_order_relaxed);
        const uint64_t presented=slot.presented.load(std::memory_order_acquire);
        const uint64_t work=slot.work.load(std::memory_order_relaxed);
        if(slot.sequence.load(std::memory_order_acquire)!=sequence)continue;
        out.insert(out.end(),{jlong(sequence),jlong(published),jlong(presented),jlong(work)});
    }
    auto array=env->NewLongArray(jsize(out.size()));
    if(array && !out.empty())env->SetLongArrayRegion(array,0,jsize(out.size()),out.data());
    return array;
}
JNI(setPresentVisible) void JNICALL Java_org_lighttouch_android_Native_setPresentVisible(JNIEnv *,jobject,jboolean visible) {
    present_visible.store(visible,std::memory_order_relaxed);
    {std::lock_guard<std::mutex> lock(frame_wake_mutex);++frame_wake_sequence;}
    frame_wake.notify_one();
}
JNI(setPresentScale) void JNICALL Java_org_lighttouch_android_Native_setPresentScale(JNIEnv *,jobject,jint mode) {
    if(mode>=ScaleSmooth && mode<=ScaleCpu)present_scale.store(mode,std::memory_order_relaxed);
}
JNI(debugArmVcpuAccounting) jstring JNICALL Java_org_lighttouch_android_Native_debugArmVcpuAccounting(JNIEnv *env,jobject) {
#ifdef LT_ANDROID_DEBUG
    std::lock_guard<std::mutex> lock(guard);
    if(runtime || starting || worker_start_consumed)
        return string(env,"vCPU accounting requires a fresh unused worker");
    // The worker may only select this fixed existing engine diagnostic. No
    // caller interval, environment name, command or output path is accepted.
    if(setenv("IT_VCPU_ACCT","5",1)!=0)
        return string(env,"Cannot configure vCPU accounting: "+std::string(std::strerror(errno)).substr(0,256));
    debug_vcpu_accounting_configured=true;return string(env,"");
#else
    return string(env,"Debug diagnosis is unavailable in this build");
#endif
}
JNI(debugSelectTcgIrCache) jstring JNICALL Java_org_lighttouch_android_Native_debugSelectTcgIrCache(JNIEnv *env,jobject,jboolean enabled) {
#ifdef LT_ANDROID_DEBUG
    std::lock_guard<std::mutex> lock(guard);
    if(runtime || starting || worker_start_consumed)
        return string(env,"Experimental cache selection requires a fresh unused worker");
    // No environment mutation here: first start selects the actual local record.
    experimental_tcg_ir_cache_requested=enabled==JNI_TRUE;
    return string(env,"");
#else
    (void)enabled;
    return string(env,"Experimental cache selection is unavailable in this build");
#endif
}
JNI(debugArmQmp) jstring JNICALL Java_org_lighttouch_android_Native_debugArmQmp(JNIEnv *env,jobject) {
#ifdef LT_ANDROID_DEBUG
    std::lock_guard<std::mutex> lock(guard);
    if(runtime || starting)return string(env,"Debug diagnosis requires a fresh unused worker");
    // Fixed copied-buffer metrics only: no caller environment name or value,
    // and no change to default workers or release builds.
    if(setenv("IT_GLES_FRAME_METRICS","1",1)!=0)
        return string(env,"Cannot enable copied-frame diagnosis: "+std::string(std::strerror(errno)).substr(0,256));
    debug_qmp_armed=true;return string(env,"");
#else
    return string(env,"Debug diagnosis is unavailable in this build");
#endif
}
JNI(debugQmpSnapshot) jobjectArray JNICALL Java_org_lighttouch_android_Native_debugQmpSnapshot(JNIEnv *env,jobject) {
    std::string failure,path;
#ifdef LT_ANDROID_DEBUG
    std::string endpoint;
    {
        std::lock_guard<std::mutex> lock(guard);endpoint=debug_qmp_path;
        if(!endpoint.empty() && !engine_log_path.empty())path=engine_log_path.substr(0,engine_log_path.find_last_of('/'))+
            "/qmp-snapshot-"+std::to_string(getpid())+"-"+std::to_string(monotonic_ns())+".jsonl";
    }
    if(endpoint.empty() || path.empty())failure="Debug endpoint was not enabled before this session";
    else {
        const int fd=open(path.c_str(),O_CREAT|O_EXCL|O_WRONLY|O_CLOEXEC,0600);
        if(fd<0)failure="Cannot retain private debug snapshot";
        else {
            // No session/Surface mutex is held across the bounded socket calls.
            const int result=lighttouch_debug_qmp_snapshot(endpoint.c_str(),fd);
            const int saved=errno;close(fd);
            if(result)failure="Read-only debug snapshot failed: "+std::string(std::strerror(saved));
        }
    }
#else
    failure="Debug diagnosis is unavailable in this build";
#endif
    auto result=env->NewObjectArray(2,env->FindClass("java/lang/String"),nullptr);
    env->SetObjectArrayElement(result,0,string(env,failure));env->SetObjectArrayElement(result,1,string(env,path));return result;
}
