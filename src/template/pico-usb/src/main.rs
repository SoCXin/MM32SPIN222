#![no_std]
#![no_main]

use cortex_m::asm::nop;
use cortex_m_rt::entry;
use panic_halt as _;

use rtt_target::{rprintln, rtt_init_print};

use rp2040_pac::{Peripherals, XOSC};

mod usb;

#[link_section = ".boot_loader"]
#[used]
pub static BOOT_LOADER: [u8; 256] = rp2040_boot2::BOOT_LOADER;

/// Handle peripheral resets so the chip is usable.
unsafe fn setup_chip(p: &mut rp2040_pac::Peripherals) {
    // Now reset all the peripherals, except QSPI and XIP (we're using those
    // to execute from external flash!)
    p.RESETS.reset.write(|w| {
        w.adc().set_bit();
        w.busctrl().set_bit();
        w.dma().set_bit();
        w.i2c0().set_bit();
        w.i2c1().set_bit();
        w.io_bank0().set_bit();
        w.io_qspi().clear_bit();
        w.jtag().set_bit();
        w.pads_bank0().set_bit();
        w.pads_qspi().clear_bit();
        w.pio0().set_bit();
        w.pio1().set_bit();
        w.pll_sys().clear_bit();
        w.pll_usb().clear_bit();
        w.pwm().set_bit();
        w.rtc().set_bit();
        w.spi0().set_bit();
        w.spi1().set_bit();
        w.syscfg().set_bit();
        w.sysinfo().set_bit();
        w.tbman().set_bit();
        w.timer().set_bit();
        w.uart0().set_bit();
        w.uart1().set_bit();
        w.usbctrl().set_bit();
        w
    });

    const RESETS_RESET_BITS: u32 = 0x01ffffff;
    const RESETS_RESET_USBCTRL_BITS: u32 = 0x01000000;
    const RESETS_RESET_UART1_BITS: u32 = 0x00800000;
    const RESETS_RESET_UART0_BITS: u32 = 0x00400000;
    const RESETS_RESET_SPI1_BITS: u32 = 0x00020000;
    const RESETS_RESET_SPI0_BITS: u32 = 0x00010000;
    const RESETS_RESET_RTC_BITS: u32 = 0x00008000;
    const RESETS_RESET_ADC_BITS: u32 = 0x00000001;

    // We want to take everything out of reset, except these peripherals:
    //
    // * ADC
    // * RTC
    // * SPI0
    // * SPI1
    // * UART0
    // * UART1
    // * USBCTRL
    //
    // These must stay in reset until the clocks are sorted out.
    const PERIPHERALS_TO_UNRESET: u32 = RESETS_RESET_BITS
        & !(RESETS_RESET_ADC_BITS
            | RESETS_RESET_RTC_BITS
            | RESETS_RESET_SPI0_BITS
            | RESETS_RESET_SPI1_BITS
            | RESETS_RESET_UART0_BITS
            | RESETS_RESET_UART1_BITS
            | RESETS_RESET_USBCTRL_BITS);

    // Write 0 to the reset field to take it out of reset
    // TODO: Figure out which should be taken out of reset here
    p.RESETS.reset.modify(|_r, w| {
        w.busctrl().clear_bit();
        w.dma().clear_bit();
        w.i2c0().clear_bit();
        w.i2c1().clear_bit();
        w.io_bank0().clear_bit();
        w.io_qspi().clear_bit();
        w.jtag().clear_bit();
        w.pads_bank0().clear_bit();
        w.pads_qspi().clear_bit();
        w.pio0().clear_bit();
        w.pio1().clear_bit();
        w.pll_sys().clear_bit();
        w.pll_usb().clear_bit();
        w.pwm().clear_bit();
        w.syscfg().clear_bit();
        w.sysinfo().clear_bit();
        w.tbman().clear_bit();
        w.timer().clear_bit();
        w
    });

    while ((!p.RESETS.reset_done.read().bits()) & PERIPHERALS_TO_UNRESET) != 0 {
        cortex_m::asm::nop();
    }
}

const XOSC_MHZ: u16 = 12;

const MHZ: u32 = 1_000_000;

