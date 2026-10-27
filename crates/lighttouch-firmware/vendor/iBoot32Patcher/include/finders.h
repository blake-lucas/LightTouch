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

#ifndef FINDERS_H
#define FINDERS_H

#include <include/iBoot32Patcher.h>

#define RSA_STR "Apple Secure Boot Certification Authority"
#define MOVW_R0_NEG_1 0x4ff0ff30
#define MOV_NEGS_R0_1 0x01204042
#define MOVS_R1_0x14 0x1421

// platform/s5l8950x/init.c
#define PLATFORM_INIT_STR "platform/s5l"
#define PLATFORM_8720 bswap32(0x38373230)
#define PLATFORM_8900 bswap32(0x38393030)
#define PLATFORM_8920 bswap32(0x38393230)
#define PLATFORM_8922 bswap32(0x38393232)
#define PLATFORM_8930 bswap32(0x38393330)
#define PLATFORM_8940 bswap32(0x38393430)
#define PLATFORM_8942 bswap32(0x38393432)
#define PLATFORM_8945 bswap32(0x38393435)
#define PLATFORM_8947 bswap32(0x38393437)
#define PLATFORM_8950 bswap32(0x38393530)
#define PLATFORM_8955 bswap32(0x38393535)

void* find_bl_verify_shsh(struct iboot_img* iboot_in);
void* find_rsa_check_3_4(struct iboot_img* iboot_in);
void* find_ldr_ecid(struct iboot_img* iboot_in);
void* find_ldr_bord(struct iboot_img* iboot_in);
void* find_ldr_prod(struct iboot_img* iboot_in);
void* find_ldr_sepo(struct iboot_img* iboot_in);
void* find_boot_partition_ldr(struct iboot_img* iboot_in);
void* find_boot_ramdisk_ldr(struct iboot_img* iboot_in);
void* find_bl_verify_shsh_5_6_7(struct iboot_img* iboot_in);
void* find_bl_verify_shsh_generic(struct iboot_img* iboot_in);
void* find_bl_verify_shsh_insn(struct iboot_img* iboot_in, void* pc);
void* find_bl_verify_shsh_insn_next(struct iboot_img* iboot_in, void* pc);
void* find_dtre_get_value_bl_insn(struct iboot_img* iboot_in, const char* var);
void* find_verify_shsh_top(void* ptr);
uint32_t find_GETENV_Addr(struct iboot_img* iboot_in);
void* find_Boot_Args_String_Location(struct iboot_img* iboot_in);
void* find_Boot_Args_MOV(void* Search_Begin);
void* find_ldr_xref(struct iboot_img *iboot_in);
void* find_null_str(void* _mov_insn, int reg);
void* find_kloader_addr(struct iboot_img* iboot_in);
void* find_usb_wait_for_image(struct iboot_img* iboot_in);
void* find_fsboot_boot_command(struct iboot_img* iboot_in);
void* find_auto_boot(struct iboot_img* iboot_in);
void* find_platform(struct iboot_img* iboot_in);
#endif
