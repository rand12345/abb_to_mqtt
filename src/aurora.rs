#![allow(dead_code, clippy::clone_on_copy)]

use anyhow::*;
use embedded_hal::serial::Write;
use esp_idf_hal::serial::{Rx, Tx, UART1};
use log::info;
use nb::block;
use serde::Serialize;
use std::convert::TryInto;
use std::result::Result::Ok;
use std::time::{Duration, Instant};

type DataMap = std::collections::HashMap<String, serde_json::Value>;

#[derive(Debug)]
pub struct MqttMessage {
    pub topic: String,
    pub payload: String,
}

#[derive(Debug, Copy, Clone, Serialize)]
pub enum Status {
    Offline,
    Online,
}
#[derive(Debug, Copy, Clone, Serialize)]
pub struct Availablilty {
    status: Status,
}

#[derive(Debug, Copy, Clone, Default, Serialize)]
pub struct EnergyTotals {
    day: f32,
    week: f32,
    month: f32,
    year: f32,
    total: f32,
    since_reset: f32,
}
impl EnergyTotals {
    pub fn update_value(
        &mut self,
        command: EnergyRequest,
        response: [u8; 8],
    ) -> anyhow::Result<()> {
        let f: f32 = convert_bytes_to_i32(response)? as f32 * 0.001;
        match command {
            EnergyRequest::Day => self.day = f,
            EnergyRequest::Week => self.week = f,
            EnergyRequest::Month => self.month = f,
            EnergyRequest::Year => self.year = f,
            EnergyRequest::Total => self.total = f,
            EnergyRequest::SinceReset => self.since_reset = f,
        }
        Ok(())
    }
}
#[derive(Debug, Copy, Clone)]
pub enum EnergyRequest {
    Day,
    Week,
    Month,
    Year,
    Total,
    SinceReset,
}

impl EnergyRequest {
    pub fn as_code(&self) -> Result<u8> {
        Ok(match self {
            Self::Day => 0,
            Self::Week => 1,
            Self::Month => 3,
            Self::Year => 4,
            Self::Total => 5,
            Self::SinceReset => 6,
        })
    }
}

#[derive(Copy, Clone)]
pub struct AuroraInverter {
    pub data: Dsp,
    availability: Availablilty,
    id: u8,
    pub energy: EnergyTotals,
    lastmessage: Instant,
}
impl AuroraInverter {
    pub fn new(id: u8) -> Self {
        Self {
            data: Dsp::default(),
            availability: Availablilty {
                status: Status::Offline,
            },
            id,
            energy: EnergyTotals::default(),
            lastmessage: Instant::now() - Duration::from_secs(60),
        }
    }
    pub fn id(&self) -> u8 {
        self.id
    }
}
impl core::fmt::Debug for AuroraInverter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(
            f,
            "Inverter ID: {}\n{:?}\n{:#?}\n{:#?}",
            self.id, self.availability, self.energy, self.data
        )
    }
}

