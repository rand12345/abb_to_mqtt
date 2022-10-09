use anyhow::*;
use byteorder::{BigEndian, ByteOrder};
use embedded_hal_0_2::serial::{Read, Write};
use esp_idf_hal::serial::{Rx, Tx};
use esp_idf_hal::{
    gpio::{Gpio18, Gpio19, Unknown},
    serial::{Serial, UART1},
};
use nb::block;
use serde::Serialize;
use std::result::Result::Ok;
use std::{thread, time::Duration, u16};

#[derive(Debug, Serialize)]
pub enum Status {
    Offline,
    Unregistered,
    Registered,
    Online,
}

pub struct SolaxX1Air {
    pub data: Data,
    tx: Tx<UART1>,
    rx: Rx<UART1>,
    pub status: Status,
    pub serial: Vec<u8>,
}

impl SolaxX1Air {
    pub fn new(port: Serial<UART1, Gpio19<Unknown>, Gpio18<Unknown>>) -> Self {
        let (tx, rx) = port.split();
        Self {
            data: Data::default(),
            status: Status::Offline,
            serial: vec![0],
            rx,
            tx,
        }
    }
    pub fn init_inverter(&mut self) -> anyhow::Result<()> {
        let mut status_counter = 0;
        let delay = 300;
        if let std::result::Result::Ok(response) = self.send_and_recv(&send_broadcast_message()) {
            println!("Sent register response back to inverter");
            if self
                .send_and_recv(&register_inverter(&response, 0xA))
                .is_ok()
            {
                self.status = Status::Registered
            }
        } else {
            self.status = Status::Unregistered;
        };

        thread::sleep(Duration::from_millis(delay));
        if self.send_and_recv(&request_config_data()).is_ok() {
            status_counter += 1
        }

        thread::sleep(Duration::from_millis(delay));
        if self.send_and_recv(&request_query_id_data()).is_ok() {
            status_counter += 1
        }

        thread::sleep(Duration::from_millis(delay));
        if self.send_and_recv(&request_live_data()).is_ok() {
            status_counter += 1
        }

        if status_counter != 3 {
            println!("Not enough inverter data recieved to populate modbus registers");
            return Err(anyhow!(
                "Not enough inverter data recieved to populate modbus registers"
            ));
        }
        println!("Enough inverter data to populate modbus registers has been received");
        self.status = Status::Online;
        Ok(())
    }
    pub fn poll_data(&mut self) -> anyhow::Result<&Data> {
        match self.send_and_recv(&request_live_data()) {
            std::result::Result::Ok(_) => {
                self.status = Status::Online;
                Ok(&self.data)
            }
            Err(_) => {
                self.status = Status::Offline;
                Err(anyhow!(
                    "Bad response from inverter during live data request"
                ))
            }
        }
    }
    fn send_and_recv(&mut self, tx: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut response: Vec<u8> = vec![];
        // clear rx buffer
        self.flush()?;
        println!("Gateway >> Solax X1 Air {:02X?}", tx);
        if self.write_all(tx).is_err() {
            self.status = Status::Offline;
            return Err(anyhow!(
                "Gateway >> Inverter RS485 message could not be sent - hardware failure?"
            ));
        };

        // timeout value from Solax protocol 1.7
        thread::sleep(Duration::from_millis(500));
        match self.waiting_data() {
            Some(bytes) => {
                if bytes < 5 {
                    self.status = Status::Offline;
                    return Err(anyhow!("No data received from RS485"));
                }
            }
            None => {
                self.status = Status::Offline;
                return Err(anyhow!("Hardware error on RS485 port"));
            }
        }

        // Read incomming data from serial port and create dynamically sized vector from the quanitity of data received.
        self.read_all(&mut response)?;

        println!("Gateway << Solax X1 Air {:02X?}", response);
        if response[0] != 0xAA && response[1] != 0x55 {
            // flush rx buffer
            self.flush()?;
            return Err(anyhow!(
                "Inverter RS485 message invalid (preamble incorrect)"
            ));
        }

        if check_crc(&response).is_ok() {
            println!("RX CRC ok")
        } else {
            return Err(anyhow!("Inverter CRC is invalid"));
        };

        if response[6] == 0x10 {
            println!("Incomming RS485 data - Register ");
            match response[7] {
                0x80 => {
                    println!("Inverter register request");
                    return Ok(response);
                }
                0x81 => {
                    println!("Inverter address confirmed");
                    return Ok(response);
                }
                0x82 => {
                    println!("Inverter remove confirmed");
                    return Ok(response);
                }
                _ => (),
            };
        };
        if response[6] == 0x11 {
            println!("Incomming RS485 data - Read ");
            match response[7] {
                0x82 => {
                    println!("Received response for query (live data)");
                    self.data.livedata = LiveData::decode(&response);
                    println!("{:#?}", self.data.livedata);
                    return Ok(response);
                }
                0x83 => {
                    println!("Received response for query (ID info)");
                    self.data.id = QueryID::decode(&response);
                    println!("{:#?}", self.data.id);
                    return Ok(response);
                }
                0x84 => {
                    println!("Received response for query (config)");
                    self.data.config = QueryConfig::decode(&response);
                    println!("{:#?}", self.data.config);
                    return Ok(response);
                }
                _ => (),
            }
        };

        if response[6] == 0x12 {
            println!("Incoming RS485 data - Write ");
        };

        if response[6] == 0x13 {
            println!("Incoming RS485 data - Execute ");
        };
        println!(
            "RS485 inverter response was not decoded by parsers {:02X?}",
            response
        );
        Err(anyhow!("Bad data?"))
    }

