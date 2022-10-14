use anyhow::anyhow;
use embedded_svc::http::server::registry::Registry;
use embedded_svc::http::server::HandlerError;
use embedded_svc::http::server::{Request, Response};
use embedded_svc::http::Headers;
use embedded_svc::io::Read;
use esp_idf_hal::mutex::{Condvar, Mutex};
use esp_idf_svc::http::server::{EspHttpRequest, EspHttpResponse, EspHttpServer};
// use esp_idf_svc::{netif, nvs::EspDefaultNvs, sysloop, wifi::EspWifi};
use esp_idf_sys as _; // If using the `binstart` feature of `esp-idf-sys`, always keep this module imported
use esp_idf_sys::{self as _};
use esp_ota::OtaUpdate;
use log::*;
// use std::env;
use std::sync::Arc;
// use std::thread;
use std::time::{Duration, Instant};
const SERVER: &str = include_str!("server.html");

pub fn httpd(_mutex: Arc<(Mutex<Option<u32>>, Condvar)>) -> anyhow::Result<EspHttpServer> {
    let mut server = EspHttpServer::new(&Default::default())?;

    server
        .handle_get("/", |_req, resp| {
            resp.send_str("Hello from Rust!")?;
            Ok(())
        })?
        .handle_get("/restart", |_req, _resp| {
            info!("Restart requested");
            unsafe { esp_idf_sys::esp_restart() } // no execution beyond this point
            Ok(())
        })?
        .handle_get("/ota", |_req, resp| {
            resp.send_str(SERVER)?;
            Ok(())
        })?
        // *********** OTA POST handler
        .handle_post(
            "/ota",
            |req, resp| -> Result<(), embedded_svc::http::server::HandlerError> {
                ota_processing(req, resp)
            },
        )?;
    Ok(server)
}

fn ota_processing(mut req: EspHttpRequest, resp: EspHttpResponse) -> Result<(), HandlerError> {
    if req.content_len().is_none() || req.content_len() == Some(0) {
        return Err(anyhow!("Multipart POST len is None").into());
    };

    if req.get_boundary().is_none() {
        return Err(anyhow!("No boundary string, check multipart form POST").into());
    }
    let start_time = Instant::now();
    let mut ota = OtaUpdate::begin()?;
    let mut ota_bytes_counter = 0;
    let mut multipart_bytes_counter = 0;
    let mut buf = [0u8; 2048];

    while let Ok(bytelen) = req.reader().read(&mut buf) {
        if start_time.elapsed() > Duration::from_millis(900) {
            std::thread::sleep(Duration::from_millis(10)) //wdt
        }
        if bytelen == 0 {
            break;
        }
        let payload = req.extract_payload(&buf[..bytelen]);
        multipart_bytes_counter += bytelen;
        ota_bytes_counter += &payload.len();

        if let Err(e) = ota.write(payload) {
            println!("Error! {e}\n{:02x?}", payload);
            return Err(anyhow!("Flashed failed at {} bytes", multipart_bytes_counter,).into());
        } else {
            info!(
                "Upload: {}%",
                (multipart_bytes_counter as f32 / req.content_len().unwrap() as f32) * 100.0
            );
        };
    }
    finalise_ota(ota, ota_bytes_counter, resp, start_time)
}

fn finalise_ota(
    ota: esp_ota::OtaUpdate,
    ota_bytes_counter: usize,
    resp: EspHttpResponse,
    start_time: Instant,
) -> Result<(), HandlerError> {
    match ota.finalize() {
        Ok(mut completed_ota) => {
            info!(
                "Flashed {ota_bytes_counter} bytes in {:?}",
                start_time.elapsed()
            );

            completed_ota.set_as_boot_partition()?;
            info!("Set as boot partition - restart required");

            // send plain string back for html alert box
            resp.send_str(&format!(
                "Flashed {ota_bytes_counter} bytes in {:?}",
                start_time.elapsed()
            ))?;
            Ok(())
        }
        Err(e) => Err(anyhow!("Flashed {ota_bytes_counter} bytes failed - {e}").into()),
    }
}

trait Multipart {
    fn get_boundary(&self) -> Option<&str>;
    fn extract_payload<'a>(&self, buf: &'a [u8]) -> &'a [u8];
}
impl Multipart for EspHttpRequest<'_> {
    fn get_boundary(&self) -> Option<&str> {
        match self.header("Content-Type") {
            Some(b) => {
                let r: Vec<&str> = b.split('=').collect();
                if r.len() == 2 {
                    Some(r[1])
                } else {
                    eprint!("Error: Boundary string = {b}");
                    None
                }
            }
            None => {
                eprint!("Error: No Boundary string");
                None
            }
        }
    }
    fn extract_payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        if twoway::find_bytes(buf, self.get_boundary().unwrap().as_bytes()).is_none() {
            return buf;
        }
        let left_offset = match twoway::find_bytes(buf, &[13, 10, 13, 10]) {
            Some(v) => v + 4,
            None => 0,
        };

        let right_offset = match twoway::rfind_bytes(buf, &[13, 10, 45, 45]) {
            Some(v) => v,
            None => buf.len(),
        };
        &buf[left_offset..right_offset]
    }
}
