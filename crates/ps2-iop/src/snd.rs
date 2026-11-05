//! The Rust 989snd ([`ps2_snd989`]) on the IOP: it answers the sound
//! driver's RPC servers in place of the disc's module, and its 240 Hz
//! timer is one of the IOP's events, so its register writes reach the
//! SPU2 at the IOP cycle they are made, as the module's would.

use ps2_core::iop::IopMmio;
use ps2_snd989::{Bus, Driver, TICK_CYCLES};

use crate::cdvd::Sectors;
use crate::core::IopCore;
use crate::kernel::Heap;
use crate::spu2::IopHardware;

/// What the driver reaches on the IOP: the hardware registers, sound
/// memory, the disc and the IOP's heap.
struct SoundBus<'a> {
    hw: &'a mut IopHardware,
    disc: Option<&'a std::rc::Rc<dyn Sectors>>,
    heap: &'a mut Heap,
}

impl Bus for SoundBus<'_> {
    fn read(&mut self, address: u32, bytes: u32) -> u32 {
        self.hw.read(address, bytes)
    }

    fn write(&mut self, address: u32, bytes: u32, value: u32) {
        self.hw.write(address, bytes, value);
    }

    fn upload(&mut self, address: u32, data: &[u8]) {
        let now = self.hw.now;
        self.hw.catch_up(now);
        let ram = self.hw.spu.ram_mut();
        let at = (address as usize).min(ram.len());
        let n = data.len().min(ram.len() - at);
        ram[at..at + n].copy_from_slice(&data[..n]);
    }

    fn sectors(&mut self, lba: u32, count: u32) -> Option<Vec<u8>> {
        self.disc?.read(lba, count)
    }

    fn alloc_iop(&mut self, bytes: u32) -> Option<u32> {
        self.heap.alloc(1, bytes, 0)
    }
}

impl IopCore {
    /// Plays sound with the Rust driver from now on, its random numbers
    /// seeded from the module image (the disc's `989SND.IRX`), its reverb
    /// presets from libsd's.
    pub(crate) fn start_sound(&mut self, module: Option<&[u8]>, libsd: Option<&[u8]>) {
        if self.sound.is_none() {
            self.sound = Some(Box::new(Driver::new(module, libsd)));
            self.sound_due = self.cpu.cycles + TICK_CYCLES;
        }
    }

    /// When the driver's next tick is due, if it plays.
    pub(crate) fn sound_alarm(&self) -> Option<u64> {
        self.sound.as_ref().map(|_| self.sound_due)
    }

    /// Runs the driver's ticks due by now, each at its own cycle.
    pub(crate) fn run_sound(&mut self) {
        let IopCore {
            sound,
            sound_due,
            hw,
            kernel,
            disc,
            cpu,
            ..
        } = self;
        let Some(driver) = sound.as_mut() else {
            return;
        };
        while *sound_due <= cpu.cycles {
            hw.now = *sound_due;
            let mut bus = SoundBus {
                hw,
                disc: disc.as_ref(),
                heap: &mut kernel.heap,
            };
            driver.tick(&mut bus);
            *sound_due += TICK_CYCLES;
        }
        hw.now = cpu.cycles;
    }

    /// A DMA channel feeding the SPU2 finished (interrupt `line`): the
    /// driver may start the next transfer at once.
    pub(crate) fn sound_dma_done(&mut self, line: u32) {
        let IopCore {
            sound,
            hw,
            kernel,
            disc,
            ..
        } = self;
        let Some(driver) = sound.as_mut() else {
            return;
        };
        let channel = if line == 0x28 { 7 } else { 4 };
        let mut bus = SoundBus {
            hw,
            disc: disc.as_ref(),
            heap: &mut kernel.heap,
        };
        driver.dma_done(channel, &mut bus);
    }

    /// Answers a call to the driver's servers at IOP cycle `now`.
    pub(crate) fn sound_call(
        &mut self,
        sid: u32,
        function: u32,
        data: &[u8],
        reply_size: u32,
    ) -> Vec<u8> {
        if self.sound.is_none() {
            self.start_sound(None, None);
        }
        let IopCore {
            sound,
            hw,
            kernel,
            disc,
            cpu,
            ..
        } = self;
        let Some(driver) = sound.as_mut() else {
            return vec![0; reply_size as usize];
        };
        hw.now = cpu.cycles;
        let mut bus = SoundBus {
            hw,
            disc: disc.as_ref(),
            heap: &mut kernel.heap,
        };
        driver.call(sid, function, data, reply_size, &mut bus)
    }
}