pub struct Aurora {
    tx: Tx<UART1>,
    rx: Rx<UART1>,
    timeout: Duration,
}
impl Aurora {
    // protocol handler only
    pub fn new(rx: Rx<UART1>, tx: Tx<UART1>, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self { rx, tx, timeout })
    }
    pub fn init_inverter(&mut self, inverter: &mut AuroraInverter) -> anyhow::Result<()> {
        // checks that inverter is communicating and not alarming
        self.rx.flush()?;
        let response = self.request_data(
            inverter,
            DspFunction::Measure,
            DspRequest::Grid.as_code()?,
            false,
        )?;
        if convert_bytes_to_f32(response)? > 0.0 {
            inverter.availability = Availablilty {
                status: Status::Online,
            };
            inverter.lastmessage = Instant::now();
            return Ok(());
        }

        inverter.availability = Availablilty {
            status: Status::Offline,
        };
        Err(anyhow!("No response from inverter"))
    }
    pub fn poll_inverter(&mut self, inverter: &mut AuroraInverter) -> anyhow::Result<&mut Aurora> {
        self.init_inverter(inverter)?;
        // aurora.init_inverter(inverter2)?;
        self.poll_data(inverter)?;
        self.request_energy_totals(inverter)?;

        inverter.lastmessage = Instant::now();
        // println!("{:?}", inverter);

        Ok(self)
    }

    pub fn data_to_vec_mqtt_json(
        &self,
        inverter: &AuroraInverter,
        mqtt_topic_name: &str,
    ) -> anyhow::Result<Vec<MqttMessage>> {
        let mut mqtt_payload: Vec<MqttMessage> = vec![];
        let d1 = serde_json::to_string(&inverter.data)?;
        let d2 = serde_json::to_string(&inverter.energy)?;
        let d3 = serde_json::to_string(&inverter.availability)?;
        [d1, d2, d3].iter().for_each(|message_json| {
            let data: DataMap =
                serde_json::from_str(message_json).expect("Serde error in contruction");
            data.iter().for_each(|(key, value)| {
                mqtt_payload.push(MqttMessage {
                    topic: format!("{}/{:?}/{}", mqtt_topic_name, inverter.id(), key),
                    payload: format!("{}", value),
                });
            });
        });

        Ok(mqtt_payload)
    }
    pub fn poll_data(&mut self, inverter: &mut AuroraInverter) -> anyhow::Result<()> {
        // takes mut reference of inverter struct and updates values

        for request in [
            // DspRequest::GridVoltage,
            DspRequest::Grid,
            DspRequest::Current,
            DspRequest::GridPower,
            DspRequest::Frequency,
            DspRequest::Vbulk,
            DspRequest::Ileak,
            DspRequest::IleakDc,
            DspRequest::Pin1,
            DspRequest::Pin2,
            DspRequest::InverterTemperature,
            DspRequest::BoosterTemperature,
            DspRequest::Input1Current,
            DspRequest::Input1Voltage,
            DspRequest::Input2Current,
            DspRequest::Input2Voltage,
            DspRequest::PowerPeak,
            DspRequest::PowerPeakToday,
        ]
        .iter()
        {
            let response =
                self.request_data(inverter, DspFunction::Measure, request.as_code()?, false)?;
            inverter.data.update_value(*request, response)?;
            inverter.lastmessage = Instant::now();
        }
        Ok(())
    }

    pub fn request_energy_totals(
        &mut self,
        inverter: &mut AuroraInverter,
    ) -> anyhow::Result<&mut Aurora> {
        for request in [
            EnergyRequest::Day,
            EnergyRequest::Week,
            EnergyRequest::Month,
            EnergyRequest::Year,
            EnergyRequest::Total,
            EnergyRequest::SinceReset,
        ]
        .iter()
        {
            let response = self.request_data(
                inverter,
                DspFunction::CumulatedEnergy,
                request.as_code()?,
                false,
            )?;
            inverter.energy.update_value(*request, response)?;
            inverter.lastmessage = Instant::now();
        }

        Ok(self)
    }

    fn request_data(
        &mut self,
        inverter: &mut AuroraInverter,
        function: DspFunction,
        command: u8,
        global: bool,
    ) -> anyhow::Result<[u8; 8]> {
        // uses enum to get data

        let global_measure: u8 = u8::from(global);
        let mut request: [u8; 10] = [
            inverter.id,
            function.to_code(),
            command,
            global_measure,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        // Clone here to stop overwrite of payload
        [request[8], request[9]] = crc(&mut request.clone()[0..8]);
        let mut response: [u8; 8] = [0u8; 8];

        self.send_and_recv(&request, &mut response, inverter)?;
        self.response_error_check(&mut response)?;
        Ok(response)
    }

    fn response_error_check(&self, response: &mut [u8]) -> anyhow::Result<()> {
        if self.parse(response[0]) != TransmissionState::OK {
            return Err(anyhow!(
                "ABB response error state {:?}",
                self.parse(response[0])
            ));
        }
        Ok(())
    }

    fn send_and_recv(
        &mut self,
        request: &[u8],
        response: &mut [u8; 8],
        inverter: &mut AuroraInverter,
    ) -> anyhow::Result<()> {
        // clear rx buffer
        self.rx.flush()?;
        info!("ESP >> ABB{} {:02x?}", inverter.id, request);
        self.write_all(request)?;
        self.read_all(response)?;
        Ok(())
    }

    fn read_all(&mut self, buf: &mut [u8; 8]) -> anyhow::Result<()> {
        // println!("RX {} bytes to be read", bytes);
        self.rx.flush()?;
        self.rx.read_bytes_blocking(buf, self.timeout)?;

        info!("ESP << ABB  {:02x?}", buf);
        Ok(())
    }

    fn write_all(&mut self, bytevec: &[u8]) -> anyhow::Result<()> {
        for byte in bytevec {
            block!(self.tx.write(*byte))?;
        }
        Ok(())
    }

    fn parse(&self, code: u8) -> TransmissionState {
        match code {
            0 => TransmissionState::OK,
            51 => TransmissionState::NotImplemented,
            52 => TransmissionState::NotExist,
            53 => TransmissionState::OutOfRange,
            54 => TransmissionState::EEpromError,
            55 => TransmissionState::NotServiceMode,
            56 => TransmissionState::InternalMicroError,
            57 => TransmissionState::NotExecuted,
            58 => TransmissionState::Retry,
            _ => TransmissionState::Unknown,
        }
    }
}
#[derive(Debug, Copy, Clone, Default, Serialize)]
pub struct Dsp {
    pub grid: f32,
    pub current: f32,
    pub gridpower: f32,
    pub frequency: f32,
    pub vbulk: f32,
    pub ileakdc: f32,
    pub ileak: f32,
    pub pin1: f32,
    pub pin2: f32,
    pub invertertemperature: f32,
    pub boostertemperature: f32,
    pub input1voltage: f32,
    pub input1current: f32,
    pub input2voltage: f32,
    pub input2current: f32,
    pub powerpeak: f32,
    pub powerpeaktoday: f32,
    #[serde(skip_serializing)]
    pub gridvoltagedcdc: f32,
    #[serde(skip_serializing)]
    pub gridfrequencydcdc: f32,
    #[serde(skip_serializing)]
    pub isolationresistance: f32,
    #[serde(skip_serializing)]
    pub vbulkdcdc: f32,
    #[serde(skip_serializing)]
    pub averagegridvoltage: f32,
    #[serde(skip_serializing)]
    pub vbulkmid: f32,
    #[serde(skip_serializing)]
    pub gridvoltageneutral: f32,
    #[serde(skip_serializing)]
    pub windgeneratorfrequency: f32,
    #[serde(skip_serializing)]
    pub gridvoltageneutralphase: f32,
    #[serde(skip_serializing)]
    pub gridcurrentphaser: f32,
    #[serde(skip_serializing)]
    pub gridcurrentphases: f32,
    #[serde(skip_serializing)]
    pub gridcurrentphaset: f32,
    #[serde(skip_serializing)]
    pub frequencyphaser: f32,
    #[serde(skip_serializing)]
    pub frequencyphases: f32,
    #[serde(skip_serializing)]
    pub frequencyphaset: f32,
    #[serde(skip_serializing)]
    pub vbulkpostitive: f32,
    #[serde(skip_serializing)]
    pub vbulknegative: f32,
    #[serde(skip_serializing)]
    pub supervisortemperature: f32,
    #[serde(skip_serializing)]
    pub alimtemperature: f32,
    #[serde(skip_serializing)]
    pub heatsinktemperature: f32,
    #[serde(skip_serializing)]
    pub powersaturationlimit: f32,
    #[serde(skip_serializing)]
    pub riferimentoanellobulk: f32,
    #[serde(skip_serializing)]
    pub vpanelmicro: f32,
    #[serde(skip_serializing)]
    pub gridvoltagephaser: f32,
    #[serde(skip_serializing)]
    pub gridvoltagephases: f32,
    #[serde(skip_serializing)]
    pub gridvoltagephaset: f32,
}

