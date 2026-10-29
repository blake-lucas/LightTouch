/*
 * Copyright 2013-2016, iH8sn0w. <iH8sn0w@iH8sn0w.com>
 *
 * This file is part of iBoot32Patcher.
 *
 * iBoot32Patcher is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * iBoot32Patcher is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with iBoot32Patcher.  If not, see <http://www.gnu.org/licenses/>.
 */

#ifndef ARM_DEFS_H
#define ARM_DEFS_H

#define ARM32_THUMB_MOV 0
#define ARM32_THUMB_CMP 1
#define ARM32_THUMB_ADD 2
#define ARM32_THUMB_SUB 3

#define ARM32_THUMB_IT_NE __builtin_bswap16(0x18BF)
#define ARM32_THUMB_IT_EQ __builtin_bswap16(0x08BF)
#define ARM32_THUMB_ITE_NE __builtin_bswap16(0x14BF)

/* The 32-bit Thumb-2 instructions below are one little-endian word, so every bit-field is uint32_t: GCC starts a
 * new storage unit when a field does not fit the remaining bits of its declared type (even in a packed struct), so
 * uint8_t/uint16_t fields spanning a word split the layout and the decoders never match (arm32_thumb_MOVW came out
 * 5 bytes and is_MOVW_insn() never fired). With uint32_t the layout is the intended LSB-first one under both clang
 * and GCC: sizeof == 4, field n at bit offset sum of the widths before it. */

struct arm32_thumb_LDR_T3 {
	uint32_t rn : 4;
	uint32_t pad : 12;
	uint32_t imm12 : 12;
	uint32_t rt : 4;
} __attribute__ ((packed));

struct arm32_thumb_MOVW {
	uint32_t imm4 : 4;
	uint32_t pad0 : 6;
	uint32_t i : 1;
	uint32_t pad1 : 5;
	uint32_t imm8 : 8;
	uint32_t rd : 4;
	uint32_t imm3 : 3;
	uint32_t bit31 : 1;
} __attribute__((packed));

struct arm32_thumb_BL {
	uint32_t offset : 11;
	uint32_t h : 1;
	uint32_t padd : 4;
	uint32_t offset2 : 11;
	uint32_t h2 : 1;
	uint32_t padd2 : 4;
} __attribute__((packed));

struct arm32_thumb_MOVT_W {
	uint32_t imm4 : 4;
	uint32_t pad0 : 6;
	uint32_t i : 1;
	uint32_t pad1 : 5;
	uint32_t imm8 : 8;
	uint32_t rd : 4;
	uint32_t imm3 : 3;
	uint32_t bit31 : 1;
} __attribute__((packed));

struct arm32_thumb_BW_T4 {
	uint32_t imm10 : 10;
	uint32_t s : 1;
	uint32_t pad0 : 5;
	uint32_t imm11 : 11;
	uint32_t j2 : 1;
	uint32_t bit12 : 1;
	uint32_t j1 : 1;
	uint32_t pad1 : 2;
} __attribute__((packed));

struct arm32_thumb_IT_T1 {
	uint8_t mask : 4;
	uint8_t cond : 4;
	uint8_t pad : 8;
}__attribute__((packed));

struct arm32_thumb_LDR {
	uint8_t imm8;
	uint8_t rd : 3;
	uint8_t padd : 5;
} __attribute__((packed));

struct arm32_thumb_hi_reg_op {
	uint8_t rd : 3;
	uint8_t rs : 3;
	uint8_t h2 : 1;
	uint8_t h1 : 1;
	uint8_t op : 2;
	uint8_t pad : 6;
} __attribute__((packed));

struct arm32_thumb {
	uint8_t offset : 8;
	uint8_t rd : 3;
	uint8_t op : 2;
	uint8_t one : 1;
	uint8_t z: 1;
	uint8_t zero : 1;
} __attribute__((packed));

struct arm32_thumb_MOVT {
    uint32_t imm4 : 4;   // Top 4 bits of the immediate value
    uint32_t pad0 : 6;   // Constant value bits (part of the encoding, usually fixed)
    uint32_t i : 1;      // The highest bit of the immediate value
    uint32_t pad1 : 5;   // Constant value bits (part of the encoding, usually fixed)
    uint32_t imm8 : 8;   // Lower 8 bits of the immediate value
    uint32_t rd : 4;     // Destination register
    uint32_t imm3 : 3;   // Middle 3 bits of the immediate value
    uint32_t bit31 : 1;  // Should be 0
} __attribute__((packed));

#endif
