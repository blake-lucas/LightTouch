/* Pattern-based local activation for emulated legacy iOS. No firmware offsets.
 * Recognizes a guarded development activation path in 32-bit ARM Mach-O files.
 * Uses a deliberately small instruction whitelist, not a general disassembler.
 */
#define _POSIX_C_SOURCE 200809L
#define _DARWIN_C_SOURCE
#define _DEFAULT_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define LIMIT (128u * 1024u * 1024u)
#define NO_OFF SIZE_MAX
static const char message[] = "Short circuiting activation state to Activated.";
static const char function[] = "dealwith_activation";
typedef struct {
    uint32_t va, size, off;
} Segment;
typedef struct {
    uint8_t *bytes;
    size_t size;
    Segment segments[64];
    size_t count;
    uint32_t text_va, text_size, text_off, message_va;
} Image;
typedef struct {
    size_t off;
    uint32_t va, target;
    unsigned width;
    const char *isa;
    uint8_t replacement[16];
    bool legacy, shared_no_record, conditional;
} Match;
#ifdef LT_ACTIVATION_LIBRARY
#include <setjmp.h>
#include "CActivation.h"
static _Thread_local jmp_buf failure;
static _Thread_local const char *failure_message;
#endif
static void fail(const char *s) {
#ifdef LT_ACTIVATION_LIBRARY
    failure_message = s;
    longjmp(failure, 1);
#else
    fprintf(stderr, "activation: %s\n", s);
    exit(1);
#endif
}
static uint16_t u16(const uint8_t *p) { return (uint16_t)(p[0] | p[1] << 8); }
static uint32_t u32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}
static bool fits(size_t off, size_t n, size_t size) { return off <= size && n <= size - off; }
static bool named(const uint8_t *p, const char *s) {
    size_t n = strlen(s);
    return n < 16 && !memcmp(p, s, n) && p[n] == 0;
}
static size_t fileoff(const Image *m, uint32_t va, size_t n) {
    size_t result = NO_OFF;
    for (size_t i = 0; i < m->count; i++) {
        Segment s = m->segments[i];
        if (va >= s.va && fits((uint64_t)va - s.va, n, s.size)) {
            if (result != NO_OFF)
                return NO_OFF;
            result = (size_t)s.off + va - s.va;
        }
    }
    return result;
}
static bool word(const Image *m, uint32_t va, uint32_t *v) {
    size_t o = fileoff(m, va, 4);
    if (o == NO_OFF)
        return false;
    *v = u32(m->bytes + o);
    return true;
}
static bool text_address(const Image *m, uint32_t va, unsigned n) {
    return va >= m->text_va && fits((uint64_t)va - m->text_va, n, m->text_size);
}
static void parse(Image *m) {
    const uint8_t *d = m->bytes;
    size_t n = m->size;
    if (n < 28 || u32(d) != 0xfeedface || u32(d + 4) != 12 || u32(d + 12) != 2)
        fail("expected a thin little-endian 32-bit ARM Mach-O executable");
    uint32_t count = u32(d + 16), commands = u32(d + 20);
    if (!fits(28, commands, n) || count > commands / 8)
        fail("invalid load commands");
    size_t o = 28, end = 28 + (size_t)commands;
    unsigned texts = 0;
    for (uint32_t i = 0; i < count; i++) {
        if (!fits(o, 8, end))
            fail("truncated load command");
        uint32_t cmd = u32(d + o), len = u32(d + o + 4);
        if (len < 8 || len % 4 || !fits(o, len, end))
            fail("invalid load command size");
        if (cmd == 1) {
            if (len < 56)
                fail("truncated segment");
            uint32_t va = u32(d + o + 24), vs = u32(d + o + 28), off = u32(d + o + 32), fs = u32(d + o + 36),
                     ns = u32(d + o + 48);
            if (ns > (len - 56) / 68 || !fits(off, fs, n) || fs > vs || (uint64_t)va + vs > UINT64_C(0x100000000))
                fail("invalid segment bounds");
            if (fs) {
                if (m->count == 64)
                    fail("too many segments");
                for (size_t k = 0; k < m->count; k++) {
                    Segment s = m->segments[k];
                    if ((uint64_t)va < s.va + (uint64_t)s.size && (uint64_t)s.va < va + (uint64_t)fs)
                        fail("overlapping mapped segments");
                }
                m->segments[m->count++] = (Segment){va, fs, off};
            }
            for (uint32_t j = 0; j < ns; j++) {
                const uint8_t *s = d + o + 56 + 68 * j;
                if (named(s, "__text") && named(s + 16, "__TEXT")) {
                    uint32_t a = u32(s + 32), z = u32(s + 36), f = u32(s + 40);
                    if (!named(d + o + 8, "__TEXT") || a < va || f < off || !z || !fits((uint64_t)a - va, z, fs) ||
                        !fits((uint64_t)f - off, z, fs) || (uint64_t)a - va != (uint64_t)f - off)
                        fail("invalid text section");
                    m->text_va = a;
                    m->text_size = z;
                    m->text_off = f;
                    texts++;
                }
            }
        }
        o += len;
    }
    if (o != end || texts != 1)
        fail("expected exactly one valid text section");
    size_t found = NO_OFF;
    unsigned hits = 0;
    for (size_t i = 0; fits(i, sizeof(message), n); i++)
        if (!memcmp(d + i, message, sizeof(message))) {
            found = i;
            hits++;
        }
    if (hits > 1)
        fail("ambiguous activation log strings");
    if (!hits)
        return;
    hits = 0;
    for (size_t i = 0; i < m->count; i++) {
        Segment s = m->segments[i];
        if (found >= s.off && fits(found - s.off, sizeof(message), s.size)) {
            m->message_va = s.va + (uint32_t)(found - s.off);
            hits++;
        }
    }
    if (hits != 1)
        fail("activation string has no unique mapped address");
}
static bool cstring(const Image *m, uint32_t va, const char *s) {
    size_t n = strlen(s) + 1, o = fileoff(m, va, n);
    return o != NO_OFF && !memcmp(m->bytes + o, s, n);
}
static bool log_message(const Image *m, uint32_t va) {
    if (!m->message_va)
        return false;
    if (va == m->message_va)
        return true;
    size_t o = fileoff(m, va, 16);
    /* Constant CFString: validate ASCII flags, character pointer and length. */
    return o != NO_OFF && u32(m->bytes + o + 4) == 0x7c8 && u32(m->bytes + o + 8) == m->message_va &&
           u32(m->bytes + o + 12) == sizeof(message) - 1;
}
/* Accept only straight-line argument construction ending in a direct log call.
 * All register values used as evidence must have been established in this block. */
