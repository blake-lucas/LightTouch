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
#include <include/rsa_result.h>

#define MEMMEM_RELATIVE(iboot_in, bufstart, needle, needleLen) memmem(bufstart, iboot_in->len - ((char*)(bufstart) - (char*)iboot_in->buf), needle, needleLen)

#define INSN2_MOV_R0_0__MOV_R0_0    0x20002000
#define INSN2_MOV_R4_R3__MOV_R4_R3  0x64c164c1
#define INSN2_MOV_R1_0__MOV_R0_0    0x20002100
#define INSN2_MOV_R0_0__STR_R0_R3    0x60182000
#define INSN2_NOP__NOP              0xBF00BF00
#define INSN2_RETURN_0              0x47702000


#define INSNT_NOP                   0xBF00
#define INSNT_STR_R1_R4_R0          0x5021
#define INSNT_LDR_R0_R0             0x6800

// xerub's iloader
#define INSNT_LDR_R_PC(d, n)        (0x4800 | (((d) & 7) << 8) | ((n) / 4))
unsigned int
make_b_w(int pos, int tgt)
{
    int delta;
    unsigned short pfx;
    unsigned short sfx;
    
    unsigned int omask = 0xB800;
    unsigned int amask = 0x7FF;
    
    delta = tgt - pos - 4; /* range: 0x400000 */
    pfx = 0xF000 | ((delta >> 12) & 0x7FF);
    sfx =  omask | ((delta >>  1) & amask);
    
    return (unsigned int)pfx | ((unsigned int)sfx << 16);
}



