//! EE kernel call wrappers. Sony's `libkernel` wraps each system call in a
//! four-instruction stub:
//!
//! ```text
//! addiu $v1, $zero, N      (negative N: the interrupt-context variant)
//! syscall
//! jr    $ra
//! nop
//! ```
//!
//! The names are the kernel's, as the homebrew SDK (ps2sdk,
//! `ee/kernel/include/syscallnr.h`) lists them.

use ps2_isa::ee::{Insn, Op};

use crate::{Discovery, Image};

/// A kernel call wrapper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyscallStub {
    /// Address of the stub.
    pub address: u32,
    /// The syscall number loaded into `$v1` (negative for `i` variants).
    pub number: i16,
    /// The kernel's name for it, if known.
    pub name: Option<&'static str>,
}

/// The stubs among `discovery`'s functions.
#[must_use]
pub fn syscall_stubs(image: &Image, discovery: &Discovery) -> Vec<SyscallStub> {
    discovery
        .functions
        .iter()
        .filter_map(|function| {
            let insn = |i: u32| Insn::decode(image.word(function.start + 4 * i).unwrap_or(0));
            let li = insn(0);
            let is_stub = li.op() == Op::Addiu
                && li.rt() == 3
                && li.rs() == 0
                && insn(1).op() == Op::Syscall
                && insn(2).op() == Op::Jr
                && insn(2).rs() == 31;
            is_stub.then(|| SyscallStub {
                address: function.start,
                number: li.simm(),
                name: syscall_name(li.simm()),
            })
        })
        .collect()
}

