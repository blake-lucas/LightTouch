// sizeof() of every ABI struct as the C compiler lays it out, for the Rust layout test.
#include "lighttouch.h"
#include <stdint.h>
#define S(t) (uint32_t)sizeof(t)
uint32_t lt_rust_layout(uint32_t *out, uint32_t capacity) {
    const uint32_t sizes[] = {
        S(lt_error), S(lt_library_config), S(lt_record_info), S(lt_import_config), S(lt_library_import_config),
        S(lt_finalize_import_config), S(lt_device_config), S(lt_device_info), S(lt_board_info), S(lt_boot_config),
        S(lt_runtime_status), S(lt_runtime_config), S(lt_runtime_diagnostics), S(lt_runtime_io_stats),
        S(lt_runtime_storage_stats), S(lt_frame_info), S(lt_contact), S(lt_touch_batch), S(lt_input_status), S(lt_motion), S(lt_pcm_info),
        S(lt_guest_request), S(lt_guest_result)};
    const uint32_t count = sizeof(sizes) / sizeof(sizes[0]);
    for (uint32_t i = 0; i < count && i < capacity; i++)
        out[i] = sizes[i];
    return count;
}
