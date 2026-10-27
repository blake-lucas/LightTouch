/* Force-included (build.rs) into the vendored iBoot32Patcher sources: its diagnostics go to stderr, not stdout,
 * so they never land in firmwarekit's JSON Lines event stream. <stdio.h> comes first, under its own name (mingw may
 * define printf inline there); the sources' later includes of it are no-ops, and their printf calls become the
 * shim's (iboot32_quiet.c). No patching logic changes. */
#ifndef LT_IBOOT32_QUIET_H
#define LT_IBOOT32_QUIET_H
#include <stdio.h>
int lt_iboot32_printf(const char *format, ...);
#define printf lt_iboot32_printf
#endif
