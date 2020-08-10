#![no_std]
#![no_main]

#[macro_use]
extern crate log;

use smoltcp as net;
use stm32h7_ethernet as ethernet;
use stm32h7xx_hal::{gpio::Speed, prelude::*};

use heapless::{consts, String};
use si7021::Si7021;

use cortex_m;

use panic_halt as _;
use serde::{Deserialize, Serialize};

use rtic::cyccnt::{Instant, U32Ext};

mod tcp_stack;

use minimq::{
    embedded_nal::{IpAddr, Ipv4Addr},
    MqttClient, QoS,
};
use tcp_stack::NetworkStack;

pub struct NetStorage {
    ip_addrs: [net::wire::IpCidr; 1],
    neighbor_cache: [Option<(net::wire::IpAddress, net::iface::Neighbor)>; 8],
}

static mut NET_STORE: NetStorage = NetStorage {
    // Placeholder for the real IP address, which is initialized at runtime.
    ip_addrs: [net::wire::IpCidr::Ipv6(
        net::wire::Ipv6Cidr::SOLICITED_NODE_PREFIX,
    )],
    neighbor_cache: [None; 8],
};

#[link_section = ".sram3.eth"]
static mut DES_RING: ethernet::DesRing = ethernet::DesRing::new();

#[derive(Serialize, Deserialize)]
struct Temperature {
    temperature_c: f32,
}

type NetworkInterface =
    net::iface::EthernetInterface<'static, 'static, 'static, ethernet::EthernetDMA<'static>>;

macro_rules! add_socket {
    ($sockets:ident, $tx_storage:ident, $rx_storage:ident) => {
        let mut $rx_storage = [0; 4096];
        let mut $tx_storage = [0; 4096];

        let tcp_socket = {
            let tx_buffer = net::socket::TcpSocketBuffer::new(&mut $tx_storage[..]);
            let rx_buffer = net::socket::TcpSocketBuffer::new(&mut $rx_storage[..]);

            net::socket::TcpSocket::new(tx_buffer, rx_buffer)
        };

        let _handle = $sockets.add(tcp_socket);
    };
}

#[cfg(not(feature = "semihosting"))]
fn init_log() {}

#[cfg(feature = "semihosting")]
fn init_log() {
    use cortex_m_log::log::{init as init_log, Logger};
    use cortex_m_log::printer::semihosting::{hio::HStdout, InterruptOk};
    use log::LevelFilter;

    static mut LOGGER: Option<Logger<InterruptOk<HStdout>>> = None;

    let logger = Logger {
        inner: InterruptOk::<_>::stdout().unwrap(),
        level: LevelFilter::Info,
    };

    let logger = unsafe { LOGGER.get_or_insert(logger) };

    init_log(logger).unwrap();
}

