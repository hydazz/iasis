MEMORY
{
  FLASH : ORIGIN = 0x10000000, LENGTH = 2048K
  RAM   : ORIGIN = 0x20000000, LENGTH = 512K
}

/* RP2350 image placement follows Embassy's RP235x example (Apache-2.0).
 * https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x
 */
SECTIONS
{
  .start_block : ALIGN(4)
  {
    __start_block_addr = .;
    KEEP(*(.start_block));
  } > FLASH
} INSERT AFTER .vector_table;

_stext = ALIGN(ADDR(.start_block) + SIZEOF(.start_block), ALIGNOF(.text));

SECTIONS
{
  .end_block : ALIGN(4)
  {
    __end_block_addr = .;
    KEEP(*(.end_block));
  } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
ASSERT(__start_block_addr - ORIGIN(FLASH) < 4096, "RP2350 image definition must be in the first 4 KiB");
