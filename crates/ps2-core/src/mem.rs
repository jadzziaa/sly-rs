//! The EE's address space without a TLB: the fixed map the PS2 kernel sets
//! up, with plain memory served directly and hardware registers routed to an
//! [`Mmio`] handler.
//!
//! | Range | What |
//! | --- | --- |
//! | `0x0000_0000..0x0200_0000` | main RAM (32 MiB); mirrored at `0x2…` (uncached) and `0x3…` (uncached accelerated) |
//! | `0x7000_0000..0x7000_4000` | scratchpad (16 KiB) |
//! | `0x1000_0000..0x1001_0000` | EE hardware registers (MMIO) |
//! | `0x1100_0000..0x1101_0000` | VU0 micro/data, VU1 micro/data memory |
//! | `0x1200_0000..0x1200_2000` | GS privileged registers (MMIO) |
//! | `0x1C00_0000..0x1C20_0000` | IOP RAM (2 MiB) |
//!
//! The kernel segments `0x8000_0000` and `0xA000_0000` map to the physical
//! addresses below `0x2000_0000`. Anything else goes to the [`Mmio`]
//! handler too, which logs it.

/// Main RAM size.
pub const RAM_SIZE: usize = 32 << 20;
/// Scratchpad size.
pub const SCRATCHPAD_SIZE: usize = 16 << 10;
/// IOP RAM size.
pub const IOP_RAM_SIZE: usize = 2 << 20;
/// Scratchpad base address.
pub const SCRATCHPAD: u32 = 0x7000_0000;

/// The memories the EE reaches directly.
pub struct Regions {
    /// Main RAM.
    pub ram: Box<[u8]>,
    /// Scratchpad RAM.
    pub scratchpad: Box<[u8]>,
    /// VU0 micro memory (4 KiB).
    pub vu0_code: Box<[u8]>,
    /// VU0 data memory (4 KiB).
    pub vu0_data: Box<[u8]>,
    /// VU1 micro memory (16 KiB).
    pub vu1_code: Box<[u8]>,
    /// VU1 data memory (16 KiB).
    pub vu1_data: Box<[u8]>,
    /// IOP RAM.
    pub iop_ram: Box<[u8]>,
}

impl Default for Regions {
    fn default() -> Self {
        Self {
            ram: vec![0; RAM_SIZE].into_boxed_slice(),
            scratchpad: vec![0; SCRATCHPAD_SIZE].into_boxed_slice(),
            vu0_code: vec![0; 4 << 10].into_boxed_slice(),
            vu0_data: vec![0; 4 << 10].into_boxed_slice(),
            vu1_code: vec![0; 16 << 10].into_boxed_slice(),
            vu1_data: vec![0; 16 << 10].into_boxed_slice(),
            iop_ram: vec![0; IOP_RAM_SIZE].into_boxed_slice(),
        }
    }
}

impl Regions {
    /// The bytes behind `[address, address + len)` when they are plain
    /// memory in one region.
    #[inline]
    pub fn slice_mut(&mut self, address: u32, len: u32) -> Option<&mut [u8]> {
        let (region, offset): (&mut [u8], u32) = match address {
            // User segment: RAM and its uncached mirrors.
            0x0000_0000..0x1000_0000 | 0x2000_0000..0x4000_0000 => {
                (&mut self.ram, address & 0x01FF_FFFF)
            }
            0x7000_0000..0x7000_4000 => (&mut self.scratchpad, address - SCRATCHPAD),
            // Kernel segments: physical below 0x2000_0000.
            0x8000_0000..0xC000_0000 => return self.physical(address & 0x1FFF_FFFF, len),
            _ => return self.physical(address, len),
        };
        let start = offset as usize;
        region.get_mut(start..start + len as usize)
    }