static bool log_block(const Image *m, uint32_t pc, bool thumb, uint32_t *after) {
    uint32_t regs[16] = {0};
    unsigned known = 0;
    for (unsigned step = 0; step < 16; step++) {
        if (!text_address(m, pc, thumb ? 2 : 4))
            return false;
        size_t o = fileoff(m, pc, thumb ? 2 : 4);
        if (o == NO_OFF)
            return false;
        uint32_t ins = thumb ? u16(m->bytes + o) : u32(m->bytes + o);
        unsigned rd;
        uint32_t val;
        if (thumb) {
            if ((ins & 0xfbf0) == 0xf240 || (ins & 0xfbf0) == 0xf2c0) { /* MOVW/MOVT T3 */
                if (!text_address(m, pc, 4))
                    return false;
                uint16_t hi = u16(m->bytes + o + 2);
                rd = (hi >> 8) & 15;
                if ((hi & 0x8000) || rd == 15)
                    return false;
                uint32_t imm = ((ins & 15) << 12) | (((ins >> 10) & 1) << 11) | (((hi >> 12) & 7) << 8) | (hi & 255);
                if ((ins & 0xfbf0) == 0xf2c0) {
                    if (!(known & (1u << rd)))
                        return false;
                    regs[rd] = (regs[rd] & 65535) | (imm << 16);
                } else
                    regs[rd] = imm;
                known |= 1u << rd;
                pc += 4;
            } else if ((ins & 0xf800) == 0x2000) { /* MOVS immediate */
                rd = (ins >> 8) & 7;
                regs[rd] = ins & 255;
                known |= 1u << rd;
                pc += 2;
            } else if ((ins & 0xf800) == 0x4800) { /* LDR literal T1 */
                rd = (ins >> 8) & 7;
                if (!word(m, ((pc + 4) & ~3u) + 4 * (ins & 255), &val))
                    return false;
                regs[rd] = val;
                known |= 1u << rd;
                pc += 2;
            } else if (ins == 0xf8df || ins == 0xf85f) { /* LDR literal T2 */
                if (!text_address(m, pc, 4))
                    return false;
                uint16_t hi = u16(m->bytes + o + 2);
                rd = hi >> 12;
                if (rd == 15)
                    return false;
                uint32_t addr = (pc + 4) & ~3u;
                addr = ins == 0xf8df ? addr + (hi & 4095) : addr - (hi & 4095);
                if (!word(m, addr, &val))
                    return false;
                regs[rd] = val;
                known |= 1u << rd;
                pc += 4;
            } else if ((ins & 0xf800) == 0x6800) { /* LDR immediate T1 (9A5220p loads a pointer
                                                    * between the arguments): never evidence */
                known &= ~(1u << (ins & 7));
                pc += 2;
            } else if ((ins & 0xff00) == 0x4400) { /* ADD high register, PC */
                rd = (ins & 7) | ((ins >> 4) & 8);
                unsigned rm = (ins >> 3) & 15;
                if (rm != 15 || rd == 15 || !(known & (1u << rd)))
                    return false;
                regs[rd] += pc + 4;
                pc += 2;
            } else if ((ins & 0xf800) == 0xf000 && text_address(m, pc, 4) &&
                       (u16(m->bytes + o + 2) & 0xd000) == 0xd000) {
                uint16_t hi = u16(m->bytes + o + 2); /* BL immediate T1, includes Thumb-1 form */
                uint32_t s = (ins >> 10) & 1, j1 = (hi >> 13) & 1, j2 = (hi >> 11) & 1;
                uint32_t imm =
                    (s << 24) | ((!(j1 ^ s)) << 23) | ((!(j2 ^ s)) << 22) | ((ins & 1023) << 12) | ((hi & 2047) << 1);
                int32_t delta = (int32_t)(imm << 7) >> 7;
                if (!text_address(m, pc + 4 + (uint32_t)delta, 2))
                    return false;
                *after = pc + 4;
                break;
            } else
                return false;
        } else {
            if ((ins & 0xff7f0000) == 0xe51f0000) { /* unconditional LDR literal */
                rd = (ins >> 12) & 15;
                if (rd == 15)
                    return false;
                uint32_t addr = pc + 8;
                addr = ins & (1u << 23) ? addr + (ins & 4095) : addr - (ins & 4095);
                if (!word(m, addr, &val))
                    return false;
                regs[rd] = val;
                known |= 1u << rd;
                pc += 4;
            } else if ((ins & 0xfff00000) == 0xe0800000 && !(ins & 0x00000ff0)) { /* ADD register, no shifts */
                rd = (ins >> 12) & 15;
                unsigned rn = (ins >> 16) & 15, rm = ins & 15;
                if (rd == 15 || (rn != 15 && !(known & (1u << rn))) || (rm != 15 && !(known & (1u << rm))))
                    return false;
                regs[rd] = (rn == 15 ? pc + 8 : regs[rn]) + (rm == 15 ? pc + 8 : regs[rm]);
                known |= 1u << rd;
                pc += 4;
            } else if ((ins & 0xff000000) == 0xeb000000) { /* BL immediate */
                int32_t delta = (int32_t)(ins << 8) >> 6;
                if (!text_address(m, pc + 8 + (uint32_t)delta, 4))
                    return false;
                *after = pc + 4;
                break;
            } else
                return false;
        }
    }
    return *after && (((known & 3) == 3 && cstring(m, regs[0], function) && log_message(m, regs[1])) ||
                      ((known & 7) == 7 && regs[0] == 0 && cstring(m, regs[1], function) && log_message(m, regs[2])));
}
static void encode16(uint8_t *d, uint16_t v) {
    d[0] = (uint8_t)v;
    d[1] = (uint8_t)(v >> 8);
}
static void encode32(uint8_t *d, uint32_t v) {
    encode16(d, (uint16_t)v);
    encode16(d + 2, (uint16_t)(v >> 16));
}
static bool bounded_target(uint32_t from, uint32_t target) { return llabs((long long)target - from) <= 4096; }
static bool cfstring(const Image *m, uint32_t va, const char *text) {
    size_t o = fileoff(m, va, 16);
    return o != NO_OFF && u32(m->bytes + o + 4) == 0x7c8 && u32(m->bytes + o + 12) == strlen(text) &&
           cstring(m, u32(m->bytes + o + 8), text);
}
static bool arm_literal(const Image *m, uint32_t pc, uint32_t ins, uint32_t *value) {
    if ((ins & 0xff7f0000) != 0xe51f0000)
        return false;
    uint32_t addr = pc + 8;
    addr = (ins & 0x800000) ? addr + (ins & 4095) : addr - (ins & 4095);
    return word(m, addr, value);
}
static bool stack_store(uint32_t ins, unsigned reg, unsigned *slot) {
    if ((ins & 0xfffff000) != (0xe58d0000 | (reg << 12)))
        return false;
    *slot = ins & 4095;
    return true;
}
static bool nearby_function(const Image *m, uint32_t va) {
    uint32_t start = va > 4096 ? va - 4096 : 0;
    for (uint32_t p = start; p < va; p += 4) {
        uint32_t ins, value;
        if (text_address(m, p, 4) && word(m, p, &ins) && arm_literal(m, p, ins, &value) &&
            cstring(m, value, "determine_activation_state"))
            return true;
    }
    return false;
}
/* Older firmware has no development shortcut. Change only the no-record
 * state initializer and its brick boolean (1.x remains experimental).
 * It leaves record verification and all ownership/cleanup code intact.
 */
