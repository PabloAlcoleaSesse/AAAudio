//! Hardware Flush-To-Zero (FTZ) and Denormals-Are-Zero (DAZ) protection.
//!
//! Subnormal floating-point numbers (`< 1.175e-38` for `f32`) trigger microcode
//! assists on x86/x86_64 and some ARM cores, inflating IIR filter execution times
//! by up to 100x during silence decay. `ScopedNoDenormals` enables FTZ/DAZ upon
//! entering the real-time audio callback and restores the previous register state on drop.

/// RAII guard that enables Flush-To-Zero (FTZ) and Denormals-Are-Zero (DAZ)
/// on the current thread for the duration of a real-time audio quantum.
pub struct ScopedNoDenormals {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    prev_mxcsr: u32,
    #[cfg(target_arch = "aarch64")]
    prev_fpcr: u64,
}

impl Default for ScopedNoDenormals {
    #[inline(always)]
    fn default() -> Self {
        Self::new()
    }
}

impl ScopedNoDenormals {
    #[inline(always)]
    pub fn new() -> Self {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        unsafe {
            #[cfg(target_arch = "x86")]
            use core::arch::x86::{_mm_getcsr, _mm_setcsr};
            #[cfg(target_arch = "x86_64")]
            use core::arch::x86_64::{_mm_getcsr, _mm_setcsr};

            let prev_mxcsr = _mm_getcsr();
            // Bit 15 = FTZ (0x8000), Bit 6 = DAZ (0x0040)
            _mm_setcsr(prev_mxcsr | 0x8040);
            Self { prev_mxcsr }
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            let prev_fpcr: u64;
            core::arch::asm!("mrs {}, fpcr", out(reg) prev_fpcr, options(nomem, nostack));
            // Bit 24 = FZ (Flush-to-zero)
            let new_fpcr = prev_fpcr | (1 << 24);
            core::arch::asm!("msr fpcr, {}", in(reg) new_fpcr, options(nomem, nostack));
            Self { prev_fpcr }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self {}
        }
    }
}

impl Drop for ScopedNoDenormals {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        unsafe {
            #[cfg(target_arch = "x86")]
            use core::arch::x86::_mm_setcsr;
            #[cfg(target_arch = "x86_64")]
            use core::arch::x86_64::_mm_setcsr;

            _mm_setcsr(self.prev_mxcsr);
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("msr fpcr, {}", in(reg) self.prev_fpcr, options(nomem, nostack));
        }
    }
}