fn enable_xosc(osc: &mut XOSC, freq_mhz: u16) {
    // Clear BADWRITE bit in status register
    osc.status.write(|w| w.badwrite().set_bit());

    // Enable external oscillator XOSC
    osc.ctrl
        .modify(|_r, w| w.freq_range()._1_15mhz().enable().enable());

    // Calculate startup delay according to section 2.16.3 of the datasheet
    //
    // Round up in case there is no exact value found.
    let startup_delay = osc_startup_delay(freq_mhz as u32);

    // Configure startup delay
    unsafe {
        osc.startup.write(|w| w.delay().bits(startup_delay as u16));
    }

    // Wait until clock is started

    loop {
        if osc.status.read().stable().bit_is_set() {
            break;
        }
    }

    rprintln!("XOSC Status: {:#x}", osc.status.read().bits());
}

const fn osc_startup_delay(freq_mhz: u32) -> u32 {
    (((freq_mhz as u32 * MHZ) / 1000) + 128) / 256
}

/// Port of the clocks_init function from the Pico SDK
unsafe fn clocks_init(p: &mut Peripherals) {
    // Enable tick generation in Watchdog
    //
    // This is necessary to use the timer
    p.WATCHDOG
        .tick
        .write(|w| w.cycles().bits(XOSC_MHZ).enable().set_bit());

    // Disable resus, if it's active for some reason
    p.CLOCKS
        .clk_sys_resus_ctrl
        .modify(|_r, w| w.enable().clear_bit());

    // Enable external oscillator XOSC
    enable_xosc(&mut p.XOSC, XOSC_MHZ);

    // `clk_sys` and `clk_ref` must be switched from the auxiliary multiplexer (aux mux),
    // to the glitchless mux, before changing them. (See section 2.15.3.2 in the datasheet)

    // TODO: Use bitbanded register to do this atomically

    // Use reference clock, not the aux mux.
    p.CLOCKS.clk_sys_ctrl.modify(|_r, w| w.src().clk_ref());

    // Wait until clock source is changed
    while p.CLOCKS.clk_sys_selected.read().bits() != 1 {
        nop()
    }

    // TODO: Use bitbanded register to do this atomically

    // Use ring oscilator (ROSC) as the clock source, not XOSC or aux
    p.CLOCKS
        .clk_ref_ctrl
        .modify(|_r, w| w.src().rosc_clksrc_ph());

    // Wait until clock source is changed
    while p.CLOCKS.clk_ref_selected.read().bits() != 1 {
        nop()
    }

    // Setup PLLs

    p.RESETS
        .reset
        .modify(|_r, w| w.pll_sys().set_bit().pll_usb().set_bit());

    p.RESETS
        .reset
        .modify(|_r, w| w.pll_sys().clear_bit().pll_usb().clear_bit());

    loop {
        let reset_done = p.RESETS.reset_done.read();

        if reset_done.pll_sys().bit_is_set() && reset_done.pll_usb().bit_is_set() {
            break;
        }
    }

    //                   REF     FBDIV VCO            POSTDIV
    // PLL SYS: 12 / 1 = 12MHz * 125 = 1500MHZ / 6 / 2 = 125MHz
    // PLL USB: 12 / 1 = 12MHz * 40  = 480 MHz / 5 / 2 =  48MHz
    pll_init(&p.PLL_SYS, XOSC_MHZ, 1, 1500 * MHZ, 6, 2);

    pll_init(&p.PLL_USB, XOSC_MHZ, 1, 480 * MHZ, 5, 2);

    // configure reference clock
    //
    // src: 12 MHz (XOSC)
    // dst: 12 MHz

    let src_freq = 12 * MHZ;
    let dst_freq = 12 * MHZ;

    let div = (((src_freq << 8) as u64) / dst_freq as u64) as u32;

    rprintln!("clock_ref: {} -> {} (div={})", src_freq, dst_freq, div);

    // Set the divisor first if we increase it, to avoid overspeed.
    if div > p.CLOCKS.clk_ref_div.read().bits() {
        p.CLOCKS.clk_ref_div.write(|w| w.bits(div))
    }

    p.CLOCKS.clk_ref_ctrl.modify(|_r, w| w.src().xosc_clksrc());

    while (p.CLOCKS.clk_ref_selected.read().bits() & (1 << 2)) != (1 << 2) {}

    // Set the dividor again, now it's safe to set
    p.CLOCKS.clk_ref_div.write(|w| w.bits(div));

    // configure system clock
    //
    // -> should run from aux source (PLL)
    //
    // src: 125 MHz (pll)
    // dst: 125 MHz

    let src_freq = 125 * MHZ;
    let dst_freq = 125 * MHZ;

    let div = (((src_freq as u64) << 8) / dst_freq as u64) as u32;

    // Set the divisor first if we increase it, to avoid overspeed.
    if div > p.CLOCKS.clk_sys_div.read().bits() {
        p.CLOCKS.clk_sys_div.write(|w| w.bits(div))
    }

    // We would have to switch away from the aux clock source, but we know that we did that already
    // above.

    // Select PLL in aux mux
    p.CLOCKS
        .clk_sys_ctrl
        .modify(|_r, w| w.auxsrc().clksrc_pll_sys());

    // Select aux mux in glitchless mux
    p.CLOCKS
        .clk_sys_ctrl
        .modify(|_r, w| w.src().clksrc_clk_sys_aux());

    // Wait until aux mux selected
    // Aux src has offset 1 -> bit 1

    while (p.CLOCKS.clk_sys_selected.read().bits() & (1 << 1)) != (1 << 1) {}

    // Set the dividor again, now it's sure to set
    p.CLOCKS.clk_sys_div.write(|w| w.bits(div));

    // configure USB clock
    //
    // -> should run from aux source (PLL USB)
    //
    // src: 48 MHz (pll)
    // dst: 48 MHz

    let src_freq = 48 * MHZ;
    let dst_freq = 48 * MHZ;

    let div = clock_divider(src_freq, dst_freq);

    rprintln!("clock_ref: {} -> {} (div={})", src_freq, dst_freq, div);

    // Set the divisor first if we increase it, to avoid overspeed.
    if div > p.CLOCKS.clk_usb_div.read().bits() {
        p.CLOCKS.clk_usb_div.write(|w| w.bits(div))
    }

    // We would have to switch away from the aux clock source, but we know that we did that already
    // above.

    // disable the clock before switching
    p.CLOCKS.clk_usb_ctrl.modify(|_r, w| w.enable().clear_bit());

    // We have to wait 3 cycles of the target clock
    //
    // TODO: Make this generic
    //
    // For know, we now that the sysclock is 125 MHz, so waiting to clock cycles is enough
    nop();
    nop();

    // Select PLL in aux mux
    p.CLOCKS
        .clk_usb_ctrl
        .modify(|_r, w| w.auxsrc().clksrc_pll_usb());

    // Enable clock again
    p.CLOCKS.clk_usb_ctrl.modify(|_r, w| w.enable().set_bit());

    // Set the dividor again, now it's safe to set
    p.CLOCKS.clk_usb_div.write(|w| w.bits(div));

    // configure ADC clock
    //
    // -> should run from aux source (PLL USB)
    //
    // src: 48 MHz (pll)
    // dst: 48 MHz

    let src_freq = 48 * MHZ;
    let dst_freq = 48 * MHZ;

    let div = clock_divider(src_freq, dst_freq);

    // Set the divisor first if we increase it, to avoid overspeed.
    if div > p.CLOCKS.clk_adc_div.read().bits() {
        p.CLOCKS.clk_adc_div.write(|w| w.bits(div))
    }

    // We would have to switch away from the aux clock source, but we know that we did that already
    // above.

    // disable the clock before switching
    p.CLOCKS.clk_adc_ctrl.modify(|_r, w| w.enable().clear_bit());

    // We have to wait 3 cycles of the target clock
    //
    // TODO: Make this generic
    //
    // For now, we know that the sysclock is 125 MHz, so waiting two clock cycles is enough
    nop();
    nop();

    // Select PLL in aux mux
    p.CLOCKS
        .clk_adc_ctrl
        .modify(|_r, w| w.auxsrc().clksrc_pll_usb());

    // Enable clock again
    p.CLOCKS.clk_adc_ctrl.modify(|_r, w| w.enable().set_bit());

    // Set the dividor again, now it's safe to set
    p.CLOCKS.clk_adc_div.write(|w| w.bits(div));

    // configure RTC clock
    //
    // -> should run from aux source (PLL USB)
    //
    // src: 48 MHz (pll)
    // dst: 46875 Hz

    let src_freq = 48 * MHZ;
    let dst_freq = 46875;

    let div = clock_divider(src_freq, dst_freq);

    // Set the divisor first if we increase it, to avoid overspeed.
    if div > p.CLOCKS.clk_rtc_div.read().bits() {
        p.CLOCKS.clk_rtc_div.write(|w| w.bits(div))
    }

    // We would have to switch away from the aux clock source, but we know that we did that already
    // above.

    // disable the clock before switching
    p.CLOCKS.clk_rtc_ctrl.modify(|_r, w| w.enable().clear_bit());

    // We have to wait 3 cycles of the target clock
    //
    // TODO: Make this generic
    //
    // For now, we now that the sysclock is 125 MHz, so waiting to clock cycles is enough
    nop();
    nop();

    // Select PLL in aux mux
    p.CLOCKS
        .clk_rtc_ctrl
        .modify(|_r, w| w.auxsrc().clksrc_pll_usb());

    // Enable clock again
    p.CLOCKS.clk_rtc_ctrl.modify(|_r, w| w.enable().set_bit());

    // Set the dividor again, now it's safe to set
    p.CLOCKS.clk_rtc_div.write(|w| w.bits(div));

    // configure PERI clock
    //
    // -> should run from sys clk
    //
    // src: 125 MHz (pll)
    // dst: 125 MHz

    // No divisor for peri clk!

    // We would have to switch away from the aux clock source, but we know that we did that already
    // above.

    // disable the clock before switching
    p.CLOCKS
        .clk_peri_ctrl
        .modify(|_r, w| w.enable().clear_bit());

    // We have to wait 3 cycles of the target clock
    //
    // TODO: Make this generic
    //
    // For now, we now that the sysclock is 125 MHz, so waiting to clock cycles is enough
    nop();
    nop();
    nop();
    nop();

    // Select PLL in aux mux
    p.CLOCKS.clk_peri_ctrl.modify(|_r, w| w.auxsrc().clk_sys());

    // Enable clock again
    p.CLOCKS.clk_peri_ctrl.modify(|_r, w| w.enable().set_bit());
}