    fn read_all(&mut self, buf: &mut Vec<u8>) -> Result<u8> {
        let bytes = self.rx.count()?;

        println!("RX {} bytes to be read", bytes);
        while self.rx.count()? > 0 {
            if let Ok(byte) = block!(self.rx.read()) {
                // check if two inverter messages are chained in the buffer
                if buf.len() > 5 && byte == 0x55u8 && buf.last() == Some(&0xaa_u8) {
                    buf.pop(); // remove last 0xaa
                    break;
                };
                buf.push(byte);
            }
        }
        Ok(bytes)
    }
    fn waiting_data(&mut self) -> Option<u8> {
        match self.rx.count() {
            Ok(byte_count) => Some(byte_count),
            Err(_) => None,
        }
    }
    fn write_all(&mut self, bytevec: &[u8]) -> anyhow::Result<()> {
        for byte in bytevec {
            block!(self.tx.write(*byte))?;
        }
        Ok(())
    }
    fn flush(&mut self) -> anyhow::Result<()> {
        let mut dump: Vec<u8> = vec![];
        self.read_all(&mut dump)?;
        Ok(())
    }
}
#[derive(Debug, Default, Serialize)]
pub struct Data {
    pub livedata: LiveData,
    pub id: QueryID,
    pub config: QueryConfig,
}

// https://github.com/syssi/esphome-modbus-solax-x1
#[allow(non_snake_case)]
#[allow(clippy::upper_case_acronyms)]
#[allow(non_camel_case_types)]
#[derive(Debug, Serialize)]
pub enum Safety {
    VDE0126,
    VDE4105,
    AS4777,
    G98,
    C10_11,
    TOR,
    EN50438_NL,
    Denmark2019_W,
    CEB,
    Cyprus2019,
    cNRS097_2_1,
    VDE0126_Greece,
    UTE_C15_712_Fr,
    IEC61727,
    G99,
    CQC,
    VDE0126_Greece_is,
    C15_712_Fr_island_50,
    C15_712_Fr_island_60,
    Guyana,
    MEA_Thailand,
    PEA_Thailand,
    cNewZealand,
    cIreland,
    cCE10_21,
    cRD1699,
    EN50438_Sweden,
    EN50549_PL,
    Czech_PPDS,
    EN50438_Norway,
    EN50438_Portug,
    cCQC_WideRange,
    BRAZIL,
    EN50438_CEZ,
    IEC_Chile,
    Sri_Lanka,
    BRAZIL_240,
    EN50549_SK,
    EN50549_EU,
    G98_NI,
    Denmark2019_E,
    Unknown,
}
impl Default for Safety {
    fn default() -> Self {
        Safety::Unknown
    }
}
#[derive(Debug, Serialize)]
pub enum RunMode {
    Wait,
    Check,
    Normal,
    Fault,
    PermanentFault,
    UpdateMode,
    Unknown,
}

