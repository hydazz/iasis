MEMORY
{
  /* The last two 4 KiB sectors hold hub settings. */
  FLASH : ORIGIN = 0x10000000, LENGTH = 2040K
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
    KEEP(*(.boot_info));
  } > FLASH
} INSERT AFTER .vector_table;

_stext = ALIGN(ADDR(.start_block) + SIZEOF(.start_block), ALIGNOF(.text));

/* The name and version picotool reads, found through .boot_info. */
SECTIONS
{
  .note.gnu.build-id : ALIGN(4)
  {
    KEEP(*(.note.gnu.build-id))
  } > FLASH
  /* ELF note header and padded GNU name precede the SHA-1 descriptor. */
  __iasis_build_id = ADDR(.note.gnu.build-id) + 16;

  .bi_entries : ALIGN(4)
  {
    __bi_entries_start = .;
    KEEP(*(.bi_entries));
    . = ALIGN(4);
    __bi_entries_end = .;
  } > FLASH
} INSERT AFTER .text;

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
ASSERT(SIZEOF(.note.gnu.build-id) == 36, "expected a GNU SHA-1 build ID");