const fn clock_divider(src_freq: u32, dst_freq: u32) -> u32 {
    (((src_freq as u64) << 8) / dst_freq as u64) as u32
}

type Pll = rp2040_pac::pll_sys::RegisterBlock;

fn pll_init(
    pll: &Pll,
    osc_freq_mhz: u16,
    ref_div: u8,
    vco_freq: u32,
    post_div1: u32,
    post_div2: u8,
) {
    // Turn off PLL, in case it is already running

    unsafe {
        pll.pwr.write(|w| w.bits(0xffffffff));
        pll.fbdiv_int.write(|w| w.bits(0));
    }

    // Ref div divides the reference frequency
    let ref_mhz = osc_freq_mhz as u32 / ref_div as u32;

    unsafe {
        pll.cs.write(|w| w.refdiv().bits(ref_div));
    }

    // Feedback Divide
    //
    let fbdiv = vco_freq / (ref_mhz * MHZ);

    rprintln!("PLL REF_MHZ: {}", ref_mhz);
    rprintln!("PLL rev_div: {}", ref_div);
    rprintln!("PLL fbdiv:   {}", fbdiv);
    rprintln!(
        "PLL Freq:    {}",
        (osc_freq_mhz as u32 / ref_div as u32) * fbdiv / (post_div1 * post_div2 as u32)
    );

    // TODO: additional checks for PLL params
    assert!((16..=320).contains(&fbdiv));

    unsafe { pll.fbdiv_int.write(|w| w.fbdiv_int().bits(fbdiv as u16)) }

    pll.pwr
        .modify(|_r, w| w.pd().clear_bit().vcopd().clear_bit());

    // Wait for PLL to lock
    while pll.cs.read().lock().bit_is_clear() {}

    // Set up post dividers
    unsafe {
        pll.prim.write(|w| {
            w.postdiv1()
                .bits(post_div1 as u8)
                .postdiv2()
                .bits(post_div2)
        });
    }

    // Turn on post divider
    pll.pwr.modify(|_r, w| w.postdivpd().clear_bit());
}