impl Default for RunMode {
    fn default() -> Self {
        RunMode::Unknown
    }
}
#[derive(Debug, Serialize)]
pub enum ErrorCode {
    None,
    MainsLostFault,
    GridVoltFault,
    GridFreqFault,
    PvVoltFault,
    IsolationFault,
    TemperatureOverFault,
    FanFault,
    OtherDeviceFault,
    Unknown,
}

impl Default for ErrorCode {
    fn default() -> Self {
        ErrorCode::Unknown
    }
}
#[derive(Debug, Default, Serialize)]
pub struct LiveData {
    pub temperature: u16,
    pub energy_today: u16,
    pub dc1_voltage: u16,
    pub dc2_voltage: u16,
    pub dc1_current: u16,
    pub dc2_current: u16,
    pub current: u16,
    pub voltage: u16,
    pub frequency: u16,
    pub active_power: u16,
    pub import_active: u32,
    pub runtime_total: u32,
    pub run_mode: RunMode,
    pub error_code: ErrorCode,
}
impl LiveData {
    pub fn decode(response: &[u8]) -> LiveData {
        Self {
            temperature: BigEndian::read_u16(&response[9..]),
            energy_today: BigEndian::read_u16(&response[11..]),
            dc1_voltage: BigEndian::read_u16(&response[13..]),
            dc2_voltage: BigEndian::read_u16(&response[15..]),
            dc1_current: BigEndian::read_u16(&response[17..]),
            dc2_current: BigEndian::read_u16(&response[19..]),
            current: BigEndian::read_u16(&response[21..]),
            voltage: BigEndian::read_u16(&response[23..]),
            frequency: BigEndian::read_u16(&response[25..]),
            active_power: BigEndian::read_u16(&response[27..]),
            import_active: BigEndian::read_u32(&response[31..]),
            runtime_total: BigEndian::read_u32(&response[35..]),
            run_mode: match BigEndian::read_u16(&response[39..]) {
                0 => RunMode::Wait,
                1 => RunMode::Check,
                2 => RunMode::Normal,
                3 => RunMode::Fault,
                4 => RunMode::PermanentFault,
                5 => RunMode::UpdateMode,
                _ => RunMode::Unknown,
            },
            error_code: match BigEndian::read_u32(&response[55..]) {
                0 => ErrorCode::None,
                1 => ErrorCode::MainsLostFault,
                2 => ErrorCode::GridVoltFault,
                3 => ErrorCode::GridFreqFault,
                4 => ErrorCode::PvVoltFault,
                5 => ErrorCode::IsolationFault,
                6 => ErrorCode::TemperatureOverFault,
                7 => ErrorCode::FanFault,
                8 => ErrorCode::OtherDeviceFault,
                _ => ErrorCode::Unknown,
            },
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct QueryID {
    pub inverter_phases: u8,
    pub bus_power: String,
    pub firmware_version: String,
    pub module_name: String,
    pub factory_name: String,
    pub serial_number: String,
    pub rated_bus_voltage: String,
}

impl QueryID {
    pub fn decode(response: &[u8]) -> QueryID {
        Self {
            inverter_phases: response[9],
            bus_power: String::from_utf8_lossy(&response[10..15]).to_string(),
            firmware_version: String::from_utf8_lossy(&response[16..20]).to_string(),
            module_name: String::from_utf8_lossy(&response[21..34]).to_string(),
            factory_name: String::from_utf8_lossy(&response[35..48]).to_string(),
            serial_number: String::from_utf8_lossy(&response[49..62]).to_string(),
            rated_bus_voltage: String::from_utf8_lossy(&response[63..66]).to_string(),
        }
    }
}

#[allow(non_snake_case)]
#[allow(non_camel_case_types)]
#[derive(Debug, Default, Serialize)]
pub struct QueryConfig {
    pub wVpvStart: u16,
    pub wTimeStart: u16,
    pub wVacMinProtect: u16,
    pub wVacMaxProtect: u16,
    pub wFacMinProtect: u16,
    pub wFacMaxProtect: u16,
    pub wDciLimits: u16,
    pub wGrid10MinAvgProtect: u16,
    pub wVacMinSlowProtect: u16,
    pub wVacMaxSlowProtect: u16,
    pub wFacMinSlowProtect: u16,
    pub wFacMaxSlowProtect: u16,
    pub wSafety: Safety,
    pub wPowerfactor_mode: u8,
    pub wPowerfactor_data: u8,
    pub wUpperLimit: u8,
    pub wLowerLimit: u8,
    pub wPowerLow: u8,
    pub wPowerUp: u8,
    pub Qpower_set: u16,
    pub WFreqSetPoint: u16,
    pub WFreqDroopRate: u16,
    pub QuVupRate: u16,
    pub QuVlowRate: u16,
    pub WPowerLimitsPercent: u16,
    pub WWgra: u16,
    pub wWv2: u16,
    pub wWv3: u16,
    pub wWv4: u16,
    pub wQurangeV1: u16,
    pub wQurangeV4: u16,
    pub BVoltPowerLimtit: u16,
    pub WPowerManagerEnable: u16,
    pub WGlobalSeachMPPTStrartFlg: u16,
    pub WFrqProtectRestrictive: u16,
    pub WQuDelayTimer: u16,
    pub WFreqActivePowerDelayTimer: u16,
}

impl QueryConfig {
    pub fn decode(response: &[u8]) -> QueryConfig {
        Self {
            wVpvStart: BigEndian::read_u16(&response[9..]),
            wTimeStart: BigEndian::read_u16(&response[11..]),
            wVacMinProtect: BigEndian::read_u16(&response[13..]),
            wVacMaxProtect: BigEndian::read_u16(&response[15..]),
            wFacMinProtect: BigEndian::read_u16(&response[17..]),
            wFacMaxProtect: BigEndian::read_u16(&response[19..]),
            wDciLimits: BigEndian::read_u16(&response[21..]),
            wGrid10MinAvgProtect: BigEndian::read_u16(&response[23..]),
            wVacMinSlowProtect: BigEndian::read_u16(&response[25..]),
            wVacMaxSlowProtect: BigEndian::read_u16(&response[27..]),
            wFacMinSlowProtect: BigEndian::read_u16(&response[29..]),
            wFacMaxSlowProtect: BigEndian::read_u16(&response[31..]),
            wSafety: match BigEndian::read_u16(&response[33..]) {
                0 => Safety::VDE0126,
                1 => Safety::VDE4105,
                2 => Safety::AS4777,
                3 => Safety::G98,
                4 => Safety::C10_11,
                5 => Safety::TOR,
                6 => Safety::EN50438_NL,
                7 => Safety::Denmark2019_W,
                8 => Safety::CEB,
                9 => Safety::Cyprus2019,
                10 => Safety::cNRS097_2_1,
                11 => Safety::VDE0126_Greece,
                12 => Safety::UTE_C15_712_Fr,
                13 => Safety::IEC61727,
                14 => Safety::G99,
                15 => Safety::CQC,
                16 => Safety::VDE0126_Greece_is,
                17 => Safety::C15_712_Fr_island_50,
                18 => Safety::C15_712_Fr_island_60,
                19 => Safety::Guyana,
                20 => Safety::MEA_Thailand,
                21 => Safety::PEA_Thailand,
                22 => Safety::cNewZealand,
                23 => Safety::cIreland,
                24 => Safety::cCE10_21,
                25 => Safety::cRD1699,
                26 => Safety::EN50438_Sweden,
                27 => Safety::EN50549_PL,
                28 => Safety::Czech_PPDS,
                29 => Safety::EN50438_Norway,
                30 => Safety::EN50438_Portug,
                31 => Safety::cCQC_WideRange,
                32 => Safety::BRAZIL,
                33 => Safety::EN50438_CEZ,
                34 => Safety::IEC_Chile,
                35 => Safety::Sri_Lanka,
                36 => Safety::BRAZIL_240,
                37 => Safety::EN50549_SK,
                38 => Safety::EN50549_EU,
                39 => Safety::G98_NI,
                40 => Safety::Denmark2019_E,
                _ => Safety::Unknown,
            },
            wPowerfactor_mode: response[35],
            wPowerfactor_data: response[36],
            wUpperLimit: response[37],
            wLowerLimit: response[38],
            wPowerLow: response[39],
            wPowerUp: response[40],
            Qpower_set: BigEndian::read_u16(&response[41..]),
            WFreqSetPoint: BigEndian::read_u16(&response[43..]),
            WFreqDroopRate: BigEndian::read_u16(&response[45..]),
            QuVupRate: BigEndian::read_u16(&response[47..]),
            QuVlowRate: BigEndian::read_u16(&response[49..]),
            WPowerLimitsPercent: BigEndian::read_u16(&response[51..]),
            WWgra: BigEndian::read_u16(&response[53..]),
            wWv2: BigEndian::read_u16(&response[55..]),
            wWv3: BigEndian::read_u16(&response[57..]),
            wWv4: BigEndian::read_u16(&response[59..]),
            wQurangeV1: BigEndian::read_u16(&response[61..]),
            wQurangeV4: BigEndian::read_u16(&response[63..]),
            BVoltPowerLimtit: BigEndian::read_u16(&response[65..]),
            WPowerManagerEnable: BigEndian::read_u16(&response[67..]),
            WGlobalSeachMPPTStrartFlg: BigEndian::read_u16(&response[69..]),
            WFrqProtectRestrictive: BigEndian::read_u16(&response[71..]),
            WQuDelayTimer: BigEndian::read_u16(&response[73..]),
            WFreqActivePowerDelayTimer: BigEndian::read_u16(&response[75..]),
        }
    }
}

fn send_broadcast_message() -> Vec<u8> {
    let mut request: Vec<u8> = vec![0xAA, 0x55, 0x01, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00];
    request.extend(calc_partial_crc(&request));
    request
}

fn register_inverter(payload: &[u8], inverter_address: u8) -> Vec<u8> {
    let serial_number = extract_serial_number(payload);
    println!(
        "Discovered serial number {:?}",
        String::from_utf8_lossy(&serial_number)
    );
    let mut message: Vec<u8> = vec![0xAA, 0x55, 0x00, 0x00, 0x00, 0x00, 0x10, 0x01, 0x0F];
    message.extend(serial_number);
    message.extend([inverter_address]);
    let crc: Vec<u8> = calc_partial_crc(&message);
    message.iter().chain(&crc).copied().collect()
    // output
}

fn request_live_data() -> Vec<u8> {
    let mut request: Vec<u8> = vec![0xAA, 0x55, 0x01, 0x00, 0x00, 0x0A, 0x11, 0x02, 0x00];
    request.extend(calc_partial_crc(&request));
    request
}

fn request_query_id_data() -> Vec<u8> {
    let mut request: Vec<u8> = vec![0xAA, 0x55, 0x01, 0x00, 0x00, 0x0A, 0x11, 0x03, 0x00];
    request.extend(calc_partial_crc(&request));
    request
}

fn request_config_data() -> Vec<u8> {
    let mut request: Vec<u8> = vec![0xAA, 0x55, 0x01, 0x00, 0x00, 0x0A, 0x11, 0x04, 0x00];
    request.extend(calc_partial_crc(&request));
    request
}

fn extract_serial_number(payload: &[u8]) -> Vec<u8> {
    payload[9..23].to_vec()
}

fn extract_crc(payload: &[u8]) -> Vec<u8> {
    payload.iter().rev().take(2).rev().copied().collect()
}

fn calc_partial_crc(payload: &Vec<u8>) -> Vec<u8> {
    let mut val: u16 = 0;
    for i in payload {
        val += *i as u16;
    }
    vec![(val >> 8) as u8, val as u8]
}

fn check_crc(i_payload: &Vec<u8>) -> Result<()> {
    let mut payload = i_payload.to_owned();
    payload.pop().unwrap();
    payload.pop().unwrap();
    let crc_should_be = calc_partial_crc(&payload);
    let i_crc: Vec<u8> = extract_crc(i_payload);
    match crc_should_be == i_crc {
        true => Ok(()),
        false => Err(anyhow!("CRC invalid")),
    }
}
