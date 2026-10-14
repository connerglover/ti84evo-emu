/*
 * Memory map of the armv8m end-to-end test programs; it must match the test bus in
 * crates/armv8m/tests/programs.rs. The default IDAU makes addresses with bit 28 set Secure
 * (and Non-secure callable where the SAU says so), the rest Non-secure.
 *
 * Each program links its Secure and Non-secure code into one image (so either side can name
 * the other's symbols); build.sh / build.ps1 then extract the Secure part (.s_text and the
 * .s_nsc veneers, loaded at 0x1000_0000) and the Non-secure part (.ns_text, at 0x0020_0000).
 * RAM is NOLOAD: the runner starts it zeroed.
 */

MEMORY
{
    S_FLASH  (rx) : ORIGIN = 0x10000000, LENGTH = 64K
    NS_FLASH (rx) : ORIGIN = 0x00200000, LENGTH = 64K
    NS_RAM   (rw) : ORIGIN = 0x20000000, LENGTH = 64K
    S_RAM    (rw) : ORIGIN = 0x30000000, LENGTH = 64K
}

ENTRY(s_reset)

SECTIONS
{
    .s_text :
    {
        KEEP(*(.s_vectors))
        *(.s_text .s_text.*)
        *(.s_rodata .s_rodata.*)
    } > S_FLASH

    /* SG veneers: the SAU's Non-secure callable region covers exactly this (32-byte granules). */
    .s_nsc : ALIGN(32)
    {
        __nsc_start = .;
        KEEP(*(.s_nsc))
        . = ALIGN(32);
        __nsc_end = .;
    } > S_FLASH

    .ns_text :
    {
        KEEP(*(.ns_vectors))
        *(.ns_text .ns_text.*)
        *(.ns_rodata .ns_rodata.*)
    } > NS_FLASH

    .s_bss (NOLOAD) : { *(.s_bss .s_bss.*) } > S_RAM
    .ns_bss (NOLOAD) : { *(.ns_bss .ns_bss.*) } > NS_RAM

    /* Every program places its code and data in the sections above explicitly. */
    .stray (NOLOAD) : { *(.text .text.* .rodata .rodata.* .data .data.* .bss .bss.*) } > S_RAM
    ASSERT(SIZEOF(.stray) == 0, "code or data outside the .s_* / .ns_* sections")

    __ns_flash_start = ORIGIN(NS_FLASH);
    __ns_flash_end = ORIGIN(NS_FLASH) + LENGTH(NS_FLASH);
    __ns_ram_start = ORIGIN(NS_RAM);
    __ns_ram_end = ORIGIN(NS_RAM) + LENGTH(NS_RAM);

    /* Main stacks at the top of each RAM; a Secure process stack halfway down. */
    __s_stack_top = ORIGIN(S_RAM) + LENGTH(S_RAM);
    __s_psp_top = ORIGIN(S_RAM) + LENGTH(S_RAM) / 2;
    __ns_stack_top = ORIGIN(NS_RAM) + LENGTH(NS_RAM);
}