const ALL_PERIPHERALS_UNRESET: u32 = 0x01ffffff;

#[entry]
fn main() -> ! {
    rtt_init_print!(NoBlockSkip, 4096);

    let mut p = rp2040_pac::Peripherals::take().unwrap();

    unsafe {
        setup_chip(&mut p);
    }

    // Setup clocks?
    unsafe {
        clocks_init(&mut p);
    }

    // Enable all peripherals
    unsafe {
        p.RESETS.reset.write_with_zero(|w| w.bits(0));

        while ((!p.RESETS.reset_done.read().bits()) & ALL_PERIPHERALS_UNRESET) != 0 {
            cortex_m::asm::nop();
        }
    }

    rprintln!("- Reset done");
    rprintln!(
        "- PLL SYS: {} kHz",
        frequency_count_khz(&p.CLOCKS, Clock::PllSys, 12 * 1000)
    );
    rprintln!(
        "- PLL USB: {} kHz",
        frequency_count_khz(&p.CLOCKS, Clock::PllUsb, 12 * 1000)
    );

    rprintln!("CLK_USB_DIV:  {:#08x}", p.CLOCKS.clk_usb_div.read().bits());
    rprintln!("CLK_USB_CTRL: {:#08x}", p.CLOCKS.clk_usb_ctrl.read().bits());

    // Prepare LED

    // Code from https://github.com/rp-rs/pico-blink-rs, by @thejpster
    //

    // Set GPIO25 to be an input (output enable is cleared)
    p.SIO.gpio_oe_clr.write(|w| unsafe {
        w.bits(1 << 25);
        w
    });

    // Set GPIO25 to be an output low (output is cleared)
    p.SIO.gpio_out_clr.write(|w| unsafe {
        w.bits(1 << 25);
        w
    });

    // Configure pin 25 for GPIO
    p.PADS_BANK0.gpio25.write(|w| {
        // Output Disable off
        w.od().clear_bit();
        // Input Enable on
        w.ie().set_bit();
        w
    });
    p.IO_BANK0.gpio25_ctrl.write(|w| {
        // Map pin 25 to SIO
        w.funcsel().sio_25();
        w
    });

    // Set GPIO25 to be an output (output enable is set)
    p.SIO.gpio_oe_set.write(|w| unsafe {
        w.bits(1 << 25);
        w
    });

    // -- END -- Code from https://github.com/rp-rs/pico-blink-rs, by @thejpster

    let resets = p.RESETS;
    let usb_ctrl = p.USBCTRL_REGS;

    let mut usb_device = usb::usb_device_init(&resets, usb_ctrl);

    // Wait for USB configuration
    while !usb_device.configured() {
        usb_device.poll();
    }

    /* Enable LED to verify we get here */

    // Set GPIO25 to be high
    p.SIO.gpio_out_set.write(|w| unsafe {
        w.bits(1 << 25);
        w
    });

    // Start to receive data from the Host
    usb_device.start_transfer(usb::EP1_OUT_ADDR, 64, None);

    loop {
        usb_device.poll();
    }
}

/// Clock source for frequency counter
enum Clock {
    PllSys = 1,
    PllUsb = 2,
    Sys = 0x9,
    Peri = 0xa,
    Usb = 0xb,
    Adc = 0xc,
    Rtc = 0xd,
}

fn frequency_count_khz(clocks: &rp2040_pac::CLOCKS, src: Clock, reference_freq_khz: u32) -> u32 {
    // Wait until Frequency counter is not running anymore
    while clocks.fc0_status.read().running().bit_is_set() {}

    unsafe {
        clocks
            .fc0_ref_khz
            .modify(|r, w| w.fc0_ref_khz().bits(reference_freq_khz));
        clocks.fc0_interval.write(|w| w.fc0_interval().bits(10));
        clocks.fc0_min_khz.write(|w| w.fc0_min_khz().bits(0));
        clocks.fc0_max_khz.write(|w| w.fc0_max_khz().bits(u32::MAX));

        // Start measurement by selecting source clock
        clocks.fc0_src.write(|w| w.fc0_src().bits(src as u8));
    }

    while clocks.fc0_status.read().done().bit_is_clear() {}

    clocks.fc0_result.read().khz().bits()
}