impl Dsp {
    pub fn update_value(&mut self, command: DspRequest, response: [u8; 8]) -> anyhow::Result<()> {
        let f = convert_bytes_to_f32(response)?;
        // let i = convert_energy_bytes(response)?;
        match command {
            // DspRequest::NC0 => todo!(),
            DspRequest::Grid => self.grid = f,
            DspRequest::Current => self.current = f,
            DspRequest::GridPower => self.gridpower = f * 0.001,
            DspRequest::Frequency => self.frequency = f,
            DspRequest::Vbulk => self.vbulk = f,
            DspRequest::IleakDc => self.ileakdc = f,
            DspRequest::Ileak => self.ileak = f,
            DspRequest::Pin1 => self.pin1 = f * 0.001,
            DspRequest::Pin2 => self.pin2 = f * 0.001,
            DspRequest::InverterTemperature => self.invertertemperature = f,
            DspRequest::BoosterTemperature => self.boostertemperature = f,
            DspRequest::Input1Voltage => self.input1voltage = f,
            DspRequest::Input1Current => self.input1current = f,
            DspRequest::Input2Voltage => self.input2voltage = f,
            DspRequest::Input2Current => self.input2current = f,
            DspRequest::IsolationResistance => self.isolationresistance = f,
            DspRequest::VbulkDCDC => self.vbulkdcdc = f,
            DspRequest::AverageGridVoltage => self.averagegridvoltage = f,
            DspRequest::VbulkMid => self.vbulkmid = f,
            DspRequest::PowerPeak => self.powerpeak = f * 0.001,
            DspRequest::PowerPeakToday => self.powerpeaktoday = f * 0.001,
            DspRequest::HeatSinkTemperature => self.heatsinktemperature = f,
            _ => {
                info!("Not supported");
            }
        }
        Ok(())
    }
}

