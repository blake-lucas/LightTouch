/* Light Touch additions to iBoot32Patcher; GPL-3.0-or-later, as the surrounding tool.
 * Recognize the actual result ABI before emitting a store through R3. */
static uint16_t rsa_half(const unsigned char *p) {
    return (uint16_t)p[0] | (uint16_t)p[1] << 8;
}
static int rsa_stack_result(struct iboot_img *img, void *call) {
    size_t o = (unsigned char *)call - (unsigned char *)img->buf;
    if (o < 6 || o + 4 > img->len) return 0;
    unsigned char *p = (unsigned char *)call;
    /* ADD R3,SP,#slot; MOV R0,Rn; MOV R1,Rm; BL. No intervening
     * branch, call, conditional execution or write to R3/SP. */
    return (rsa_half(p - 6) & 0xff00) == 0xab00 &&
           (rsa_half(p - 4) & 0xff87) == 0x4600 &&
           (rsa_half(p - 2) & 0xff87) == 0x4601;
}

/* Older IMG3 code keeps authentication and data extraction in the same
 * function. The CERT-based upstream finder selects only its two-argument
 * signature call. Bypassing that call alone still runs personalization checks.
 * Recognize the result initialization followed by the DATA extraction block;
 * retain the initialization and jump over authentication, never over decryption.
 * Return the DATA block only when both call ABI and destination are unambiguous. */
static void *rsa_legacy_data_block(struct iboot_img *img, void *call) {
    unsigned char *base = img->buf, *p = call;
    size_t o = p - base;
    if (o + 20 > img->len) return NULL;
    uintptr_t t = (uintptr_t)resolve_bl32(call) & ~(uintptr_t)1;
    if (t < (uintptr_t)base || t - (uintptr_t)base > img->len ||
        img->len - (t - (uintptr_t)base) < 24) return NULL;
    unsigned char *target = (unsigned char *)t;
    /* PUSH frame; LDR R1,[R0,#4]; TST R1,#2: image flags, no R3 output. */
    const unsigned char frame[] = {0xf0,0xb5,0x03,0xaf,0x2d,0xe9,0x00,0x0d};
    const unsigned char flags[] = {0x41,0x68,0x11,0xf0,0x02,0x0f};
    if (memcmp(target, frame, sizeof(frame)) || memcmp(target + 18, flags, sizeof(flags))) return NULL;
    /* MOVS R5,#0; STR R5,[SP,#result]; CBZ R0,success;
     * MOV.W R5,#-1; CMP R0,#1. The store must precede our new branch. */
    if (rsa_half(p + 4) != 0x2500 || (rsa_half(p + 6) & 0xff00) != 0x9500 ||
        (rsa_half(p + 8) & 0xfd07) != 0xb100 || rsa_half(p + 10) != 0xf04f ||
        rsa_half(p + 12) != 0x35ff || rsa_half(p + 14) != 0x2801) return NULL;
    unsigned result = rsa_half(p + 6) & 255;
    void *match = NULL;
    for (size_t d = o + 20; d < o + 0x600 && d + 32 <= img->len; d += 2) {
        unsigned char *q = base + d;
        /* MOVS R5,#0; MOVW R1,#'TA'; STR R5,[SP,#len]; MOVT R1,#'DA';
         * STR R5,[SP]; MOV.W R5,#-1; LDR R0,[SP,#img];
         * ADD R2,SP,#data; ADD R3,SP,#len; BL image_get_tag; CMP R0,#0. */
        if (rsa_half(q) != 0x2500 || rsa_half(q + 2) != 0xf245 || rsa_half(q + 4) != 0x4141 ||
            (rsa_half(q + 6) & 0xff00) != 0x9500 || rsa_half(q + 8) != 0xf2c4 ||
            rsa_half(q + 10) != 0x4141 || rsa_half(q + 12) != 0x9500 ||
            rsa_half(q + 14) != 0xf04f || rsa_half(q + 16) != 0x35ff ||
            (rsa_half(q + 18) & 0xff00) != 0x9800 ||
            (rsa_half(q + 20) & 0xff00) != 0xaa00 ||
            (rsa_half(q + 22) & 0xff00) != 0xab00 ||
            (rsa_half(q + 6) & 255) != (rsa_half(q + 22) & 255) ||
            (rsa_half(q + 20) & 255) == (rsa_half(q + 22) & 255) ||
            (rsa_half(q + 22) & 255) == result || (rsa_half(q + 20) & 255) == result ||
            (rsa_half(q + 24) & 0xf800) != 0xf000 || (rsa_half(q + 26) & 0xd000) != 0xd000 ||
            rsa_half(q + 28) != 0x2800) continue;
        uintptr_t tag = (uintptr_t)resolve_bl32(q + 24) & ~(uintptr_t)1;
        if (tag < (uintptr_t)base || tag >= (uintptr_t)base + img->len) continue;
        if (match) return NULL;
        match = q;
    }
    return match;
}
