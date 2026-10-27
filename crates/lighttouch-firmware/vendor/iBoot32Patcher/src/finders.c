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

#include <stdio.h>
#include <stdlib.h>
#include <include/finders.h>
#include <include/functions.h>
#include <include/patchers.h>
#include <include/iBoot32Patcher.h>

void* find_bl_verify_shsh(struct iboot_img* iboot_in) {
    int os_vers = get_os_version(iboot_in);
    
    /* Use the os-specific method for finding BL verify_shsh... */
    if(os_vers >= 5 && os_vers <= 7) {
        return find_bl_verify_shsh_5_6_7(iboot_in);
    }
    
    return find_bl_verify_shsh_generic(iboot_in);
}

void* find_ldr_ecid(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the LDR Rx, ='ECID' instruction... */
    void* ldr_insn = find_next_LDR_insn_with_value(iboot_in, 'ECID');
    if(!ldr_insn) {
        printf("%s: Unable to find LDR ECID!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found LDR instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_insn));
    
    /* Resolve the BL verify_shsh routine from found instruction... */
    char *ldr_ecid = bl_search_down(ldr_insn,0x100);
    if(!ldr_ecid) {
        printf("%s: Unable to find a BL ECID! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found BL ECID at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_ecid));
    
    printf("%s: Leaving...\n", __FUNCTION__);
    
    return ldr_ecid;
}

void* find_ldr_bord(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the LDR Rx, ='BORD' instruction... */
    void* ldr_insn = find_next_LDR_insn_with_value(iboot_in, 'BORD');
    if(!ldr_insn) {
        printf("%s: Unable to find LDR insn!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found LDR BORD instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_insn));
    
    /* Resolve the BL verify_shsh routine from found instruction... */
    char *ldr_bord = bl_search_down(ldr_insn,0x100);
    if(!ldr_bord) {
        printf("%s: Unable to find a BL BORD! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found BL BORD at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_bord));
    
    printf("%s: Leaving...\n", __FUNCTION__);
    
    return ldr_bord;
}

void* find_ldr_prod(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the LDR Rx, ='PROD' instruction... */
    void* ldr_insn = find_next_LDR_insn_with_value(iboot_in, 'PROD');
    if(!ldr_insn) {
        printf("%s: Unable to find LDR insn!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found LDR PROD instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_insn));
    
    /* Resolve the BL verify_shsh routine from found instruction... */
    char *ldr_prod = bl_search_down(ldr_insn,0x100);
    if(!ldr_prod) {
        printf("%s: Unable to find a BL PROD! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found BL PROD at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_prod));
    
    printf("%s: Leaving...\n", __FUNCTION__);
    
    return ldr_prod;
}

void* find_ldr_sepo(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the LDR Rx, ='SEPO' instruction... */
    void* ldr_insn = find_next_LDR_insn_with_value(iboot_in, 'SEPO');
    if(!ldr_insn) {
        printf("%s: Unable to find LDR insn!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found LDR SEPO instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_insn));
    
    /* Resolve the BL verify_shsh routine from found instruction... */
    char *ldr_sepo = bl_search_down(ldr_insn,0x100);
    if(!ldr_sepo) {
        printf("%s: Unable to find a BL SEPO! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found BL SEPO at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_sepo));
    
    printf("%s: Leaving...\n", __FUNCTION__);
    
    return ldr_sepo;
}

void* find_rsa_check_3_4(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    void* ldr_cert = find_next_LDR_insn_with_value(iboot_in, 'CERT');
    if (!ldr_cert) {
        printf("%s: Failed to find LDR Rx, CERT!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found LDR Rx, CERT at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_cert));

    /* Look for MOVS R1, #0x14 (hacky) as it seems to be used throughout multiple devices */

    void* movs_insn = pattern_search(ldr_cert + 0x30, 0x100, bswap16(MOVS_R1_0x14), bswap16(MOVS_R1_0x14), 2);
    if (!movs_insn) {
        printf("%s: Failed to find MOVS R1, #0x14!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found MOVS R1, #0x14 at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, movs_insn));

    void* bl_insn_1 = bl_search_down(movs_insn, 0x20);
    if (!bl_insn_1) {
        printf("%s: Failed to find BL 1 instruction!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found BL 1 instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_insn_1));

    void* bl_insn_1_xref = resolve_bl32(bl_insn_1);
    if (!bl_insn_1_xref) {
        printf("%s: Failed to get BL 1 instruction xref!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found BL 1 instruction xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_insn_1_xref));

    void* bl_insn_2 = bl_search_down(bl_insn_1_xref, 0x20);
    if (!bl_insn_2) {
        printf("%s: Failed to find BL 2 instruction!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found BL 2 instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_insn_2));

    void* bl_insn_2_xref = resolve_bl32(bl_insn_2);
    if (!bl_insn_2_xref) {
        printf("%s: Failed to get BL 2 instruction xref!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found BL 2 instruction xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_insn_2_xref));

    void* cmp_insn = find_next_CMP_insn_with_value(bl_insn_2_xref + 0x300 + 1, 0x200, 0x14);
    if (!cmp_insn) {
        printf("%s: Failed to find CMP Rx, 0x14!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found CMP Rx, 0x14 at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, cmp_insn));

    void* bl_insn_3 = bl_search_down(cmp_insn + 0x25, 0x20);
    if (!bl_insn_3) {
        printf("%s: Failed to find BL 3 instruction!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found BL 3 instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_insn_3));

    void* bl_insn_3_xref = resolve_bl32(bl_insn_3);
    if (!bl_insn_3_xref) {
        printf("%s: Failed to find BL 3 instruction xref!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found BL 3 instruction xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_insn_3_xref));

    void* rsa_mov_neg1 = pattern_search(bl_insn_3_xref, 0x200, bswap32(MOVW_R0_NEG_1), bswap32(MOVW_R0_NEG_1), 1);
    if(!rsa_mov_neg1) {
        printf("%s: Failed to find MOV.W R0, #0xFFFFFFFF!\n", __FUNCTION__);

        void* rsa_movs_1 = pattern_search(bl_insn_3_xref + 0x160, 0x10, bswap32(MOV_NEGS_R0_1), bswap32(MOV_NEGS_R0_1), 1);
        if(!rsa_movs_1) {
            printf("%s: Failed to find MOVS R0, #1; NEGS R0, R0! Adjusting offset!\n", __FUNCTION__);

            void* rsa_movs_2 = pattern_search(bl_insn_3_xref + 0x150, 0x10, bswap32(MOV_NEGS_R0_1), bswap32(MOV_NEGS_R0_1), 1);
            if (!rsa_movs_2) {
                printf("%s: Failed to find MOVS R0, #1; NEGS R0, R0!\n", __FUNCTION__);
                return 0;
            }
            rsa_movs_1 = rsa_movs_2;
        }

        printf("%s: Found MOVS R0, #1; NEGS R0, R0 at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, rsa_movs_1));
        rsa_mov_neg1 = rsa_movs_1;
    } else {
        printf("%s: Found MOV.W R0, #0xFFFFFFFF! at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, rsa_mov_neg1));
    }

    printf("%s: Leaving...\n", __FUNCTION__);

    return rsa_mov_neg1;
}

void* find_bl_verify_shsh_5_6_7(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the MOVW Rx, #'RT' instruction... */
    void* movw = find_next_MOVW_insn_with_value(iboot_in->buf, iboot_in->len, 'RT');
    if(!movw) {
        printf("%s: Unable to find MOVW instruction!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found MOVW instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, movw));
    
    /* Resolve the BL verify_shsh routine from found instruction... */
    void* bl_verify_shsh = find_bl_verify_shsh_insn(iboot_in, movw);
    if(!bl_verify_shsh) {
        printf("%s: Unable to find a BL verify_shsh! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    void* bl_verify_shsh_next = find_bl_verify_shsh_insn_next(iboot_in, movw);
    if(!bl_verify_shsh_next) {
        printf("%s: Found BL verify_shsh at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_verify_shsh));
        
        printf("%s: Leaving...\n", __FUNCTION__);
        
        return bl_verify_shsh;
    }
    
    printf("%s: Found BL verify_shsh_next at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_verify_shsh_next));
    
    printf("%s: Leaving...\n", __FUNCTION__);
    
    return bl_verify_shsh_next;
}

void* find_bl_verify_shsh_generic(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the LDR Rx, ='CERT' instruction... */
    void* ldr_insn = find_next_LDR_insn_with_value(iboot_in, 'CERT');
    if(!ldr_insn) {
        printf("%s: Unable to find LDR insn!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found LDR instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_insn));
    
    /* Resolve the BL verify_shsh routine from found instruction... */
    void* bl_verify_shsh = find_bl_verify_shsh_insn(iboot_in, ldr_insn);
    if(!bl_verify_shsh) {
        printf("%s: Unable to find a BL verify_shsh! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found BL verify_shsh at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_verify_shsh));
    
    printf("%s: Leaving...\n", __FUNCTION__);
    
    return bl_verify_shsh;
}

void* find_bl_verify_shsh_insn(struct iboot_img* iboot_in, void* pc) {
    /* Find the top of the function... */
    void* function_top = find_verify_shsh_top(pc);
    if(!function_top) {
        printf("%s: Unable to find top of verify_shsh!\n", __FUNCTION__);
        return 0;
    }
    
    /* Find the BL insn resolving to this function... (BL verify_shsh seems to only happen once) */
    void* bl_verify_shsh = find_next_bl_insn_to(iboot_in, (uint32_t) ((uintptr_t)GET_IBOOT_FILE_OFFSET(iboot_in, function_top)));
    if(!bl_verify_shsh) {
        return 0;
    }
    
    return bl_verify_shsh;
}

void* find_bl_verify_shsh_insn_next(struct iboot_img* iboot_in, void* pc) {
    /* Find the top of the function... */
    void* function_top = find_verify_shsh_top(pc);
    if(!function_top) {
        printf("%s: Unable to find top of verify_shsh!\n", __FUNCTION__);
        return 0;
    }
    
    /* Find the BL insn resolving to this function... (BL verify_shsh seems to only happen once) */
    void* bl_verify_shsh = find_next_next_bl_insn_to(iboot_in, (uint32_t) ((uintptr_t)GET_IBOOT_FILE_OFFSET(iboot_in, function_top)));
    if(!bl_verify_shsh) {
        return 0;
    }
    
    return bl_verify_shsh;
}

void* find_dtre_get_value_bl_insn(struct iboot_img* iboot_in, const char* var) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the variable string... */
    void* var_str_loc = memstr(iboot_in->buf, iboot_in->len, var);
    if(!var_str_loc) {
        printf("%s: Unable to find %s string!\n", __FUNCTION__, var);
        return 0;
    }
    printf("%s: %s string is at %p\n", __FUNCTION__, var, (void*) GET_IBOOT_FILE_OFFSET(iboot_in, var_str_loc));
    
    /* Find the variable string xref... */
    void* var_xref = iboot_memmem(iboot_in, var_str_loc);
    if(!var_xref) {
        printf("%s: Unable to find %s string xref!\n", __FUNCTION__, var);
        return 0;
    }
    printf("%s: \"%s\" xref is at %p\n", __FUNCTION__, var, (void*) GET_IBOOT_FILE_OFFSET(iboot_in, var_xref));
    
    /* Locate corresponding LDR insn for xref... */
    void* var_ldr = ldr_to(var_xref);
    if(!var_ldr) {
        printf("%s: Unable to find %s string LDR from xref!\n", __FUNCTION__, var);
        return 0;
    }
    
    struct arm32_thumb_LDR* ldr_rd_debug_enabled = (struct arm32_thumb_LDR*) var_ldr;
    printf("%s: Found LDR R%d, =\"%s\" at %p\n", __FUNCTION__, ldr_rd_debug_enabled->rd, var, GET_IBOOT_FILE_OFFSET(iboot_in, var_ldr));
    
    /* Find the BL insn that follows the LDR insn... (Usually BL does_dtre_have_var) */
    void* is_in_dtre_bl = bl_search_down(var_ldr, 0x100);
    if(!is_in_dtre_bl) {
        printf("%s: Unable to find a BL following LDR insn.\n", __FUNCTION__);
        return 0;
    }
    is_in_dtre_bl++;
    
    /* Find the next BL insn after the previous BL insn... (Usually BL get_value_for_dtre_var) */
    void* get_value_for_dtre_bl = bl_search_down(is_in_dtre_bl, 0x100);
    if(!get_value_for_dtre_bl) {
        printf("%s: Unable to find a BL following the previous BL.\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found BL instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, get_value_for_dtre_bl));
    
    printf("%s: Leaving...\n", __FUNCTION__);
    
    return get_value_for_dtre_bl;
}

void* find_verify_shsh_top(void* ptr) {
    void* top = push_r4_to_r7_lr_search_up(ptr, 0x500);
    if(!top) {
        return 0;
    }
    top++; // Thumb
    return top;
}

uint32_t find_GETENV_Addr(struct iboot_img* iboot_in) {
    printf("%s: Finding network-type ldr\n", __FUNCTION__);
    void* network_type_ldr =  find_next_LDR_insn_with_str(iboot_in, "network-type");
    if(!network_type_ldr) {
        printf("%s: Failed to find network-type ldr\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found network-type ldr at %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, network_type_ldr));
    printf("%s: Finding getenv bl\n", __FUNCTION__);
    void* network_type_bl = bl_search_down(network_type_ldr, 0x10);
    if(!network_type_bl) {
        printf("%s: Failed to find getenv bl\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found getenv bl at %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, network_type_bl));
    printf("%s: Finding getenv address\n", __FUNCTION__);
    uint32_t GetENV_Addr = Resolve_BL_Long((uint32_t)GET_IBOOT_ADDR(iboot_in, network_type_bl), network_type_bl);
    printf("%s: Found getenv address at: 0x%x\n", __FUNCTION__, GetENV_Addr);
    return GetENV_Addr;
}

void* find_Boot_Args_String_Location(struct iboot_img* iboot_in) {
    printf("%s: Finding boot-args string location\n", __FUNCTION__);
    void* boot_args_str_loc = memstr(iboot_in->buf, iboot_in->len, "boot-args");
    if(!boot_args_str_loc) {
        printf("%s: Failed to find boot-args string location", __FUNCTION__);
        return 0;
    }
    return boot_args_str_loc;
}


void* find_Boot_Args_MOV(void* Search_Begin) {
    char* Potential_Mov = find_next_MOV_insn(Search_Begin, 0x10);
    if(!Potential_Mov) {
        printf("%s: Failed to find Last MOV\n", __FUNCTION__);
        return 0;
    }
    if(is_MOV_insn(Potential_Mov + 0x2)) {
        return (Potential_Mov + 0x2);
    }
    return Potential_Mov;
}
void* find_ldr_xref(struct iboot_img* iboot_in) {
    void* default_boot_args_str_loc = memstr(iboot_in->buf, iboot_in->len, DEFAULT_BOOTARGS_STR);
    if(!default_boot_args_str_loc) {
        printf("%s: Unable to find default boot-args string!\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Default boot-args string is at %p\n", __FUNCTION__, (void*) GET_IBOOT_ADDR(iboot_in, default_boot_args_str_loc));
    
    void* default_boot_args_xref = iboot_memmem(iboot_in, default_boot_args_str_loc);
    if(!default_boot_args_xref) {
        printf("%s: Unable to find default boot-args string xref!\n", __FUNCTION__);
        return 0;
    }
    printf("%s: boot-args xref is at %p\n", __FUNCTION__, (void*) GET_IBOOT_ADDR(iboot_in, default_boot_args_xref));
    return default_boot_args_xref;
}
void* find_null_str(void* _mov_insn, int reg) {
    void* _Candidate = ldr_search_up(_mov_insn, 0x50);
    if(!_Candidate) {
        printf("%s: Failed to find ldr r%d, = null_str\n", __FUNCTION__, reg);
        return NULL;
    }
    struct arm32_thumb_LDR* Candidate = (struct arm32_thumb_LDR*) _Candidate;
    if(Candidate->rd == reg){
    return _Candidate;
    }
    else {
        printf("%s: Failed to find ldr r%d, = null_str\n", __FUNCTION__, reg);
        return NULL;
    }
}

void* find_boot_partition_ldr(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    uint32_t boot_partition_loc = GET_IBOOT_ADDR(iboot_in, memmem(iboot_in -> buf, iboot_in -> len, "boot-partition", strlen("boot-partition")));
    if (!boot_partition_loc) {
        printf("%s: Failed to find boot-partition string\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found boot-partition string: %p\n", __FUNCTION__, boot_partition_loc);
    void * boot_partition_ldr = find_next_LDR_insn_with_value(iboot_in, boot_partition_loc);
    if (!boot_partition_ldr) {
        printf("%s: Failed to find boot-partition LDR\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found boot-partition LDR: %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, boot_partition_ldr));
    return boot_partition_ldr;
}

void* find_boot_ramdisk_ldr(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    uint32_t boot_ramdisk_loc = GET_IBOOT_ADDR(iboot_in, memmem(iboot_in -> buf, iboot_in -> len, "boot-ramdisk", strlen("boot-ramdisk")));
    if (!boot_ramdisk_loc) {
        printf("%s: Failed to find boot-ramdisk string\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found boot-ramdisk string: %p\n", __FUNCTION__, boot_ramdisk_loc);
    void * boot_ramdisk_ldr = find_next_LDR_insn_with_value(iboot_in, boot_ramdisk_loc);
    if (!boot_ramdisk_ldr) {
        printf("%s: Failed to find boot-ramdisk LDR\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found boot-ramdisk LDR: %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, boot_ramdisk_ldr));
    return boot_ramdisk_ldr;
}

void* find_kloader_addr(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    char iBSS_ready[] = "iBSS ready, asking for DFU...\n";

    void* iBSS_ready_ldr = find_next_LDR_insn_with_str(iboot_in, iBSS_ready);
    if (!iBSS_ready_ldr) {
        printf("%s: Failed to find %s\n", __FUNCTION__, iBSS_ready);
        return 0;
    }

    printf("%s: Found LDR at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, iBSS_ready_ldr));

    void* kloader_addr = find_next_MOVT_insn(iBSS_ready_ldr + 10, 14);
    if (!kloader_addr) {
        printf("%s: Failed to find MOVT for kloader!\n", __FUNCTION__);

        kloader_addr = find_next_MOVT_insn(iBSS_ready_ldr + 12, 14);
        if (!kloader_addr) {
            printf("%s: Failed to find slight adjust for MOVT\n", __FUNCTION__);

            kloader_addr = find_next_MOVT_insn(iBSS_ready_ldr + 0x22, 2);
            if (!kloader_addr) {
                printf("%s: Failed to find extended adjust for MOVT\n", __FUNCTION__);

                // MOVT can sometimes be above the LDR
                kloader_addr = find_next_MOVT_insn(iBSS_ready_ldr - 0x30, 0x10);
                if (!kloader_addr) {
                    printf("%s: Failed to find MOVT above LDR!\n", __FUNCTION__);
                    return 0;
                }
            }
        }
    }

    printf("%s: Found MOV for kloader at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, kloader_addr));

    return kloader_addr;
}

void* find_usb_wait_for_image(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    char dfu[] = "Apple Mobile Device (DFU Mode)";

    void* dfu_ldr = find_next_LDR_insn_with_str(iboot_in, dfu);
    if (!dfu_ldr) {
        printf("%s: Failed to find %s\n", __FUNCTION__, dfu);
        return 0;
    }

    printf("%s: Found LDR at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, dfu_ldr));

    void* push = push_r4_r7_lr_search_up(dfu_ldr, 0x20);
    if (!push) {
        printf("%s: Failed to find PUSH {R4,R7,LR}!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found PUSH {R4,R7,LR} at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, push));

    void* push_xref = find_next_bl_insn_to(iboot_in, (uint32_t) ((uintptr_t)GET_IBOOT_FILE_OFFSET(iboot_in, push) + 1));
    if (!push_xref) {
        printf("%s: Failed to find BL to %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, push));
        return 0;
    }

    printf("%s: Found xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, push_xref));

    void* next_push = push_r4_to_r7_lr_search_up(push_xref, 0x10);
    if (!next_push) {
        printf("%s: Failed to find PUSH {R4-R7,LR}!\n", __FUNCTION__);
        
        // Some can still be PUSH {R4,R7,LR}

        next_push = push_r4_r7_lr_search_up(push_xref, 0x10);

        if (!next_push) {
            printf("%s: Failed to find PUSH {R4,R7,LR}!\n", __FUNCTION__);
            return 0;
        }
    }

    printf("%s: Found PUSH {R4-R7,LR} at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, next_push));

    void* next_push_xref = find_next_bl_insn_to(iboot_in, (uint32_t) ((uintptr_t)GET_IBOOT_FILE_OFFSET(iboot_in, next_push) + 1));
    if (!next_push_xref) {
        printf("%s: Failed to find next xref!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found next xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, next_push_xref));

    return next_push_xref;
}

void* find_fsboot_boot_command(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    void* fsboot_str = memstr(iboot_in->buf, iboot_in->len, "fsboot");
    if (!fsboot_str) {
        printf("%s: Failed to find fsboot!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found fsboot at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, fsboot_str));

    void* fsboot_xref = iboot_memmem(iboot_in, fsboot_str);
    if (!fsboot_xref) {
        printf("%s: Failed to find fsboot xref!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found fsboot xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, fsboot_xref));

    return fsboot_xref;
}

void* find_auto_boot(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    void* false_str = memstr(iboot_in->buf, iboot_in->len, "false");
    if (!false_str) {
        printf("%s: Failed to find false str!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found false at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, false_str));

    void* false_xref = iboot_memmem(iboot_in, false_str);
    if (!false_xref) {
        printf("%s: Failed to find false xref!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found false xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, false_xref));

    return false_xref;
}

void* find_platform(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    void* platformStr = memstr(iboot_in->buf, iboot_in->len, PLATFORM_INIT_STR);
    if (!platformStr) {
        printf("%s: Failed to find platform string!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found platform string at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, platformStr));
    return platformStr;
}
