//! Paging subsystem. *Note: uses recursive mapping.*
//!
//! Was extremely ripped off from Phil Oppermann's tutorials, because I didn't feel like writing
//! a paging system off the top of my head. Nowadays, it's a bit more _mine_.

#![cfg_attr(feature = "cargo-clippy", allow(unreadable_literal))]

use core::ops::{Deref, DerefMut};
use multiboot2::BootInformation;

mod frame;
pub mod frame_allocators;
mod mapper;
mod page;
pub mod table;
mod temporary_page;

pub use self::frame::Frame;
pub use self::frame_allocators::FrameAllocator;
use self::mapper::Mapper;
pub use self::page::{Page, PageIter};
use self::table::{EntryFlags, Table};
use self::temporary_page::TemporaryPage;

/// Helper type aliases used to make function signatures more expressive
///
/// # TODO
/// Replace these with the equivalent newtypes from the `x86_64` crate.
pub type PhysicalAddress = usize;
pub type VirtualAddress = usize;

pub struct ActivePageTable {
    mapper: Mapper,
}

impl Deref for ActivePageTable {
    type Target = Mapper;
    fn deref(&self) -> &Mapper {
        &self.mapper
    }
}

impl DerefMut for ActivePageTable {
    fn deref_mut(&mut self) -> &mut Mapper {
        &mut self.mapper
    }
}

impl ActivePageTable {
    unsafe fn new() -> ActivePageTable {
        ActivePageTable {
            mapper: Mapper::new(),
        }
    }

    /// Executes a closure, with a different page table recursively mapped
    pub fn with<F>(&mut self, table: &mut InactivePageTable, scratch_page: &mut TemporaryPage, f: F)
    where
        F: FnOnce(&mut Mapper),
    {
        use x86_64::instructions::tlb;
        use x86_64::registers::control::Cr3;

        {
            // Backup the original P4 pointer
            let backup = Frame::containing_address(Cr3::read().0.start_address().as_u64() as usize);

            // Map a scratch page to the current p4 table
            let p4_table = scratch_page.map_table_frame(backup.clone(), self);

            // Overwrite main P4 recursive mapping
            self.p4_mut()[511].set(
                table.p4_frame.clone(),
                EntryFlags::PRESENT | EntryFlags::WRITABLE,
            );
            tlb::flush_all(); // flush *all* TLBs to prevent fuckiness

            // Execute f in context of the new page table
            f(self);

            // Restore the original pointer to P4
            p4_table[511].set(backup, EntryFlags::PRESENT | EntryFlags::WRITABLE);
            tlb::flush_all(); // prevent fuckiness
        }

        scratch_page.unmap(self);
    }

    /// Switches to a new [`InactivePageTable`], making it active.
    ///
    /// Note: We don't need to flush the TLB here, as the CPU automatically flushes
    /// the TLB when the P4 table is switched.
    pub fn switch(&mut self, new_table: InactivePageTable) -> InactivePageTable {
        use x86_64::registers::control::Cr3;
        use x86_64::structures::paging::PhysFrame;
        use x86_64::PhysAddr;

        let old_table = InactivePageTable {
            p4_frame: Frame::containing_address(Cr3::read().0.start_address().as_u64() as usize),
        };

        unsafe {
            Cr3::write(
                PhysFrame::from_start_address(PhysAddr::new(
                    new_table.p4_frame.start_address() as u64
                ))
                .unwrap(),
                Cr3::read().1,
            );
        }

        old_table
    }
}

/// Owns an inactive P4 table.
pub struct InactivePageTable {
    p4_frame: Frame,
}

impl InactivePageTable {
    pub fn new(
        frame: Frame,
        active_table: &mut ActivePageTable,
        temporary_page: &mut TemporaryPage,
    ) -> InactivePageTable {
        {
            let table = temporary_page.map_table_frame(frame.clone(), active_table);

            // zero the new inactive page table
            table.zero();

            // set up a recursive mapping for this table
            table[511].set(frame.clone(), EntryFlags::PRESENT | EntryFlags::WRITABLE);
        }
        temporary_page.unmap(active_table);

        InactivePageTable { p4_frame: frame }
    }
}

/// Remap the kernel
pub fn remap_kernel<A>(allocator: &mut A, boot_info: &BootInformation) -> ActivePageTable
where
    A: FrameAllocator,
{
    let mut scratch_page = TemporaryPage::new(Page::new(0xabadcafe), allocator);

    let mut active_table = unsafe { ActivePageTable::new() };
    let mut new_table = {
        let frame = allocator.alloc_frame().expect(
            "Attempted to allocate a frame for a new page table, but no frames are available!",
        );
        InactivePageTable::new(frame, &mut active_table, &mut scratch_page)
    };

    active_table.with(&mut new_table, &mut scratch_page, |mapper| {
        let elf_sections_tag = boot_info
            .elf_sections_tag()
            .expect("ELF sections tag required!");

        // -- Identity map the kernel sections
        for section in elf_sections_tag.sections() {
            if !section.is_allocated() {
                // section is not loaded to memory
                continue;
            }

            assert!(
                section.start_address() as usize % Frame::SIZE == 0,
                "ELF sections must be page-aligned!"
            );
            debug!(
                "Mapping section at addr: {:#x}, size: {:#x}",
                section.start_address(),
                section.size()
            );

            let flags = EntryFlags::from_elf_section_flags(&section);
            let start_frame = Frame::containing_address(section.start_address() as usize);
            let end_frame = Frame::containing_address(section.end_address() as usize - 1);
            for frame in Frame::range_inclusive(start_frame, end_frame) {
                mapper.identity_map(frame, flags, allocator);
            }
        }

        // -- Identity map the VGA console buffer (it's only one frame long)
        let vga_buffer_frame = Frame::containing_address(0xb8000);
        mapper.identity_map(vga_buffer_frame, EntryFlags::WRITABLE, allocator);

        // -- Identity map the multiboot info structure
        let multiboot_start = Frame::containing_address(boot_info.start_address());
        let multiboot_end = Frame::containing_address(boot_info.end_address() - 1);
        for frame in Frame::range_inclusive(multiboot_start, multiboot_end) {
            mapper.identity_map(frame, EntryFlags::PRESENT | EntryFlags::WRITABLE, allocator);
        }
    });

    let old_table = active_table.switch(new_table);
    info!("kremap: successful table switch");

    // Create a guard page in place of the old P4 table's page
    let old_p4_page = Page::containing_address(old_table.p4_frame.start_address());
    active_table.unmap(old_p4_page, allocator);
    info!(
        "kremap: guard page established at {:#x}",
        old_p4_page.start_address()
    );

    active_table
}