/// The kernel's name for syscall `number` (negative for `i` variants).
#[must_use]
#[allow(clippy::too_many_lines, reason = "a table")]
pub fn syscall_name(number: i16) -> Option<&'static str> {
    Some(match number {
        0x01 => "ResetEE",
        0x02 => "SetGsCrt",
        0x04 => "Exit",
        0x05 => "ResumeIntrDispatch",
        0x06 => "LoadExecPS2",
        0x07 => "ExecPS2",
        0x08 => "ResumeT3IntrDispatch",
        0x0A => "AddSbusIntcHandler",
        0x0B => "RemoveSbusIntcHandler",
        0x0C => "Interrupt2Iop",
        0x0D => "SetVTLBRefillHandler",
        0x0E => "SetVCommonHandler",
        0x0F => "SetVInterruptHandler",
        0x10 => "AddIntcHandler",
        0x11 => "RemoveIntcHandler",
        0x12 => "AddDmacHandler",
        0x13 => "RemoveDmacHandler",
        0x14 => "_EnableIntc",
        0x15 => "_DisableIntc",
        0x16 => "_EnableDmac",
        0x17 => "_DisableDmac",
        0x18 => "_SetAlarm",
        0x19 => "_ReleaseAlarm",
        -0x1A => "_iEnableIntc",
        -0x1B => "_iDisableIntc",
        -0x1C => "_iEnableDmac",
        -0x1D => "_iDisableDmac",
        -0x1E => "_iSetAlarm",
        -0x1F => "_iReleaseAlarm",
        0x20 => "CreateThread",
        0x21 => "DeleteThread",
        0x22 => "StartThread",
        0x23 => "ExitThread",
        0x24 => "ExitDeleteThread",
        0x25 => "TerminateThread",
        -0x26 => "iTerminateThread",
        0x27 => "DisableDispatchThread",
        0x28 => "EnableDispatchThread",
        0x29 => "ChangeThreadPriority",
        -0x2A => "iChangeThreadPriority",
        0x2B => "RotateThreadReadyQueue",
        -0x2C => "iRotateThreadReadyQueue",
        0x2D => "ReleaseWaitThread",
        -0x2E => "iReleaseWaitThread",
        0x2F => "GetThreadId",
        -0x2F => "iGetThreadId",
        0x30 => "ReferThreadStatus",
        -0x31 => "iReferThreadStatus",
        0x32 => "SleepThread",
        0x33 => "WakeupThread",
        -0x34 => "iWakeupThread",
        0x35 => "CancelWakeupThread",
        -0x36 => "iCancelWakeupThread",
        0x37 => "SuspendThread",
        -0x38 => "iSuspendThread",
        0x39 => "ResumeThread",
        -0x3A => "iResumeThread",
        0x3C => "SetupThread",
        0x3D => "SetupHeap",
        0x3E => "EndOfHeap",
        0x40 => "CreateSema",
        0x41 => "DeleteSema",
        0x42 => "SignalSema",
        -0x43 => "iSignalSema",
        0x44 => "WaitSema",
        0x45 => "PollSema",
        -0x46 => "iPollSema",
        0x47 => "ReferSemaStatus",
        -0x48 => "iReferSemaStatus",
        -0x49 => "iDeleteSema",
        0x4A => "SetOsdConfigParam",
        0x4B => "GetOsdConfigParam",
        0x4C => "GetGsHParam",
        0x4D => "GetGsVParam",
        0x4E => "SetGsHParam",
        0x4F => "SetGsVParam",
        0x50 => "CreateEventFlag",
        0x51 => "DeleteEventFlag",
        0x52 => "SetEventFlag",
        0x53 => "iSetEventFlag",
        0x54 => "xlaunch",
        0x55 => "PutTLBEntry",
        -0x55 => "iPutTLBEntry",
        0x56 => "_SetTLBEntry",
        -0x56 => "iSetTLBEntry",
        0x57 => "GetTLBEntry",
        -0x57 => "iGetTLBEntry",
        0x58 => "ProbeTLBEntry",
        -0x58 => "iProbeTLBEntry",
        0x59 => "ExpandScratchPad",
        0x5A => "Copy",
        0x5B => "GetEntryAddress",
        0x5C => "EnableIntcHandler",
        -0x5C => "iEnableIntcHandler",
        0x5D => "DisableIntcHandler",
        -0x5D => "iDisableIntcHandler",
        0x5E => "EnableDmacHandler",
        -0x5E => "iEnableDmacHandler",
        0x5F => "DisableDmacHandler",
        -0x5F => "iDisableDmacHandler",
        0x60 => "KSeg0",
        0x61 => "EnableCache",
        0x62 => "DisableCache",
        0x63 => "GetCop0",
        0x64 => "FlushCache",
        0x66 => "CpuConfig",
        -0x67 => "iGetCop0",
        -0x68 => "iFlushCache",
        -0x6A => "iCpuConfig",
        0x6B => "sceSifStopDma",
        0x6C => "SetCPUTimerHandler",
        0x6D => "SetCPUTimer",
        0x6E => "SetOsdConfigParam2",
        0x6F => "GetOsdConfigParam2",
        0x70 => "GsGetIMR",
        -0x70 => "iGsGetIMR",
        0x71 => "GsPutIMR",
        -0x71 => "iGsPutIMR",
        0x72 => "SetPgifHandler",
        0x73 => "SetVSyncFlag",
        0x74 => "SetSyscall",
        0x75 => "_print",
        0x76 => "sceSifDmaStat",
        -0x76 => "isceSifDmaStat",
        0x77 => "sceSifSetDma",
        -0x77 => "isceSifSetDma",
        0x78 => "sceSifSetDChain",
        -0x78 => "isceSifSetDChain",
        0x79 => "sceSifSetReg",
        0x7A => "sceSifGetReg",
        0x7B => "ExecOSD",
        0x7C => "Deci2Call",
        0x7D => "PSMode",
        0x7E => "MachineType",
        0x7F => "GetMemorySize",
        0x80 => "_GetGsDxDyOffset",
        0x82 => "_InitTLB",
        0x83 => "FindAddress",
        0x85 => "SetMemoryMode",
        0x86 => "GetMemoryMode",
        0x87 => "ExecPSX",
        0xFC => "SetAlarm",
        0xFE => "ReleaseAlarm",
        -0xFD => "iSetAlarm",
        -0xFF => "iReleaseAlarm",
        _ => return None,
    })
}