int patch_boot_args(struct iboot_img* iboot_in, const char* boot_args) {
	printf("%s: Entering...\n", __FUNCTION__);

	/* Find the pre-defined boot-args from iBoot "rd=md0 ..." */
	void* default_boot_args_str_loc = memstr(iboot_in->buf, iboot_in->len, DEFAULT_BOOTARGS_STR);
	if(!default_boot_args_str_loc) {
		printf("%s: Unable to find default boot-args string!\n", __FUNCTION__);
		return 0;
	}
	printf("%s: Default boot-args string is at %p\n", __FUNCTION__, (void*) GET_IBOOT_FILE_OFFSET(iboot_in, default_boot_args_str_loc));

	/* Find the boot-args string xref within the kernel load routine. */
	void* default_boot_args_xref = iboot_memmem(iboot_in, default_boot_args_str_loc);
	if(!default_boot_args_xref) {
		printf("%s: Unable to find default boot-args string xref!\n", __FUNCTION__);
		return 0;
	}
	printf("%s: boot-args xref is at %p\n", __FUNCTION__, (void*) GET_IBOOT_FILE_OFFSET(iboot_in, default_boot_args_xref));

	/* If new boot-args length exceeds the pre-defined one in iBoot, we need to point the xref somewhere else... */
	if(strlen(boot_args) > strlen(DEFAULT_BOOTARGS_STR)) {
		printf("%s: Relocating boot-args string...\n", __FUNCTION__);

		/* Find the "Reliance on this cert..." string. */
		char* reliance_cert_str_loc = (char*) memstr(iboot_in->buf, iboot_in->len, RELIANCE_CERT_STR);
		if(!reliance_cert_str_loc) {
			printf("%s: Unable to find \"%s\" string!\n", __FUNCTION__, RELIANCE_CERT_STR);
			return 0;
		}
		printf("%s: \"%s\" string found at %p\n", __FUNCTION__, RELIANCE_CERT_STR, GET_IBOOT_FILE_OFFSET(iboot_in, reliance_cert_str_loc));

		/* Point the boot-args xref to the "Reliance on this cert..." string. */
		printf("%s: Pointing default boot-args xref to %p...\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, reliance_cert_str_loc));
		*(uint32_t*)default_boot_args_xref = (uintptr_t) GET_IBOOT_ADDR(iboot_in, reliance_cert_str_loc);

		default_boot_args_str_loc = reliance_cert_str_loc;
	}
	printf("%s: Applying custom boot-args \"%s\"\n", __FUNCTION__, boot_args);
	strcpy(default_boot_args_str_loc, boot_args);

	/* This is where things get tricky... (Might run into issues on older loaders)*/

	/* Patch out the conditional branches... */
	void* _ldr_rd_boot_args = ldr_to(default_boot_args_xref);
	if(!_ldr_rd_boot_args) {
		uintptr_t default_boot_args_str_loc_with_base = (uintptr_t) GET_IBOOT_FILE_OFFSET(iboot_in, default_boot_args_str_loc) + get_iboot_base_address(iboot_in->buf);

		_ldr_rd_boot_args = find_next_LDR_insn_with_value(iboot_in, (uint32_t) default_boot_args_str_loc_with_base);
		if(!_ldr_rd_boot_args) {
			printf("%s: Error locating LDR Rx, =boot_args!\n", __FUNCTION__);
			return 0;
		}
	}

	struct arm32_thumb_LDR* ldr_rd_boot_args = (struct arm32_thumb_LDR*) _ldr_rd_boot_args;
	printf("%s: Found LDR R%d, =boot_args at %p\n", __FUNCTION__, ldr_rd_boot_args->rd, GET_IBOOT_FILE_OFFSET(iboot_in, _ldr_rd_boot_args));

	void* arm32_thumb_IT_insn = ldr_rd_boot_args;
    bool it_found = false;
    uint16_t* itPtrStart = (uint16_t*)arm32_thumb_IT_insn;
    uint16_t* itPtrEnd = itPtrStart + 0x30;

    /* Find IT instruction, if it's even there */

    for (uint16_t* itPtr = itPtrStart; itPtr != itPtrEnd; itPtr++) {
        if (*itPtr == ARM32_THUMB_IT_EQ || *itPtr == ARM32_THUMB_IT_NE || *itPtr == ARM32_THUMB_ITE_NE) {
            it_found = true;
            arm32_thumb_IT_insn = (void*)itPtr;
            break;
        }
    }

    if (it_found) {
        printf("%s: Found IT instruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, arm32_thumb_IT_insn));
    }

    void* _cmp_insn = NULL;

    /* FIXME iOS 10 some reason is represented as iOS 0 instead of 10 */
    int os_vers = get_os_version(iboot_in);
    if (os_vers >= 2 && os_vers <= 4) {
        if (it_found) {
            /* IT instruction found! CMP is right above the IT instruction */
            _cmp_insn = find_next_CMP_insn_with_value(arm32_thumb_IT_insn - 2, 0x10, 0);
        } else {
            if (os_vers == 2) {
                /* iOS 2 has the CMP farther away */
                _cmp_insn = find_next_CMP_insn_with_value(ldr_rd_boot_args - 0x40, 0x10, 0);
            } else {
                /* Images like iPod2,1 don't have IT instruction. It has the CMP above boot-args LDR */
                _cmp_insn = find_next_CMP_insn_with_value(ldr_rd_boot_args - 0x10, 0x20, 0);
            }
        }

        if (!_cmp_insn) {
            printf("%s: Failed to find CMP Rx, #0!\n", __FUNCTION__);
            return 0;
        }

        printf("%s: Found CMP Rx, #0 at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, _cmp_insn));
        struct arm32_thumb* cmp_insn = (struct arm32_thumb*)_cmp_insn;

        cmp_insn->offset = 1;
        return 1;
    } else {
        _cmp_insn = find_next_CMP_insn_with_value(ldr_rd_boot_args, 0x100, 0);
        if (!_cmp_insn) {
            printf("%s: Failed to find CMP Rx, #0!\n", __FUNCTION__);
            return 0;
        }

        printf("%s: Found CMP Rx, #0 at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, _cmp_insn));

        struct arm32_thumb* cmp_insn = (struct arm32_thumb*)_cmp_insn;
        cmp_insn->offset = 1;
    }

	/* MOV Rd, Rs instruction usually follows right after the IT instruction. */
	struct arm32_thumb_hi_reg_op* mov_insn = (struct arm32_thumb_hi_reg_op*) (arm32_thumb_IT_insn + 2);

	printf("%s: Found MOV R%d, R%d at %p\n", __FUNCTION__, mov_insn->rd, mov_insn->rs, GET_IBOOT_FILE_OFFSET(iboot_in, arm32_thumb_IT_insn + 2));

	/* Find the last LDR Rd which holds the null string pointer... */
	int null_str_reg = (ldr_rd_boot_args->rd == mov_insn->rs) ? mov_insn->rd : mov_insn->rs;

    /* + 0x10: Some iBoots have the null string load after the CMP instruction... */

	void* ldr_null_str = find_last_LDR_rd((uintptr_t) (_cmp_insn + 0x10), 0x200, null_str_reg);
	if(!ldr_null_str) {
        
        /* + 0x9 Some iBoots have the null string load after the CMP instruction... */        
        
        ldr_null_str = find_last_LDR_rd((uintptr_t) (_cmp_insn + 0x9), 0x200, null_str_reg);
        if(!ldr_null_str) {
                printf("%s: Unable to find LDR R%d, =null_str\n", __FUNCTION__, null_str_reg);
                return 0;
        }
	}

	printf("%s: Found LDR R%d, =null_str at %p\n", __FUNCTION__, null_str_reg, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_null_str));

	/* Calculate the new PC relative load from the default boot args xref to the LDR Rd, =null_string location. */
	uint32_t diff = (uint32_t) (GET_IBOOT_FILE_OFFSET(iboot_in, default_boot_args_xref) - GET_IBOOT_FILE_OFFSET(iboot_in, ldr_null_str));

	/* T1 LDR PC-based instructions use the immediate 8 bits multiplied by 4. */
	struct arm32_thumb_LDR* ldr_rd_null_str = (struct arm32_thumb_LDR*) ldr_null_str;
	printf("%s: Pointing LDR R%d, =null_str to boot-args xref...\n", __FUNCTION__, ldr_rd_null_str->rd);
	ldr_rd_null_str->imm8 = (diff / 0x4);

	printf("%s: Leaving...\n", __FUNCTION__);
	return 1;
}

int patch_env_boot_args(struct iboot_img* iboot_in) {
    printf("%s: Finding rd=md0 LDR\n", __FUNCTION__);
    char* bootargs_ldr =  find_next_LDR_insn_with_str(iboot_in, DEFAULT_BOOTARGS_STR);
    if(!bootargs_ldr) {
        printf("%s: Failed to find rd=md0 LDR\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found rd=md0 LDR at %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, bootargs_ldr));
    struct arm32_thumb_LDR* ldr_rd_boot_args = (struct arm32_thumb_LDR*) bootargs_ldr;
    char* _cmp_insn = find_next_CMP_insn_with_value(ldr_rd_boot_args, 0x100, 0);
    if(!_cmp_insn) {
        printf("%s: Error locating next CMP instruction!\n", __FUNCTION__);
        return 0;
    }
    struct arm32_thumb* cmp_insn = (struct arm32_thumb*) _cmp_insn;
    printf("%s: Found CMP R%d, #%d at %p\n", __FUNCTION__, cmp_insn->rd, cmp_insn->offset, GET_IBOOT_ADDR(iboot_in, _cmp_insn));
    
    char* _mov_insn = find_Boot_Args_MOV(_cmp_insn);
    if(!_mov_insn) {
        printf("%s: Error finding MOV instruction!\n", __FUNCTION__);
        return 0;
    }
    struct arm32_thumb_hi_reg_op* mov_insn = (struct arm32_thumb_hi_reg_op*) _mov_insn;
    printf("%s: Found mov r%d, r%d at: %p\n", __FUNCTION__, mov_insn->rd, mov_insn->rs, GET_IBOOT_ADDR(iboot_in, _mov_insn));
    void* boot_args_str_loc = find_Boot_Args_String_Location(iboot_in);
    if(!boot_args_str_loc) {
        printf("%s: Error finding boot-args string location!\n", __FUNCTION__);
        return 0;
    }
    void* default_ldr_xref = find_ldr_xref(iboot_in);
    if(!default_ldr_xref) {
        printf("%s: Error finding rd=md0 xref!\n", __FUNCTION__);
        return 0;
    }
    uint32_t getenv_addr = find_GETENV_Addr(iboot_in);
    if(!getenv_addr) {
        printf("%s: Error finding getenv function!\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Pointing rd=md0 ldr to boot-args string location\n", __FUNCTION__);
    *(uint32_t*)default_ldr_xref = (uintptr_t) GET_IBOOT_ADDR(iboot_in, boot_args_str_loc);
    int null_str_reg = (ldr_rd_boot_args->rd == mov_insn->rs) ? mov_insn->rd : mov_insn->rs;
    char* null_ins = find_null_str(_mov_insn, null_str_reg);
    if(!null_ins) {
        printf("%s: Failed to find ldr r%d, = null_str\n", __FUNCTION__, null_str_reg);
        return 0;
    }
    char* Current_ins = (_cmp_insn < null_ins) ? _cmp_insn : null_ins;
    printf("%s: Building mov r0, r%d at %p\n", __FUNCTION__, ldr_rd_boot_args->rd, GET_IBOOT_ADDR(iboot_in, Current_ins));
    Build_MOV((void*)Current_ins, 0x0, ldr_rd_boot_args->rd);
    Current_ins += 0x2;
    printf("%s: Building bl 0x%x at %p\n", __FUNCTION__, getenv_addr, GET_IBOOT_ADDR(iboot_in, Current_ins));
    Build_BL_Long((void*)Current_ins, getenv_addr, GET_IBOOT_ADDR(iboot_in, Current_ins));
    Current_ins += 0x4;
    printf("%s: Building mov r%d, r0 at %p\n", __FUNCTION__, mov_insn->rd, GET_IBOOT_ADDR(iboot_in, Current_ins));
    Build_MOV((void*)Current_ins, mov_insn->rd, 0x0);
    Current_ins += 0x2;
    printf("%s: Nopping extra instructions\n", __FUNCTION__);
    while(Current_ins <= _mov_insn ) {
        Current_ins[0] = 0x00;
        Current_ins[1] = 0xBF; //NOP
        Current_ins += 0x2;
    }
	printf("%s: Leaving\n", __FUNCTION__);
    return 1;
}

int disable_kaslr(struct iboot_img* iboot_in) {
	printf("%s: Entering...\n", __FUNCTION__);
	printf("%s: Finding __TEXT LDR\n", __FUNCTION__);
    char* text_ldr =  find_next_LDR_insn_with_str(iboot_in, "__TEXT");
    if(!text_ldr) {
        printf("%s: Failed to find __TEXT LDR\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found __TEXT LDR at %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, text_ldr));
    printf("%s: Finding push\n", __FUNCTION__);
    void* push = push_search(text_ldr, 0x200, 1);
    if(!push) {
        printf("%s: Failed to find push\n", __FUNCTION__);
        return 0;
    }
	printf("%s: Found push at %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, push));
    printf("%s: Finding bne\n", __FUNCTION__);
    char* bne = branch_thumb_conditional_search(push, 0x50, 0);
    if(!bne) {
        printf("%s: Failed to find bne\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found bne at %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, bne));
    printf("%s: nopping bne\n", __FUNCTION__);
    bne[0] = 0x00;
    bne[1] = 0xBF;
	printf("%s: Leaving\n", __FUNCTION__);
	return 1;
}

int patch_boot_partition(struct iboot_img* iboot_in, int ver) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the BL boot-partition instruction... */
    void* boot_partition_ldr = find_boot_partition_ldr(iboot_in);
    
    if(!boot_partition_ldr) {
        printf("%s: Unable to find boot_partition_ldr!\n", __FUNCTION__);
        return 0;
    }
    
    char *bl_boot_partition = bl_search_down(boot_partition_ldr,0x100);
    if(!bl_boot_partition) {
        printf("%s: Unable to find the boot-partition BL! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Patching boot_partition BL at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_boot_partition));
    
    /* BL boot-partition --> MOVS R0, #0; MOVS R0, #0 */
    *(uint32_t*)bl_boot_partition = bswap32(0x00200020);
    
    /* iOS 9 or later */
    if(ver == 1) {
        printf("%s: iOS 9 or later (for De Rebus Antiquis)\n", __FUNCTION__);
        void* boot_partition_loc = memmem(iboot_in -> buf, iboot_in -> len, "boot-partition", strlen("boot-partition"));
        if (!boot_partition_loc) {
            printf("%s: Failed to find boot-partition string\n", __FUNCTION__);
            return 0;
        }
        printf("%s: Found boot-partition string: %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, boot_partition_loc));
        printf("%s: Patching boot-partition at %p\n", __FUNCTION__, 2);
        *(uint16_t*)boot_partition_loc = bswap16(0x3200);
    }
    
    printf("%s: Leaving...\n", __FUNCTION__);
    return 1;
}

int patch_boot_ramdisk(struct iboot_img* iboot_in) {
    void* boot_ramdisk_ldr = find_boot_ramdisk_ldr(iboot_in);
    
    if(!boot_ramdisk_ldr) {
        printf("%s: Unable to find boot_ramdisk_ldr!\n", __FUNCTION__);
        return 0;
    }
    
    char *bl_boot_ramdisk = bl_search_down(boot_ramdisk_ldr,0x100);
    if(!bl_boot_ramdisk) {
        printf("%s: Unable to find the boot-ramdisk BL! (Image may already be patched?)\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Patching boot_ramdisk BL at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_boot_ramdisk));
    
    /* BL ramdisk --> MOVS R0, #0; MOVS R0, #0 */
    *(uint32_t*)bl_boot_ramdisk = bswap32(0x00200020);
    
    printf("%s: Leaving...\n", __FUNCTION__);
    return 1;
}

int patch_logo(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    uint8_t logo[] = {0x46, 0xF2, 0x6F, 0x70};
    uint8_t *logo_loc = memmem(iboot_in->buf, iboot_in->len, &logo, sizeof(logo));
    if (!logo_loc) {
        printf("%s: Failed to find AppleLogo\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found applelogo: %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, logo_loc));

    printf("%s: Patching logo -> logb ...\n", __FUNCTION__);
    *(uint32_t*)logo_loc = 0x7062f246;

    printf("%s: Leaving...\n", __FUNCTION__);
    return 1;
}

int patch_logo4(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    void* logo_loc = memmem(iboot_in -> buf, iboot_in -> len, "ogol", strlen("ogol"));
    if (!logo_loc) {
        printf("%s: Failed to find logo string\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found main string: %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, logo_loc));
    
    printf("%s: Patching logo -> log4 ...\n", __FUNCTION__);
    *(uint8_t*)logo_loc = 0x34;
    
    printf("%s: Leaving...\n", __FUNCTION__);
    return 1;
}

/* Jump from iBoot to iOS 4.3.3 or lower iBoot via go command */
int patch_433orlower_jumpiBoot(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    int ipad2 = 0;
    void* main_loc = memmem(iboot_in -> buf, iboot_in -> len, "main", strlen("main"));
    if (!main_loc) {
        printf("%s: Failed to find main string\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found main string: %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, main_loc));
    
    struct iboot32_cmd_t* main_function = (struct iboot32_cmd_t*) iboot_memmem(iboot_in, main_loc);
    if(!main_function) {
        printf("%s: Unable to find a ref to \"%p\".\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, main_loc));
        return 0;
    }
    printf("%s: Found the main_function string reference at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, (void*) main_function));
    uint32_t main_function_loc = 4 + GET_IBOOT_FILE_OFFSET(iboot_in, (void*) main_function);
    
    const char* patch_val = "\x00\x28\x08\xBF\x01\x20\x80\xBD";
    void* patch_loc = memmem(iboot_in -> buf, iboot_in -> len, patch_val, sizeof(patch_val));
    if (!patch_loc) {
        printf("%s: Failed to find patch_offset string! Is this iBoot for iPad 2?\n", __FUNCTION__);
        patch_val = "\x0F\x28\x98\xBF\x10\x20\x80\xBD";
        patch_loc = memmem(iboot_in -> buf, iboot_in -> len, patch_val, sizeof(patch_val));
        if (!patch_loc){
            printf("%s: Failed to find patch_offset string\n", __FUNCTION__);
            return 0;
        }
        else
            ipad2 = 1;
    }
    printf("%s: Found patch_offset string: %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, patch_loc));
    uint32_t iBoot4_fix_offset = GET_IBOOT_FILE_OFFSET(iboot_in, patch_loc);
    
    const char* search_payload_header = "\x10\xFF\x2F\xE1\xFE\xFF\xFF\xEA";
    void* payload = memmem(iboot_in -> buf, iboot_in -> len, search_payload_header, sizeof(search_payload_header));
    if (!payload) {
        printf("%s: Failed to find payload_offset\n", __FUNCTION__);
        return 0;
    }
    payload = payload + sizeof(search_payload_header);
    printf("%s: Found payload_offset: %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, payload));
    uint32_t payload_loc = GET_IBOOT_FILE_OFFSET(iboot_in, payload);
    uint32_t baseaddr = get_iboot_base_address(iboot_in->buf);
    
    /* payload shellcode */
    printf("%s: Writing payload\n", __FUNCTION__);
    *(uint16_t*)payload = INSNT_LDR_R_PC(4, 0x14); payload+=2;
    *(uint16_t*)payload = INSNT_LDR_R_PC(0, 0x18); payload+=2;
    *(uint16_t*)payload = INSNT_LDR_R_PC(1, 0x18); payload+=2;
    *(uint16_t*)payload = INSNT_STR_R1_R4_R0; payload+=2;
    *(uint16_t*)payload = INSNT_LDR_R_PC(0, 0x18); payload+=2;
    *(uint16_t*)payload = INSNT_LDR_R_PC(1, 0x1c); payload+=2;
    *(uint16_t*)payload = INSNT_STR_R1_R4_R0; payload+=2;
    *(uint32_t*)payload = *((uint32_t*)main_function+1); payload+=4;
    *(uint32_t*)payload = make_b_w(payload_loc+0x12, main_function_loc+4); payload+=4;
    *(uint16_t*)payload = INSNT_NOP; payload+=2;
    *(uint32_t*)payload = baseaddr; payload+=4;
    *(uint32_t*)payload = iBoot4_fix_offset; payload+=4;
    *(uint32_t*)payload = *(uint32_t*)patch_loc; payload+=4;
    *(uint32_t*)payload = iBoot4_fix_offset+4; payload+=4;
    *(uint32_t*)payload = *((uint32_t*)patch_loc+1);
    
    /* hook main function */
    *((uint32_t*)main_function+1) = make_b_w(main_function_loc, payload_loc);
    
    /* fix jump to iBoot */
    if (ipad2)
    {
        *(uint32_t*)patch_loc = 0xbf982810;patch_loc+=4;
        *(uint32_t*)patch_loc = 0xbd802011;
    }
    else
    {
        *(uint32_t*)patch_loc = 0xbf982801;patch_loc+=4;
        *(uint32_t*)patch_loc = 0xbd802002;
    }
    
    printf("%s: Leaving\n", __FUNCTION__);
    return 1;
}

int patch_setenv_cmd(struct iboot_img* iboot_in) {
	printf("%s: Entering...\n", __FUNCTION__);

	char* setenvstr = "setenv";

	size_t setenv_str_len = strlen(setenvstr);
	size_t setenv_bytes_len = setenv_str_len + 2;

	char* setenv_bytes = (char*)malloc(setenv_bytes_len);
	if(!setenv_bytes) {
		printf("%s: Out of memory.\n", __FUNCTION__);
		return 0;
	}

	memset(setenv_bytes, 0, setenv_bytes_len);

	/* Fill the buffer to make the string look like \0<cmd>\0 */
	for(int i = 0; i < setenv_str_len; i++) {
		setenv_bytes[i+1] = setenvstr[i];
	}

	printf("%s: Finding setenv command string\n", __FUNCTION__);
	void* setenv_ptr_str_loc = memmem(iboot_in->buf, iboot_in->len, setenv_bytes, setenv_bytes_len);
	if(!setenv_ptr_str_loc) {
		printf("%s: Unable to find the setenv command\n", __FUNCTION__);
		return 0;
	}
	setenv_ptr_str_loc++;

	printf("%s: Found the setenv command string at %p\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, setenv_ptr_str_loc));

	printf("%s: Finding setenv command\n", __FUNCTION__);

	struct iboot32_cmd_t* setenv = (struct iboot32_cmd_t*) iboot_memmem(iboot_in, setenv_ptr_str_loc);
	if(!setenv) {
		printf("%s: Unable to find a ref to \"%p\".\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, setenv_ptr_str_loc));
		return 0;
	}
	size_t setenvptr = setenv->cmd_ptr - get_iboot_base_address(iboot_in->buf);
	printf("%s: Found the cmd string reference at %p\n", __FUNCTION__, setenvptr + get_iboot_base_address(iboot_in->buf));
	char* p = iboot_in->buf;
	void* firstBL = bl_search_down((void*)p + setenvptr, 0x50);

	if(!firstBL) {
		printf("%s: Unable to find first bl\n", __FUNCTION__);
		return 0;
	}

	void* theCheck = bl_search_down(firstBL+0x4, 0x50);
	
	if(!theCheck) {
		printf("%s: Unable to find environment variable check\n", __FUNCTION__);
		return 0;
	}
	printf("%s Patching check to always allow env\n", __FUNCTION__);
	
	*(uint32_t*) theCheck = bswap32(0x00200020);

	printf("%s: Leaving\n", __FUNCTION__);
	return 1;
}
int patch_cmd_handler(struct iboot_img* iboot_in, const char* cmd_str, uint32_t ptr) {
	printf("%s: Entering...\n", __FUNCTION__);

	size_t cmd_str_len = strlen(cmd_str);
	size_t cmd_bytes_len = cmd_str_len + 2;

	char* cmd_bytes = (char*)malloc(cmd_bytes_len);
	if(!cmd_bytes) {
		printf("%s: Out of memory.\n", __FUNCTION__);
		return 0;
	}

	memset(cmd_bytes, 0, cmd_bytes_len);

	/* Fill the buffer to make the string look like \0<cmd>\0 */
	for(int i = 0; i < cmd_str_len; i++) {
		cmd_bytes[i+1] = cmd_str[i];
	}

	/* Find the cmd handler string... */
	void* cmd_ptr_str_loc = memmem(iboot_in->buf, iboot_in->len, cmd_bytes, cmd_bytes_len);

	free(cmd_bytes);

	if(!cmd_ptr_str_loc) {
		printf("%s: Unable to find the cmd \"%s\".\n", __FUNCTION__, cmd_str);
		return 0;
	}
	/* +1 to bring the found offset to the beginning of the cmd string... \0<cmd>\0 --> <cmd>\0 */
	cmd_ptr_str_loc++;

	printf("%s: Found the cmd string at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, cmd_ptr_str_loc));

	/* Resolve the cmd table referencing the cmd string... */
	struct iboot32_cmd_t* cmd = (struct iboot32_cmd_t*) iboot_memmem(iboot_in, cmd_ptr_str_loc);
	if(!cmd) {
		printf("%s: Unable to find a ref to \"%p\".\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, cmd_ptr_str_loc));
		return 0;
	}

	printf("%s: Found the cmd string reference at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, (void*) cmd));

	printf("%s: Pointing \"%s\" from 0x%08x to 0x%08x...\n", __FUNCTION__, cmd_str, cmd->cmd_ptr, ptr);

	/* Point cmd handler to user-specified pointer... */
	cmd->cmd_ptr = ptr;

	printf("%s: Leaving...\n", __FUNCTION__);

	return 1;
}

int patch_debug_enabled(struct iboot_img* iboot_in) {
	printf("%s: Entering...\n", __FUNCTION__);

	/* Find the BL get_value_for_dtre_var insn... */
	void* get_value_for_dtre_bl = find_dtre_get_value_bl_insn(iboot_in, DEBUG_ENABLED_DTRE_VAR_STR);
	if(!get_value_for_dtre_bl) {
		printf("%s: Unable to find appropriate BL insn.\n", __FUNCTION__);
		return 0;
	}

	printf("%s: Patching BL insn at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, get_value_for_dtre_bl));

	/* BL get_dtre_value --> MOVS R0, #1; MOVS R0, #1 */
	*(uint32_t*)get_value_for_dtre_bl = bswap32(0x01200120);

	printf("%s: Leaving...\n", __FUNCTION__);
	return 1;
}

int patch_rsa_check(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);
    
    /* Find the BL verify_shsh instruction... */
    int os_vers = get_os_version(iboot_in);
    if(os_vers == 4 || os_vers == 3) {
        void* rsa_check_3_4 = find_rsa_check_3_4(iboot_in);
        if(!rsa_check_3_4) {
            return 0;
        }
        /* BL --> MOVS R0, #0; MOVS R0, #0 */
        printf("%s: Patching RSA at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, rsa_check_3_4));
        *(uint32_t*)rsa_check_3_4 = bswap32(0x00200020);

        void* ldr_ecid = find_ldr_ecid(iboot_in);
        if(!ldr_ecid) {
            printf("%s: Unable to find RSA check!\n", __FUNCTION__);
            return 0;
        }
        /* BL --> MOVS R0, #0; MOVS R0, #0 */
        printf("%s: Patching BL ECID at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_ecid));
        *(uint32_t*)ldr_ecid = bswap32(0x00200020);
        
        void* ldr_bord = find_ldr_bord(iboot_in);
        if(!ldr_bord) {
            printf("%s: Unable to find BL BORD!\n", __FUNCTION__);
            return 0;
        }
        /* BL --> MOVS R0, #0; MOVS R0, #0 */
        printf("%s: Patching BL BORD at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_bord));
        *(uint32_t*)ldr_bord = bswap32(0x00200020);
        
        void* ldr_prod = find_ldr_prod(iboot_in);
        if(!ldr_prod) {
            printf("%s: Unable to find BL PROD!\n", __FUNCTION__);
            return 0;
        }
        /* BL --> MOVS R0, #0; MOVS R0, #0 */
        printf("%s: Patching BL PROD at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_prod));
        *(uint32_t*)ldr_prod = bswap32(0x00200020);
        
        void* ldr_sepo = find_ldr_sepo(iboot_in);
        if(!ldr_sepo) {
            printf("%s: Unable to find BL SEPO!\n", __FUNCTION__);
            return 0;
        }
        /* BL --> MOVS R0, #0; MOVS R0, #0 */
        printf("%s: Patching BL SEPO at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_sepo));
        *(uint32_t*)ldr_sepo = bswap32(0x00200020);
        
        return 1;
    }
    
    void* bl_verify_shsh = find_bl_verify_shsh(iboot_in);
    if(!bl_verify_shsh) {
        printf("%s: Unable to find BL verify_shsh!\n", __FUNCTION__);
        return 0;
    }
    
    if (os_vers >= 5 && os_vers <= 7 && !rsa_stack_result(iboot_in, bl_verify_shsh)) {
        void *data = rsa_legacy_data_block(iboot_in, bl_verify_shsh);
        if (!data) {
            printf("%s: Unsupported verification result ABI; refusing patch.\n", __FUNCTION__);
            return 0;
        }
        printf("%s: Bypassing legacy authentication at %p, retaining result initialization and DATA extraction at %p...\n",
               __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_verify_shsh), GET_IBOOT_FILE_OFFSET(iboot_in, data));
        *(uint32_t *)bl_verify_shsh = bswap32(0x002000bf); /* MOVS R0,#0; NOP */
        /* Replace CBZ with B; its PC is call+12. Search range fits Thumb B's imm11. */
        uint16_t branch = 0xe000 | (((unsigned char *)data - (unsigned char *)bl_verify_shsh - 12) / 2);
        memcpy((unsigned char *)bl_verify_shsh + 8, &branch, sizeof(branch));
        return 1;
    }
    printf("%s: Patching BL verify_shsh at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, bl_verify_shsh));
    /* BL verify_shsh --> MOVS R0, #0; STR R0, [R3] */
    *(uint32_t*)bl_verify_shsh = bswap32(0x00201860);
    
    printf("%s: Leaving...\n", __FUNCTION__);
    return 1;
}

int patch_boot_mode(struct iboot_img* iboot_in, int mode) {
    printf("%s: Entering...\n", __FUNCTION__);
    /* Find the variable string... */
    char* var_str_loc = memstr(iboot_in->buf, iboot_in->len, "debug-uarts");
    if(!var_str_loc) {
        printf("%s: Unable to find %s string!\n", __FUNCTION__, "debug-uarts");
        return 0;
    }
    printf("%s: %s string is at %p\n", __FUNCTION__, "debug-uarts", (void*) GET_IBOOT_FILE_OFFSET(iboot_in, var_str_loc));
    
    /* Find the variable string xref... */
    uint32_t* var_xref = iboot_memmem(iboot_in, var_str_loc);
    if(!var_xref) {
        printf("%s: Unable to find %s string xref!\n", __FUNCTION__, "debug-uarts");
        return 0;
    }
    
    void* var_ldr = ldr_to(var_xref);
    if(!var_ldr) {
        printf("%s: Unable to find %s string LDR from xref!\n", __FUNCTION__, "debug-uarts");
        return 0;
    }
    
    void* firstBL = bl_search_down(var_ldr+4, 0x10);
    if(!firstBL) {
        printf("%s: Unable to find firstBL!\n", __FUNCTION__);
        return 0;
    }
    void* secondBL = bl_search_down(firstBL+4, 0x10);
    if(!secondBL) {
        printf("%s: Unable to find secondBL!\n", __FUNCTION__);
        return 0;
    }
    void* thefunc = bl_search_down(secondBL+4, 0x10);
    if(!thefunc) {
        printf("%s: Unable to find thefunc!\n", __FUNCTION__);
        return 0;
    }
    
    void *afterBL = bl_search_down(thefunc+4, 0x10);
    if(!afterBL) {
        printf("%s: Unable to find afterBL!\n", __FUNCTION__);
        return 0;
    }
    
    if (afterBL-4 == thefunc) {
        printf("%s: afterbl is too close!\n", __FUNCTION__);
        return 0;
    }
    
    uint32_t *dst = resolve_bl32(thefunc);
    if(!afterBL) {
        printf("%s: Unable to find dst!\n", __FUNCTION__);
        return 0;
    }
    
    dst = (uint32_t*)((uint8_t*)dst-1);
    printf("%s: dst is at %p\n", __FUNCTION__, (void*) GET_IBOOT_FILE_OFFSET(iboot_in, (void*)dst));
    if(mode == 0) { /* local (iBoot) mode */
        *dst = 0x47702000; // return 0;
    }
    if(mode == 1) { /* remote (iBEC) mode */
        *dst = 0x47702001; // return 1;
    }
    
    return 1;
}


int patch_ticket_check(struct iboot_img* iboot_in) {
#define pointer(p) (__pointer[0] = (uint32_t)p & 0xff, __pointer[1] = ((uint32_t)p/0x100) & 0xff, __pointer[2] = ((uint32_t)p/0x10000) & 0xff, __pointer[3] = ((uint32_t)p/0x1000000) & 0xff, _pointer)
    char __pointer[4];
    char *_pointer = __pointer;
    printf("%s: Entering...\n", __FUNCTION__);
    char *bl_stack_fail = NULL;
    char *NOPstart = NULL;
    char *NOPstop = NULL;
    
    /* find iBoot_vers_str */
    const char* iboot_vers_str = memstr(iboot_in->buf, iboot_in->len, "iBoot-");
    if (!iboot_vers_str) {
        printf("%s: Unable to find iboot_vers_str!\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found iBoot baseaddr %p\n", __FUNCTION__, get_iboot_base_address(iboot_in->buf));
    printf("%s: Found iboot_vers_str at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, iboot_vers_str));
    
    
    /* find pointer to vers_str (should be a few bytes below string) */
    uint32_t vers_str_iboot = (uint32_t)GET_IBOOT_ADDR(iboot_in,iboot_vers_str);
    char *str_pointer = MEMMEM_RELATIVE(iboot_in, iboot_vers_str, pointer(vers_str_iboot), 4);
    if (!str_pointer) {
        printf("%s: Unable to find str_pointer!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found str_pointer at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, str_pointer));


    /* find 3rd xref */
    uint32_t *str_pointer_iboot = (uint32_t)GET_IBOOT_ADDR(iboot_in,str_pointer);
    char *iboot_str_3_xref = iboot_in->buf;
    for (int i=0; i<3; i++) {
        if (!(iboot_str_3_xref = MEMMEM_RELATIVE(iboot_in, iboot_str_3_xref+1, pointer(str_pointer_iboot), 4))){
            printf("%s: Unable to find %d iboot_str_3_xref!\n", __FUNCTION__,i+1);
            return 0;
        }
    }
    
    printf("%s: Found iboot_str_3_xref at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, iboot_str_3_xref));
    
    /* find ldr rx = iboot_str_3_xref */
    char *ldr_intruction = ldr_pcrel_search_up(iboot_str_3_xref, 0x100);
    if (!ldr_intruction) {
        printf("%s: Unable to find ldr_intruction!\n", __FUNCTION__);
        return 0;
    }
    
    printf("%s: Found ldr_intruction at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, ldr_intruction));
    
    char *last_good_bl = bl_search_down(ldr_intruction,0x100);
    if (!last_good_bl) {
        printf("%s: Unable to find last_good_bl!\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found last_good_bl at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, last_good_bl));
    last_good_bl +=4;
    
    char *next_pop = pop_search(last_good_bl,0x100,0);
    if (!next_pop) {
        printf("%s: Unable to find next_pop!\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found next_pop at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, next_pop));
    printf("%s: Found next_pop at %p...\n", __FUNCTION__, GET_IBOOT_ADDR(iboot_in, next_pop));
    
    char *last_branch = branch_search(next_pop,0x20,1);
    char *prev_mov_r0_fail = pattern_search(next_pop, 0x20, bswap32(0x4ff0ff30), bswap32(0x4ff0ff30), -2);

    if (prev_mov_r0_fail && prev_mov_r0_fail > last_branch) {
        printf("%s: Detected prev_mov_r0_fail at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, prev_mov_r0_fail));
        last_branch = prev_mov_r0_fail-2; //last branch is a BL
    }
    
    if (!last_branch) {
        printf("%s: Unable to find last_branch!\n", __FUNCTION__);
        return 0;
    }
    printf("%s: Found last_branch at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, last_branch));
    
    
    printf("%s: Patching in mov.w r0, #0 at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, last_good_bl));
    *(uint32_t*)last_good_bl = bswap32(0x4ff00000);
    last_good_bl +=4;
    
    printf("%s: Patching in mov.w r1, #0 at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, last_good_bl));
    *(uint32_t*)last_good_bl = bswap32(0x4ff00001);
    last_good_bl +=4;
    
    NOPstart = last_good_bl;
    NOPstop = last_branch+2;
    
    //because fuck clean patches
    printf("%s: NOPing useless stuff at %p to %p ...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, NOPstart),  GET_IBOOT_FILE_OFFSET(iboot_in, NOPstop));
    
    while (NOPstart<NOPstop) {
        NOPstart[0] = 0x00;
        NOPstart[1] = 0xBF; //NOP
        NOPstart +=2;
    }

    if (*(uint32_t*)NOPstop == bswap32(0x4ff0ff30)){ //mov.w      r0, #0xffffffff
        printf("%s: Detected mov r0, #0xffffffff at NOPstop\n", __FUNCTION__);
        printf("%s: Applying additional mov.w r0, #0 patch at %p...\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, NOPstop));
        /* mov.w      r0, #0xffffffff -->  mov.w      r0, #0x0 */
        *(uint32_t*)NOPstop = bswap32(0x4ff00000);
    }
        
    
    printf("%s: Leaving...\n", __FUNCTION__);
    return 1;
}

int patch_bgcolor(struct iboot_img* iboot_in, const char* bgcolor) {
    
    printf("%s: Entering...\n", __FUNCTION__);
    
    if (strlen(bgcolor) != 6) {
        printf("%s: Unable to decode passed color!\n", __FUNCTION__);
        return 0;
    }
    
    uint8_t red, green, blue;
    
    char *tmp = malloc(3);
    memset(tmp, 0, 3);
    
    strncpy(tmp, bgcolor, 2);
    red = strtol(tmp, NULL, 16);
    
    strncpy(tmp, bgcolor+2, 2);
    green = strtol(tmp, NULL, 16);
    
    strncpy(tmp, bgcolor+4, 2);
    blue = strtol(tmp, NULL, 16);
    
    printf("%s: red=%d green=%d blue=%d\n", __FUNCTION__, red, green, blue);
    
    
    uint8_t MOV_r1_logo[] = {0x46, 0xF2, 0x6F, 0x70, 0xC6, 0xF6, 0x6F, 0x40};
    
    uint8_t *MOV_r1_logo_ptr = memmem(iboot_in->buf, iboot_in->len, &MOV_r1_logo, sizeof(MOV_r1_logo));
    if (!MOV_r1_logo_ptr) {
        printf("%s: Unable to find MOV R1, #'logo'\n", __FUNCTION__);
        return 0;
    } else {
        printf("%s: Found MOV R1, #'logo' at %p\n", __FUNCTION__, (void*)MOV_r1_logo_ptr-(iboot_in->buf));
    }
    
    
    uint8_t setbgcolor_args[] = {0x00, 0x20, 0x00, 0x21, 0x00, 0x22};
    
    uint8_t *setbgcolor_args_ptr = memmem(MOV_r1_logo_ptr-0x80, 0x80, &setbgcolor_args, sizeof(setbgcolor_args));
    if (!setbgcolor_args_ptr) {
        printf("%s: Unable to find setbgcolor() args\n", __FUNCTION__);
        return 0;
    } else {
        printf("%s: Found setbgcolor() args at %p\n", __FUNCTION__, (void*)setbgcolor_args_ptr-(iboot_in->buf));
    }
    
    setbgcolor_args[0] = red;
    setbgcolor_args[2] = green;
    setbgcolor_args[4] = blue;
    
    memmove(setbgcolor_args_ptr, &setbgcolor_args, sizeof(setbgcolor_args));
    printf("%s: Overwriting setbgcolor() args\n", __FUNCTION__);
    
    printf("%s: Leaving...\n", __FUNCTION__);
    return 1;
}

int patch_dualboot_ibss(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    uint32_t iBootType = get_iBoot_type(iboot_in);
    if (iBootType != IBOOT_TYPE_IBSS) {
        printf("%s: This image is not an iBSS!\n", __FUNCTION__);
        return 0;
    }

    // TODO: I need to test if all of this is doable for iBSS <= 4.x
    int osVersion = get_os_version(iboot_in);
    if (osVersion < 5 && has_kernel_load(iboot_in)) {
        printf("%s: Detected iBSS is pre-iOS 5! Please use an iBEC instead!\n", __FUNCTION__);
        return 0;
    }

    void* kloader_addr = find_kloader_addr(iboot_in);
    if (!kloader_addr) {
        printf("%s: Failed to find kloader MOV!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found kloader MOV at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, kloader_addr));

    void* platform_str = find_platform(iboot_in);
    if (!platform_str) {
        printf("%s: Failed to get platform!\n", __FUNCTION__);
        return 0;
    }

    uint32_t platform = *(uint32_t*)(platform_str + strlen(PLATFORM_INIT_STR));

    if (platform == PLATFORM_8920 || platform == PLATFORM_8922) {
        printf("%s: Using 920 patch!\n", __FUNCTION__);
        *(uint32_t*)kloader_addr = bswap32(0xC6F6D071);
    } else if (platform == PLATFORM_8930 || platform == PLATFORM_8950 || platform == PLATFORM_8955) {
        printf("%s: Image is not 920 or 940. Using default patch!\n", __FUNCTION__);
        *(uint32_t*)kloader_addr = bswap32(0xC7F6D074);
    } else if (platform == PLATFORM_8940 || platform == PLATFORM_8942 || platform == PLATFORM_8945 || platform == PLATFORM_8947) {
        printf("%s: Using 940 patch!\n", __FUNCTION__);
        *(uint32_t*)kloader_addr = bswap32(0xCBF6D074);
    } else {
        printf("%s: Unsupported platform %x\n", __FUNCTION__, platform);
        return 0;
    }

    void* usb = find_usb_wait_for_image(iboot_in);
    if (!usb) {
        printf("%s: Failed to find usb_wait_for_image()!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found usb_wait_for_image() at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, usb));

    *(uint32_t*)usb = bswap32(0x00BF00BF);

    // Patch BLT

    void* blt = branch_thumb_conditional_search(usb, 10, 0);
    if (!blt) {
        printf("%s: Failed to find BLT to patch!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found BLT at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, blt));

    *(uint16_t*) blt = bswap16(0x00BF);

    return 1;
}

int patch_dualboot_ibec(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    uint32_t iBootType = get_iBoot_type(iboot_in);
    if (iBootType != IBOOT_TYPE_IBEC) {
        printf("%s: This image is not an iBEC!\n", __FUNCTION__);
        return 0;
    }

    void* fsboot = find_fsboot_boot_command(iboot_in);
    if (!fsboot) {
        printf("%s: Failed to find fsboot!\n", __FUNCTION__);
        return 0;
    }

    void* upgrade = memstr(iboot_in->buf, iboot_in->len, "upgrade");
    if (!upgrade) {
        printf("%s: Failed to find upgrade str!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found upgrade at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, upgrade));

    *(uint32_t*)fsboot = GET_IBOOT_ADDR(iboot_in, upgrade);

    void* auto_boot_false = find_auto_boot(iboot_in);
    if (!auto_boot_false) {
        printf("%s: Failed to find auto-boot=false!\n", __FUNCTION__);
        return 0;
    }

    void* true_str = memstr(iboot_in->buf, iboot_in->len, "true");
    if (!true_str) {
        printf("%s: Failed to find true str!\n", __FUNCTION__);
        return 0;
    }

    printf("%s: Found true str at %p\n", __FUNCTION__, GET_IBOOT_FILE_OFFSET(iboot_in, true_str));

    *(uint32_t*)auto_boot_false = GET_IBOOT_ADDR(iboot_in, true_str);

    return 1;
}

int patch_dualboot(struct iboot_img* iboot_in) {
    printf("%s: Entering...\n", __FUNCTION__);

    uint32_t imageType = get_iBoot_type(iboot_in);
    int ret = 0;

    if (imageType == IBOOT_TYPE_IBSS) {
        ret = patch_dualboot_ibss(iboot_in);
    } else if (imageType == IBOOT_TYPE_IBEC) {
        ret = patch_dualboot_ibec(iboot_in);
    } else {
        printf("%s: Image must be either iBSS or iBEC!\n", __FUNCTION__);
        return 0;
    }

    if (!ret) {
        printf("%s: Failed to apply dualboot patches!\n", __FUNCTION__);
        return 0;
    }

    return 1;
}
