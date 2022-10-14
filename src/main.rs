#![allow(clippy::redundant_clone)]

use embedded_hal::digital::v2::OutputPin;
use esp_idf_hal::mutex::{Condvar, Mutex};
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::prelude::Hertz;
use esp_idf_hal::serial;
use esp_idf_svc::{netif::EspNetifStack, nvs::EspDefaultNvs, sysloop::EspSysLoopStack};
use std::sync::Arc;
// use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
mod aurora;
mod events;
mod http_server;
mod idf_mqtt;
mod led_strip;
mod wifi_init;
use aurora::*;
use esp_idf_svc::mqtt::client::MqttClientConfiguration;
use led_strip::{Led, LedState};

#[macro_use]
extern crate dotenv_codegen;

// Secrets from .env file
const SSID: &str = dotenv!("SSID");
const PASS: &str = dotenv!("PASS");
const MQTT_ADDR: &str = dotenv!("MQTT_ADDR");
const MQTT_USERNAME: &str = dotenv!("MQTT_USERNAME");
const MQTT_PASSWORD: &str = dotenv!("MQTT_PASSWORD");
const MQTT_CLIENT_ID: &str = dotenv!("MQTT_CLIENT_ID");
const MQTT_TOPIC_NAME: &str = dotenv!("MQTT_TOPIC_NAME");
const MQTT_FREQUENCY: Duration = Duration::from_secs(10);
const INVERTER_COMMS_TIMEOUT: Duration = Duration::from_millis(250);

const VERSION: &str = dotenv!("CARGO_PKG_VERSION");

/*
Need to qualify MQTT publish with a check on wifi status
Maybe add a state
*/

fn main() -> anyhow::Result<()> {
    // Temporary. Will disappear once ESP-IDF 4.4 is released, but for now it is necessary to call this function once,
    // or else some patches to the runtime implemented by esp-idf-sys might not link properly.
    esp_idf_sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let boot_time: Instant = Instant::now();

    #[allow(unused)]
    let netif_stack = Arc::new(EspNetifStack::new()?);
    #[allow(unused)]
    let sys_loop_stack = Arc::new(EspSysLoopStack::new()?);
    #[allow(unused)]
    let default_nvs = Arc::new(EspDefaultNvs::new()?);

    // GPIO setup ****************************
    let peripherals = Peripherals::take().expect("Problem aquiring Peripherals::take()");

    // +3v3 for RS485 tranceiver**************
    let mut powerpin = peripherals.pins.gpio6.into_output()?;
    powerpin.set_drive_strength(esp_idf_hal::gpio::DriveStrength::I40mA)?;
    powerpin.set_high()?; // power to RS485

    // For UART 1 ****************************
    let config = serial::config::Config::default().baudrate(Hertz(19_200));
    let userial: serial::Serial<serial::UART1, _, _> = serial::Serial::new(
        peripherals.uart1,
        serial::Pins {
            tx: peripherals.pins.gpio5,
            rx: peripherals.pins.gpio4,
            cts: None,
            rts: None,
        },
        config,
    )
    .unwrap();

    // LED reworked ****************************
    let mut led = Led::new(
        esp_idf_sys::rmt_channel_t_RMT_CHANNEL_0,
        esp_idf_sys::gpio_num_t_GPIO_NUM_2,
    )?;

    led.set_color(LedState::Off, LedState::Off, LedState::Off)?;

    // Init WiFi network ****************************
    let _wifi = wifi_init::wifi(
        netif_stack.clone(),
        sys_loop_stack.clone(),
        default_nvs.clone(),
        SSID,
        PASS,
    )?;

    led.set_color(LedState::NC, LedState::On, LedState::NC)?;
    let _current_ssid = SSID;

    // Get MAC address - janky + unsafe
    let mut mac: [u8; 6] = [0; 6];
    esp_idf_sys::esp!(unsafe {
        esp_idf_sys::esp_read_mac(
            mac.as_mut_ptr() as *mut _,
            esp_idf_sys::esp_mac_type_t_ESP_MAC_WIFI_STA,
        )
    })?;

    // MQTT unique client_id
    let client_id = &format!("{}{:?}", MQTT_CLIENT_ID, mac);
    let conf = MqttClientConfiguration {
        client_id: Some(client_id),
        username: Some(MQTT_USERNAME),
        password: Some(MQTT_PASSWORD),
        ..Default::default()
    };
    let mqttclient = Arc::new(Mutex::new(idf_mqtt::mqtt_client(
        MQTT_ADDR.to_string(),
        vec!["test".to_string()],
        Some(client_id),
        "12panels".to_string(),
        conf,
    )?));

    let (tx, rx) = userial.split();
    let aurora_arc_mutex = Arc::new(Mutex::new(Aurora::new(rx, tx, INVERTER_COMMS_TIMEOUT)?));
    let inverters_arc_mutex = Arc::new(Mutex::new(vec![
        AuroraInverter::new(2),
        AuroraInverter::new(3),
    ]));
    let _poller = events::periodic_inverter_event(
        inverters_arc_mutex,
        aurora_arc_mutex,
        mqttclient,
        MQTT_FREQUENCY,
        boot_time,
    )?;
    let mutex = Arc::new((Mutex::new(None), Condvar::new()));
    let _httpd = http_server::httpd(mutex)?;

    println!("FW version: {}", VERSION);

    loop {
        led.set_color(LedState::NC, LedState::NC, LedState::On)?;
        thread::sleep(Duration::from_millis(500));
        led.set_color(LedState::NC, LedState::NC, LedState::Off)?;
        thread::sleep(Duration::from_millis(500));
    }
}