static bool legacy_initializer(const Image *m, size_t o, uint32_t va, Match *out) {
    uint32_t ins = u32(m->bytes + o), value;
    if (!arm_literal(m, va, ins, &value) || !cfstring(m, value, "Unactivated") || !nearby_function(m, va))
        return false;
    unsigned state = (ins >> 12) & 15, flag = 0, slot1, slot2, slot3;
    if (state >= 13 || !text_address(m, va, 24))
        return false;
    uint32_t a = u32(m->bytes + o + 4), b = u32(m->bytes + o + 8), c = u32(m->bytes + o + 12),
             e = u32(m->bytes + o + 16), jump = u32(m->bytes + o + 20);
    unsigned change = 0;
    uint32_t dest;
    bool shared = false;
    if ((a & 0xffff0fff) == 0xe3a00001) {
        /* Early 1.x keeps the no-record state and brick boolean in stack slots. */
        flag = (a >> 12) & 15;
        if (flag >= 13 || flag == state || va < m->text_va + 12 || !stack_store(b, flag, &slot1) ||
            !stack_store(c, state, &slot2) || slot1 == slot2 || (e & 0xff000000) != 0xea000000)
            return false;
        uint32_t cmp = u32(m->bytes + o - 12), mov = u32(m->bytes + o - 8), guard = u32(m->bytes + o - 4);
        if ((cmp & 0xfff0ffff) != 0xe3500000 || (mov & 0xffff0ff0) != 0xe1a00000 ||
            (guard & 0xff000000) != 0x1a000000 || va + 4 + (uint32_t)((int32_t)(guard << 8) >> 6) != va + 20)
            return false;
        dest = va + 24 + (uint32_t)((int32_t)(e << 8) >> 6);
        if (!text_address(m, dest, 4) || dest <= va + 24 || !bounded_target(va, dest))
            return false;
        change = 4;
    } else {
        if ((c & 0xfff00fff) != 0xe2800001)
            return false;
        flag = (c >> 12) & 15;
        if (flag >= 13 || ((c >> 16) & 15) != flag || state == flag || va < m->text_va + 8)
            return false;
        if (!stack_store(a, flag, &slot1) || !stack_store(b, flag, &slot2))
            return false;
        if ((e & 0xff000000) == 0xea000000) {
            shared = true;
            /* 2.x iPod: the no-record block jumps to a shared state store. The old
             * matcher patched the later factory-cache fallback, which never ran. */
            if (va < m->text_va + 36)
                return false;
            uint32_t cmp = u32(m->bytes + o - 36), guard = u32(m->bytes + o - 32);
            uint32_t fn, msg;
            if (cmp != (0xe3500000 | (flag << 16)) || (guard & 0xff000000) != 0x1a000000 ||
                !arm_literal(m, va - 28, u32(m->bytes + o - 28), &fn) ||
                !cstring(m, fn, "determine_activation_state") ||
                !arm_literal(m, va - 24, u32(m->bytes + o - 24), &msg) ||
                !cstring(m, msg, "There is no activation record?") ||
                (u32(m->bytes + o - 20) & 0xff000000) != 0xeb000000 ||
                (u32(m->bytes + o - 12) & 0xff000000) != 0xeb000000 || u32(m->bytes + o - 8) != 0xe3500000 ||
                (u32(m->bytes + o - 4) & 0xff000000) != 0x1a000000)
                return false;
            dest = va + 24 + (uint32_t)((int32_t)(e << 8) >> 6);
            uint32_t store;
            if (!text_address(m, dest, 4) || !word(m, dest, &store) || !stack_store(store, state, &slot3))
                return false;
        } else {
            uint32_t cmp = u32(m->bytes + o - 8), guard = u32(m->bytes + o - 4);
            if (cmp != (0xe3500000 | (flag << 16)) || (guard & 0xff000000) != 0x1a000000 ||
                va + 4 + (uint32_t)((int32_t)(guard << 8) >> 6) != va + 24 || !stack_store(e, state, &slot3) ||
                (jump & 0xff000000) != 0xea000000)
                return false;
            dest = va + 28 + (uint32_t)((int32_t)(jump << 8) >> 6);
        }
        change = 12;
        if (slot1 == slot2 || slot1 == slot3 || slot2 == slot3 || !text_address(m, dest, 4) || dest <= va + 24 ||
            !bounded_target(va, dest))
            return false;
    }
    /* Reuse a nearby literal that already points to the firmware's Activated
     * CFString. Do not overwrite a shared constant or synthesize an object. */
    uint32_t literal = 0;
    uint64_t best = UINT64_MAX;
    for (size_t r = 0; fits(r, 4, m->text_size); r += 4) {
        uint32_t pc = m->text_va + (uint32_t)r;
        long long delta = (long long)pc - (va + 8);
        if (llabs(delta) <= 4095 && cfstring(m, u32(m->bytes + m->text_off + r), "Activated") &&
            (uint64_t)llabs(delta) < best) {
            best = (uint64_t)llabs(delta);
            literal = pc;
        }
    }
    if (best == UINT64_MAX)
        return false;
    *out = (Match){.off = o,
                   .va = va,
                   .target = dest,
                   .width = change + 4,
                   .isa = "arm",
                   .legacy = !shared,
                   .shared_no_record = shared};
    memcpy(out->replacement, m->bytes + o, out->width);
    long long delta = (long long)literal - (va + 8);
    encode32(out->replacement, 0xe51f0000 | (state << 12) | (delta >= 0 ? 0x800000 : 0) | (uint32_t)llabs(delta));
    encode32(out->replacement + change, 0xe3a00000 | (flag << 12));
    return true;
}
/* Reuse a literal within reach of `va` that already points to the firmware's Activated CFString:
 * the PC-relative LDR that loads it, with `va`'s condition and destination register. */
