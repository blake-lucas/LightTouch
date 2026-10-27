#ifndef LT_CACTIVATION_H
#define LT_CACTIVATION_H
#include <stdint.h>
#include <stddef.h>
typedef struct {
    const char *strategy;
    const char *isa;
    size_t offset;
    unsigned width;
    uint8_t original[16];
    uint8_t replacement[16];
} LTActivationReport;
// As lt_activate, with the recognized operation recorded for preparation provenance.
int lt_activate_report(uint8_t *bytes, size_t size, LTActivationReport *report, const char **error);
// Returns 1 for a signed-era activation path, 2 for the pre-signing 1.x path,
// or 0 with a static error string and the buffer unchanged.
int lt_activate(uint8_t *bytes, size_t size, const char **error);
#endif