    fn physical(&mut self, address: u32, len: u32) -> Option<&mut [u8]> {
        let (region, offset): (&mut [u8], u32) = match address {
            0x0000_0000..0x1000_0000 => (&mut self.ram, address & 0x01FF_FFFF),
            0x1100_0000..0x1100_4000 => (&mut self.vu0_code, address & 0xFFF),
            0x1100_4000..0x1100_8000 => (&mut self.vu0_data, address & 0xFFF),
            0x1100_8000..0x1100_C000 => (&mut self.vu1_code, address & 0x3FFF),
            0x1100_C000..0x1101_0000 => (&mut self.vu1_data, address & 0x3FFF),
            0x1C00_0000..0x1E00_0000 => (&mut self.iop_ram, address & 0x001F_FFFF),
            _ => return None,
        };
        let start = offset as usize;
        region.get_mut(start..start + len as usize)
    }
}

/// Hardware registers and everything else that is not plain memory. Values
/// are passed as the low `bytes` bytes of a `u128`.
pub trait Mmio {
    /// A read of `bytes` (1, 2, 4, 8 or 16) at `address`.
    fn read(&mut self, regions: &mut Regions, address: u32, bytes: u32) -> u128;
    /// A write of `bytes` at `address`. True if an interrupt may be taken
    /// after it (an enabled cause is pending) or the machine's next event
    /// changed (a DMA transfer started): recompiled code then hands control
    /// back to the machine at the next instruction boundary.
    fn write(&mut self, regions: &mut Regions, address: u32, bytes: u32, value: u128) -> bool;
    /// The clock at the instruction about to access a register. The
    /// interpreter keeps the machine's clock current itself; recompiled code
    /// counts a block's instructions in advance and says here what the clock
    /// would read.
    fn set_clock(&mut self, _cycles: u64) {}
}

/// An [`Mmio`] handler that reads zeros and ignores writes.
pub struct NoMmio;

impl Mmio for NoMmio {
    fn read(&mut self, _: &mut Regions, _: u32, _: u32) -> u128 {
        0
    }

    fn write(&mut self, _: &mut Regions, _: u32, _: u32, _: u128) -> bool {
        false
    }
}

/// The EE's view of memory.
pub struct Memory {
    /// Plain memory.
    pub regions: Regions,
    /// Hardware registers.
    pub mmio: Box<dyn Mmio>,
    /// Set by a write after which the [`Mmio`] handler says an interrupt
    /// may be taken; recompiled code clears it and exits.
    pub attention: bool,
}

macro_rules! access {
    ($read:ident, $write:ident, $ty:ty) => {
        #[doc = concat!("Reads a `", stringify!($ty), "` (little-endian).")]
        #[inline]
        pub fn $read(&mut self, address: u32) -> $ty {
            const N: usize = size_of::<$ty>();
            match self.regions.slice_mut(address, N as u32) {
                Some(bytes) => {
                    let mut raw = [0; N];
                    raw.copy_from_slice(bytes);
                    <$ty>::from_le_bytes(raw)
                }
                None => self.mmio.read(&mut self.regions, address, N as u32) as $ty,
            }
        }

        #[doc = concat!("Writes a `", stringify!($ty), "` (little-endian).")]
        #[inline]
        pub fn $write(&mut self, address: u32, value: $ty) {
            const N: usize = size_of::<$ty>();
            match self.regions.slice_mut(address, N as u32) {
                Some(bytes) => bytes.copy_from_slice(&value.to_le_bytes()),
                None => {
                    self.attention |=
                        self.mmio
                            .write(&mut self.regions, address, N as u32, u128::from(value));
                }
            }
        }
    };
}

impl Memory {
    /// Memory with the given hardware handler.
    #[must_use]
    pub fn new(mmio: Box<dyn Mmio>) -> Self {
        Self {
            regions: Regions::default(),
            mmio,
            attention: false,
        }
    }

    access!(read8, write8, u8);
    access!(read16, write16, u16);
    access!(read32, write32, u32);
    access!(read64, write64, u64);
    access!(read128, write128, u128);

    /// Copies `data` into memory at `address` (plain memory only).
    ///
    /// # Errors
    ///
    /// The address if the range is not plain memory.
    pub fn load(&mut self, address: u32, data: &[u8]) -> Result<(), u32> {
        let len = u32::try_from(data.len()).map_err(|_| address)?;
        self.regions
            .slice_mut(address, len)
            .ok_or(address)?
            .copy_from_slice(data);
        Ok(())
    }
}
