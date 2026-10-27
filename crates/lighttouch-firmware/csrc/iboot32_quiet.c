/* The vendored iBoot32Patcher's printf (iboot32_quiet.h): to stderr. */
#include <stdarg.h>
#include <stdio.h>

int lt_iboot32_printf(const char *format, ...) {
    va_list ap;
    va_start(ap, format);
    int n = vfprintf(stderr, format, ap);
    va_end(ap);
    return n;
}
