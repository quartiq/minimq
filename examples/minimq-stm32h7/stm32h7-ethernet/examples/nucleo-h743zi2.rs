#![no_main]
#![no_std]

extern crate cortex_m_rt as rt;
use core::sync::atomic::{AtomicU32, Ordering};
use rt::{entry, exception};

#[macro_use]
extern crate log;

extern crate cortex_m;

#[cfg(feature = "itm")]
extern crate panic_itm;
extern crate stm32h7_ethernet as ethernet;

use stm32h7xx_hal::gpio::Speed;
use stm32h7xx_hal::hal::digital::v2::OutputPin;
use stm32h7xx_hal::rcc::CoreClocks;
use stm32h7xx_hal::{prelude::*, stm32, stm32::interrupt};
use Speed::*;

#[cfg(feature = "itm")]
use cortex_m_log::log::{trick_init, Logger};

#[cfg(feature = "itm")]
use cortex_m_log::{
    destination::Itm, printer::itm::InterruptSync as InterruptSyncItm,
};

#[cfg(not(feature = "itm"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    use stm32h7xx_hal::stm32::GPIOB;

    // Turn on LED PB14
    let bit_index = 14_u32;
    unsafe {
        // Into push pull output
        let offset: u32 = 2 * bit_index;
        &(*GPIOB::ptr()).pupdr.modify(|r, w| {
            w.bits((r.bits() & !(0b11_u32 << offset)) | (0b00 << offset))
        });
        &(*GPIOB::ptr())
            .otyper
            .modify(|r, w| w.bits(r.bits() & !(0b1_u32 << bit_index)));
        &(*GPIOB::ptr()).moder.modify(|r, w| {
            w.bits((r.bits() & !(0b11_u32 << offset)) | (0b01_u32 << offset))
        });

        // Set high
        (*GPIOB::ptr()).bsrr.write(|w| w.bits(1_u32 << bit_index));
    }
    loop {}
}

/// Configure SYSTICK for 1ms timebase
fn systick_init(syst: &mut stm32::SYST, clocks: CoreClocks) {
    let c_ck_mhz = clocks.c_ck().0 / 1_000_000;

    let syst_calib = 0x3E8;

    syst.set_clock_source(cortex_m::peripheral::syst::SystClkSource::Core);
    syst.set_reload((syst_calib * c_ck_mhz) - 1);
    syst.enable_interrupt();
    syst.enable_counter();
}

/// ======================================================================
/// Entry point
/// ======================================================================

/// TIME is an atomic u32 that counts milliseconds. Although not used
/// here, it is very useful to have for network protocols
static TIME: AtomicU32 = AtomicU32::new(0);

/// Locally administered MAC address
const MAC_ADDRESS: [u8; 6] = [0x02, 0x00, 0x11, 0x22, 0x33, 0x44];

/// Ethernet descriptor rings are a global singleton
#[link_section = ".sram3.eth"]
static mut DES_RING: ethernet::DesRing = ethernet::DesRing::new();

// the program entry point
#[entry]
fn main() -> ! {
    let dp = stm32::Peripherals::take().unwrap();
    let mut cp = stm32::CorePeripherals::take().unwrap();

    // Initialise logging...
    #[cfg(feature = "itm")]
    let logger = Logger {
        inner: InterruptSyncItm::new(Itm::new(cp.ITM)),
        level: log::LevelFilter::Trace,
    };
    #[cfg(feature = "itm")]
    unsafe {
        let _ = trick_init(&logger);
    }

    // Initialise power...
    let pwr = dp.PWR.constrain();
    let vos = pwr.freeze();

    // Initialise SRAM3
    dp.RCC.ahb2enr.modify(|_, w| w.sram3en().set_bit());

    // Initialise clocks...
    let rcc = dp.RCC.constrain();
    let mut ccdr = rcc
        .sys_ck(200.mhz())
        .hclk(200.mhz())
        .pll1_r_ck(100.mhz()) // for TRACECK
        .freeze(vos, &dp.SYSCFG);

    // Get the delay provider.
    let delay = cp.SYST.delay(ccdr.clocks);

    // Initialise system...
    cp.SCB.invalidate_icache();
    cp.SCB.enable_icache();
    // TODO: ETH DMA coherence issues
    // cp.SCB.enable_dcache(&mut cp.CPUID);
    cp.DWT.enable_cycle_counter();

    // Initialise IO...
    let gpioa = dp.GPIOA.split(&mut ccdr.ahb4);
    let gpiob = dp.GPIOB.split(&mut ccdr.ahb4);
    let gpioc = dp.GPIOC.split(&mut ccdr.ahb4);
    let gpiog = dp.GPIOG.split(&mut ccdr.ahb4);
    let mut link_led = gpiob.pb0.into_push_pull_output(); // LED1, green
    link_led.set_high().ok();

    let _rmii_ref_clk = gpioa.pa1.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_mdio = gpioa.pa2.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_mdc = gpioc.pc1.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_crs_dv = gpioa.pa7.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_rxd0 = gpioc.pc4.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_rxd1 = gpioc.pc5.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_tx_en = gpiog.pg11.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_txd0 = gpiog.pg13.into_alternate_af11().set_speed(VeryHigh);
    let _rmii_txd1 = gpiob.pb13.into_alternate_af11().set_speed(VeryHigh);

    // Initialise ethernet...
    assert_eq!(ccdr.clocks.hclk().0, 200_000_000); // HCLK 200MHz
    assert_eq!(ccdr.clocks.pclk1().0, 100_000_000); // PCLK 100MHz
    assert_eq!(ccdr.clocks.pclk2().0, 100_000_000); // PCLK 100MHz
    assert_eq!(ccdr.clocks.pclk4().0, 100_000_000); // PCLK 100MHz

    let mac_addr = smoltcp::wire::EthernetAddress::from_bytes(&MAC_ADDRESS);
    let (_eth_dma, mut eth_mac) = unsafe {
        ethernet::ethernet_init(
            dp.ETHERNET_MAC,
            dp.ETHERNET_MTL,
            dp.ETHERNET_DMA,
            &mut DES_RING,
            mac_addr.clone(),
        )
    };
    unsafe {
        ethernet::enable_interrupt();
        cp.NVIC.set_priority(stm32::Interrupt::ETH, 196); // Mid prio
        cortex_m::peripheral::NVIC::unmask(stm32::Interrupt::ETH);
    }

    // ----------------------------------------------------------
    // Begin periodic tasks

    systick_init(&mut delay.free(), ccdr.clocks);
    unsafe {
        cp.SCB.shpr[15 - 4].write(128);
    } // systick exception priority

    // ----------------------------------------------------------
    // Main application loop

    let mut eth_up = false;
    loop {
        let _time = TIME.load(Ordering::Relaxed);

        // Ethernet
        let eth_last = eth_up;
        eth_up = eth_mac.phy_poll_link();
        match eth_up {
            true => link_led.set_low(),
            _ => link_led.set_high(),
        }
        .ok();

        if eth_up != eth_last {
            // Interface state change
            match eth_up {
                true => info!("Ethernet UP"),
                _ => info!("Ethernet DOWN"),
            }
        }
    }
}

#[interrupt]
fn ETH() {
    unsafe { ethernet::interrupt_handler() }
}

#[exception]
fn SysTick() {
    TIME.fetch_add(1, Ordering::Relaxed);
}

#[exception]
fn HardFault(ef: &cortex_m_rt::ExceptionFrame) -> ! {
    panic!("HardFault at {:#?}", ef);
}

#[exception]
fn DefaultHandler(irqn: i16) {
    panic!("Unhandled exception (IRQn = {})", irqn);
}