#[rtic::app(device = stm32h7xx_hal::stm32, peripherals = true, monotonic = rtic::cyccnt::CYCCNT)]
const APP: () = {
    struct Resources {
        net_interface: NetworkInterface,
        si7021: Si7021<stm32h7xx_hal::i2c::I2c<stm32h7xx_hal::stm32::I2C2>>,
    }

    #[init]
    fn init(c: init::Context) -> init::LateResources {
        let mut cp = cortex_m::Peripherals::take().unwrap();
        cp.DWT.enable_cycle_counter();

        // Enable SRAM3 for the descriptor ring.
        c.device.RCC.ahb2enr.modify(|_, w| w.sram3en().set_bit());

        let rcc = c.device.RCC.constrain();
        let pwr = c.device.PWR.constrain();
        let vos = pwr.freeze();

        let ccdr = rcc
            .sysclk(400.mhz())
            .hclk(200.mhz())
            .per_ck(100.mhz())
            .pll2_p_ck(100.mhz())
            .pll2_q_ck(100.mhz())
            .freeze(vos, &c.device.SYSCFG);

        init_log();

        let gpioa = c.device.GPIOA.split(ccdr.peripheral.GPIOA);
        let gpiob = c.device.GPIOB.split(ccdr.peripheral.GPIOB);
        let gpioc = c.device.GPIOC.split(ccdr.peripheral.GPIOC);
        let gpiof = c.device.GPIOF.split(ccdr.peripheral.GPIOF);
        let gpiog = c.device.GPIOG.split(ccdr.peripheral.GPIOG);

        // Configure ethernet IO
        {
            let _rmii_refclk = gpioa.pa1.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_mdio = gpioa.pa2.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_mdc = gpioc.pc1.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_crs_dv = gpioa.pa7.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_rxd0 = gpioc.pc4.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_rxd1 = gpioc.pc5.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_tx_en = gpiog.pg11.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_txd0 = gpiog.pg13.into_alternate_af11().set_speed(Speed::VeryHigh);
            let _rmii_txd1 = gpiob.pb13.into_alternate_af11().set_speed(Speed::VeryHigh);
        }

        // Configure ethernet
        let net_interface = {
            let mac_addr = net::wire::EthernetAddress([0xAC, 0x6F, 0x7A, 0xDE, 0xD6, 0xC8]);
            let (eth_dma, _eth_mac) = unsafe {
                ethernet::ethernet_init(
                    c.device.ETHERNET_MAC,
                    c.device.ETHERNET_MTL,
                    c.device.ETHERNET_DMA,
                    &mut DES_RING,
                    mac_addr.clone(),
                )
            };

            unsafe { ethernet::enable_interrupt() }

            let store = unsafe { &mut NET_STORE };

            store.ip_addrs[0] = net::wire::IpCidr::new(net::wire::IpAddress::v4(10, 0, 0, 2), 24);

            let neighbor_cache = net::iface::NeighborCache::new(&mut store.neighbor_cache[..]);

            net::iface::EthernetInterfaceBuilder::new(eth_dma)
                .ethernet_addr(mac_addr)
                .neighbor_cache(neighbor_cache)
                .ip_addrs(&mut store.ip_addrs[..])
                .finalize()
        };

        // Configure I2C
        let si7021 = {
            let i2c_sda = gpiof.pf0.into_open_drain_output().into_alternate_af4();
            let i2c_scl = gpiof.pf1.into_open_drain_output().into_alternate_af4();
            let i2c = c.device.I2C2.i2c(
                (i2c_scl, i2c_sda),
                100.khz(),
                ccdr.peripheral.I2C2,
                &ccdr.clocks,
            );

            Si7021::new(i2c)
        };

        cp.SCB.enable_icache();

        init::LateResources {
            net_interface: net_interface,
            si7021: si7021,
        }
    }

    #[idle(resources=[net_interface, si7021])]
    fn idle(c: idle::Context) -> ! {
        let mut time: u32 = 0;
        let mut next_ms = Instant::now();

        next_ms += 400_00.cycles();

        let mut socket_set_entries: [_; 8] = Default::default();
        let mut sockets = net::socket::SocketSet::new(&mut socket_set_entries[..]);
        add_socket!(sockets, rx_storage, tx_storage);

        let tcp_stack = NetworkStack::new(c.resources.net_interface, sockets);
        let mut client = MqttClient::<consts::U256, _>::new(
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            "nucleo",
            tcp_stack,
        )
        .unwrap();

        loop {
            let tick = Instant::now() > next_ms;

            if tick {
                next_ms += 400_000.cycles();
                time += 1;
            }

            if tick && (time % 1000) == 0 {
                client
                    .publish("nucleo", "Hello, World!".as_bytes(), QoS::AtMostOnce, &[])
                    .unwrap();

                let temperature = Temperature {
                    temperature_c: c.resources.si7021.temperature_celsius().unwrap(),
                };
                let temperature: String<consts::U256> =
                    serde_json_core::to_string(&temperature).unwrap();
                client
                    .publish(
                        "temperature",
                        &temperature.into_bytes(),
                        QoS::AtMostOnce,
                        &[],
                    )
                    .unwrap();
            }

            client
                .poll(|_client, topic, message, _properties| match topic {
                    _ => info!("On '{:?}', received: {:?}", topic, message),
                })
                .unwrap();

            // Update the TCP stack.
            let sleep = client.network_stack.update(time);
            if sleep {
                //cortex_m::asm::wfi();
                cortex_m::asm::nop();
            }
        }
    }

    #[task(binds=ETH)]
    fn eth(_: eth::Context) {
        unsafe { ethernet::interrupt_handler() }
    }
};