static bool activated_literal(const Image *m, uint32_t va, uint32_t ins, uint32_t *out) {
    uint32_t literal = 0;
    uint64_t best = UINT64_MAX;
    for (size_t r = 0; fits(r, 4, m->text_size); r += 4) {
        uint32_t pc = m->text_va + (uint32_t)r;
        long long delta = (long long)pc - (va + 8);
        if (llabs(delta) <= 4095 && cfstring(m, u32(m->bytes + m->text_off + r), "Activated") &&
            (uint64_t)llabs(delta) < best) {
            best = (uint64_t)llabs(delta);
            literal = pc;
        }
    }
    if (best == UINT64_MAX)
        return false;
    long long delta = (long long)literal - (va + 8);
    *out = (ins & 0xff7ff000) | (delta >= 0 ? 0x800000 : 0) | (uint32_t)llabs(delta);
    return true;
}
/* 1.0 (1A543a): the no-record case is conditional code in the state function itself:
 *     cmp record, #0 ; moveq brick, #1 ; ldreq state, =Unactivated ; ... ; beq store
 * where the store logs "The activation state has not changed." when nothing moved. Only that
 * initializer changes (brick 0, state Activated); a present record is verified as before. */
static bool conditional_initializer(const Image *m, size_t o, uint32_t va, Match *out) {
    uint32_t ins = u32(m->bytes + o), value;
    if ((ins & 0xff7f0000) != 0x051f0000 || va < m->text_va + 8 || !text_address(m, va, 16) ||
        !arm_literal(m, va, ins | 0xe0000000, &value) || !cfstring(m, value, "Unactivated"))
        return false;
    uint32_t cmp = u32(m->bytes + o - 8), set = u32(m->bytes + o - 4);
    unsigned state = (ins >> 12) & 15, flag = (set >> 12) & 15;
    if ((cmp & 0xfff0ffff) != 0xe3500000 || (set & 0xffff0fff) != 0x03a00001 || state >= 13 || flag >= 13 ||
        state == flag || ((cmp >> 16) & 15) == state)
        return false;
    uint32_t dest = 0;
    for (unsigned k = 1; k <= 3 && !dest; k++) {
        uint32_t b = u32(m->bytes + o + 4 * k);
        if ((b & 0xff000000) == 0x0a000000)
            dest = va + 4 * k + 8 + (uint32_t)((int32_t)(b << 8) >> 6);
        else if ((b & 0x0e000000) == 0x0a000000 || ((b >> 12) & 15) == state || ((b >> 12) & 15) == flag)
            return false; /* other control flow, or the two values overwritten before the store */
    }
    if (!dest || dest <= va || !bounded_target(va, dest) || !text_address(m, dest, 4))
        return false;
    bool logged = false;
    for (uint32_t p = dest; p < dest + 1024 && text_address(m, p, 4) && !logged; p += 4) {
        uint32_t w;
        logged = word(m, p, &w) && arm_literal(m, p, w, &value) &&
                 cstring(m, value, "The activation state has not changed.");
    }
    uint32_t load;
    if (!logged || !activated_literal(m, va, ins, &load))
        return false;
    *out = (Match){
        .off = o - 4, .va = va - 4, .target = dest, .width = 8, .isa = "arm", .legacy = true, .conditional = true};
    encode32(out->replacement, 0x03a00000 | (flag << 12));
    encode32(out->replacement + 4, load);
    return true;
}
static Match locate(const Image *m) {
    Match result = {0};
    unsigned count = 0;
    for (size_t rel = 0; fits(rel, 8, m->text_size); rel += 2) {
        size_t o = m->text_off + rel;
        uint32_t va = m->text_va + (uint32_t)rel;
        uint16_t cmp = u16(m->bytes + o), b = u16(m->bytes + o + 2);
        uint32_t after = 0, target = 0;
        unsigned width = 0;
        if ((cmp & 0xf800) == 0x2800 && (cmp & 255) <= 1) {
            if ((b & 0xff00) == 0xd000) {
                width = 2;
                target = va + 6 + (uint32_t)((int32_t)(int8_t)(b & 255) * 2);
            } else if ((b & 0xfbc0) == 0xf000 && (u16(m->bytes + o + 4) & 0xd000) == 0x8000) {
                uint16_t hi = u16(m->bytes + o + 4);
                uint32_t imm = (((b >> 10) & 1) << 20) | (((hi >> 11) & 1) << 19) | (((hi >> 13) & 1) << 18) |
                               ((b & 63) << 12) | ((hi & 2047) << 1);
                width = 4;
                target = va + 6 + (uint32_t)((int32_t)(imm << 11) >> 11);
            }
            if (width && bounded_target(va, target) && text_address(m, target, 2)) {
                bool fall = log_block(m, va + 2 + width, true, &after) && (target < va || target >= after);
                uint32_t target_after = 0;
                bool take = (target < va || target > va + 2 + width) && log_block(m, target, true, &target_after);
                if (fall && take)
                    fail("ambiguous branch destinations; file unchanged");
                if (fall || take) {
                    result = (Match){.off = o + 2, .va = va + 2, .target = target, .width = width, .isa = "thumb"};
                    count++;
                    if (fall) {
                        encode16(result.replacement, 0xbf00);
                        if (width == 4)
                            encode16(result.replacement + 2, 0xbf00);
                    } else {
                        int32_t delta = (int32_t)(target - (va + 6));
                        if (width == 2)
                            encode16(result.replacement, (uint16_t)(0xe000 | (((uint32_t)delta >> 1) & 2047)));
                        else {
                            uint32_t imm = (uint32_t)delta, sign = (imm >> 24) & 1;
                            encode16(result.replacement, (uint16_t)(0xf000 | (sign << 10) | ((imm >> 12) & 1023)));
                            encode16(result.replacement + 2,
                                     (uint16_t)(0x9000 | ((!(((imm >> 23) & 1) ^ sign)) << 13) |
                                                ((!(((imm >> 22) & 1) ^ sign)) << 11) | ((imm >> 1) & 2047)));
                        }
                    }
                }
            }
        }
        if (!m->message_va && !(va & 3)) {
            Match legacy = {0};
            if (legacy_initializer(m, o, va, &legacy) || conditional_initializer(m, o, va, &legacy)) {
                result = legacy;
                count++;
            }
        }
        if (!(va & 3) && fits(rel, 12, m->text_size)) {
            uint32_t c = u32(m->bytes + o), br = u32(m->bytes + o + 4);
            after = 0;
            if ((c & 0xfff0fffe) == 0xe3500000 && (br & 0xff000000) == 0x0a000000) {
                int32_t delta = (int32_t)(br << 8) >> 6;
                target = va + 12 + (uint32_t)delta;
                if (bounded_target(va, target) && text_address(m, target, 4)) {
                    bool fall = log_block(m, va + 8, false, &after) && (target < va || target >= after);
                    uint32_t target_after = 0;
                    bool take = (target < va || target > va + 8) && log_block(m, target, false, &target_after);
                    if (fall && take)
                        fail("ambiguous branch destinations; file unchanged");
                    if (fall || take) {
                        result = (Match){.off = o + 4, .va = va + 4, .target = target, .width = 4, .isa = "arm"};
                        count++;
                        encode32(result.replacement, fall ? 0xe1a00000 : br | 0xe0000000);
                    }
                }
            }
        }
    }
    if (count != 1)
        fail(count ? "ambiguous activation paths; file unchanged"
                   : "no supported activation path (or already patched); file unchanged");
    return result;
}
#ifdef LT_ACTIVATION_LIBRARY
int lt_activate_report(uint8_t *bytes, size_t size, LTActivationReport *report, const char **error) {
    if (report)
        memset(report, 0, sizeof(*report));
    if (setjmp(failure)) {
        *error = failure_message;
        return 0;
    }
    if (size < 28 || size > LIMIT)
        fail("invalid input size");
    Image m = {.bytes = bytes, .size = size};
    parse(&m);
    Match match = locate(&m);
    if (report) {
        report->strategy = match.conditional        ? "conditional-no-record-initializer"
                           : match.legacy           ? "legacy-no-record-initializer"
                           : match.shared_no_record ? "ipod-no-record-initializer"
                                                    : "development-activation-shortcut";
        report->isa = match.isa;
        report->offset = match.off;
        report->width = match.width;
        memcpy(report->original, bytes + match.off, match.width);
        memcpy(report->replacement, match.replacement, match.width);
    }
    memcpy(bytes + match.off, match.replacement, match.width);
    return match.legacy ? 2 : 1;
}
int lt_activate(uint8_t *bytes, size_t size, const char **error) {
    return lt_activate_report(bytes, size, NULL, error);
}
#else
static void atomic_write(const char *path, const Image *m, const struct stat *original) {
    size_t len = strlen(path) + 32;
    char *temp = malloc(len);
    if (!temp)
        fail("out of memory");
    snprintf(temp, len, "%s.activation.XXXXXX", path);
    int fd = mkstemp(temp);
    if (fd < 0)
        fail("cannot create adjacent temporary file");
    bool ok = true;
    size_t off = 0;
    while (off < m->size) {
        ssize_t n = write(fd, m->bytes + off, m->size - off);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0) {
            ok = false;
            break;
        }
        off += (size_t)n;
    }
    if (fchmod(fd, original->st_mode & 0777) < 0 || fsync(fd) < 0)
        ok = false;
    if (close(fd) < 0)
        ok = false;
    struct stat now;
    if (lstat(path, &now) < 0 || now.st_dev != original->st_dev || now.st_ino != original->st_ino ||
        now.st_size != original->st_size || now.st_mtime != original->st_mtime)
        ok = false;
    if (!ok || rename(temp, path) < 0) {
        unlink(temp);
        free(temp);
        fail("atomic replacement failed or input changed");
    }
    free(temp);
}
int main(int argc, char **argv) {
    bool probe = false, experimental = false;
    const char *path = NULL;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--probe"))
            probe = true;
        else if (!strcmp(argv[i], "--experimental-legacy"))
            experimental = true;
        else if (argv[i][0] != '-' && !path)
            path = argv[i];
        else {
            fprintf(stderr, "Usage: %s [--probe] [--experimental-legacy] LOCKDOWND\n", argv[0]);
            return 2;
        }
    }
    if (!path) {
        fprintf(stderr,
                "Usage: %s [--probe] [--experimental-legacy] LOCKDOWND\nSigning is the caller's "
                "responsibility.\n",
                argv[0]);
        return 2;
    }
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0)
        fail("cannot open regular input (symlinks are refused)");
    struct stat st;
    if (fstat(fd, &st) < 0 || !S_ISREG(st.st_mode) || st.st_size < 28 || st.st_size > LIMIT)
        fail("invalid input file size or type");
    Image m = {0};
    m.size = (size_t)st.st_size;
    m.bytes = malloc(m.size);
    if (!m.bytes)
        fail("out of memory");
    size_t off = 0;
    while (off < m.size) {
        ssize_t n = read(fd, m.bytes + off, m.size - off);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            fail("cannot read input");
        off += (size_t)n;
    }
    close(fd);
    parse(&m);
    Match match = locate(&m);
    (void)experimental; /* Accepted for existing diagnostic scripts; activation is automatic. */
    char old[33] = {0};
    for (unsigned i = 0; i < match.width; i++)
        snprintf(old + 2 * i, 3, "%02x", m.bytes[match.off + i]);
    char replacement[33] = {0};
    for (unsigned i = 0; i < match.width; i++)
        snprintf(replacement + 2 * i, 3, "%02x", match.replacement[i]);
    if (!probe) {
        memcpy(m.bytes + match.off, match.replacement, match.width);
        atomic_write(path, &m, &st);
    }
    printf("{\"mode\":\"%s\",\"strategy\":\"%s\",\"isa\":\"%s\",\"file_offset\":%zu,\"virtual_"
           "address\":%" PRIu32 ",\"size\":%u,\"old\":\"%s\",\"new\":\"%s\"}\n",
           probe ? "probe" : "apply",
           match.conditional        ? "conditional-no-record-initializer"
           : match.legacy           ? "legacy-no-record-initializer"
           : match.shared_no_record ? "ipod-no-record-initializer"
                                    : "development-activation-shortcut",
           match.isa, match.off, match.va, match.width, old, replacement);
    free(m.bytes);
    return 0;
}

#endif