#[allow(unused)]
#[derive(Copy, Clone)]
pub enum DspRequest {
    GridVoltage,
    Grid,
    Current,
    GridPower,
    Frequency,
    Vbulk,
    IleakDc,
    Ileak,
    Pin1,
    Pin2,
    NC10,
    NC11,
    NC12,
    NC13,
    NC14,
    NC15,
    NC16,
    NC17,
    NC18,
    NC19,
    NC20,
    InverterTemperature,
    BoosterTemperature,
    Input1Voltage,
    NC24,
    Input1Current,
    Input2Voltage,
    Input2Current,
    GridVoltageDCDC,
    GridFrequencyDCDC,
    IsolationResistance,
    VbulkDCDC,
    AverageGridVoltage,
    VbulkMid,
    PowerPeak,
    PowerPeakToday,
    GridVoltageneutral,
    WindGeneratorFrequency,
    GridVoltageneutralphase,
    GridCurrentphaser,
    GridCurrentphases,
    GridCurrentphaset,
    Frequencyphaser,
    Frequencyphases,
    Frequencyphaset,
    VbulkPostitive,
    VbulkNegative,
    SupervisorTemperature,
    AlimTemperature,
    HeatSinkTemperature,
    Temperature1,
    Temperature2,
    Temperature3,
    Fan1Speed,
    Fan2Speed,
    Fan3Speed,
    Fan4Speed,
    Fan5Speed,
    PowerSaturationlimit,
    RiferimentoAnelloBulk,
    Vpanelmicro,
    GridVoltagephaser,
    GridVoltagephases,
    GridVoltagephaset,
}
impl DspRequest {
    pub fn as_code(&self) -> anyhow::Result<u8> {
        Ok((*self as usize).try_into()?)
    }
}

pub enum DspFunction {
    State,                //50
    PN,                   //52
    Version,              //58
    Measure,              //59
    Serial,               //63
    MaufacturerDate,      //65
    Flags,                //67
    CumulatedFloatEnergy, //68
    TimeDate,             //70
    Firmware,             //72
    CumulatedEnergy,      //78
    Alarms,               //86
}
impl DspFunction {
    pub fn to_code(&self) -> u8 {
        match self {
            DspFunction::State => 50,
            DspFunction::PN => 52,
            DspFunction::Version => 58,
            DspFunction::Measure => 59,
            DspFunction::Serial => 63,
            DspFunction::MaufacturerDate => 65,
            DspFunction::Flags => 67,
            DspFunction::CumulatedFloatEnergy => 68,
            DspFunction::TimeDate => 70,
            DspFunction::Firmware => 72,
            DspFunction::CumulatedEnergy => 78,
            DspFunction::Alarms => 86,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TransmissionState {
    OK,
    NotImplemented,
    NotExist,
    OutOfRange,
    EEpromError,
    NotServiceMode,
    InternalMicroError,
    NotExecuted,
    Retry,
    Unknown,
}

fn crc(buf: &mut [u8]) -> [u8; 2] {
    let poly = 0x8408;
    let mask = 0xffff;
    let mut crc: u16 = 0xffff;

    if buf.is_empty() {
        return (!crc & mask).to_le_bytes();
    }

    for data in buf.iter_mut() {
        for _i in 0..8u8 {
            if (crc & 0x1) ^ ((*data & 0x1) as u16) > 0 {
                crc = ((crc >> 1) ^ poly) & mask
            } else {
                crc >>= 1;
            }
            *data >>= 1;
        }
    }
    crc = !crc & mask;
    crc.to_le_bytes()
}

fn convert_bytes_to_f32(response: [u8; 8]) -> anyhow::Result<f32> {
    Ok(f32::from_be_bytes(response[2..6].try_into()?))
}

fn convert_bytes_to_i32(response: [u8; 8]) -> anyhow::Result<i32> {
    Ok(i32::from_be_bytes(response[2..6].try_into()?))
}
