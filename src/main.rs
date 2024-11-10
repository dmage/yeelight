use std::{
    io::{BufRead, Write},
    net::ToSocketAddrs,
};

#[derive(Debug, thiserror::Error)]
enum MainParseError {
    #[error("invalid format: expected X or moonlight:V or normal:V or off")]
    InvalidFormat,
    #[error("invalid number: {0}")]
    InvalidNumber(#[from] std::num::ParseIntError),
    #[error("invalid value: should be between 0 and 100")]
    InvalidValue,
}

#[derive(Debug)]
enum Mode {
    Normal = 1,
    Moonlight = 5,
}

fn parse_main(input: &str) -> Result<(Mode, u8), MainParseError> {
    if input == "off" {
        return Ok((Mode::Normal, 0));
    }

    if let Ok(v) = input.parse::<u8>() {
        match v {
            0..=100 => return Ok((Mode::Moonlight, v)),
            0..=200 => return Ok((Mode::Normal, v - 100)),
            _ => return Err(MainParseError::InvalidFormat),
        }
    }

    let parts: Vec<&str> = input.split(':').collect();
    if parts.len() != 2 {
        return Err(MainParseError::InvalidFormat);
    }

    let v: u8 = parts[1].parse().map_err(MainParseError::InvalidNumber)?;
    if v > 100 {
        return Err(MainParseError::InvalidValue);
    }
    match parts[0] {
        "moonlight" => Ok((Mode::Moonlight, v)),
        "normal" => Ok((Mode::Normal, v)),
        _ => Err(MainParseError::InvalidValue),
    }
}

#[derive(Debug, thiserror::Error)]
enum HsvParseError {
    #[error("invalid format: expected H,S,V or off")]
    InvalidFormat,
    #[error("invalid number: {0}")]
    InvalidNumber(#[from] std::num::ParseIntError),
    #[error("invalid hue: should be between 0 and 359")]
    InvalidHue,
    #[error("invalid saturation: should be between 0 and 100")]
    InvalidSaturation,
    #[error("invalid value: should be between 0 and 100")]
    InvalidValue,
}

fn parse_hsv(input: &str) -> Result<(u16, u8, u8), HsvParseError> {
    if input == "off" {
        return Ok((0, 0, 0));
    }

    let parts: Vec<&str> = input.split(',').collect();
    if parts.len() != 3 {
        return Err(HsvParseError::InvalidFormat);
    }

    let h: u16 = parts[0].parse().map_err(HsvParseError::InvalidNumber)?;
    let s: u8 = parts[1].parse().map_err(HsvParseError::InvalidNumber)?;
    let v: u8 = parts[2].parse().map_err(HsvParseError::InvalidNumber)?;

    if h > 359 {
        return Err(HsvParseError::InvalidHue);
    }
    if s > 100 {
        return Err(HsvParseError::InvalidSaturation);
    }
    if v > 100 {
        return Err(HsvParseError::InvalidValue);
    }

    Ok((h, s, v))
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct Message {
    id: u16,
    method: String,
    params: Vec<Param>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(untagged)]
enum Param {
    Uint8(u8),
    Uint16(u16),
    Str(String),
}

#[derive(Debug)]
struct Client {
    stream: bufstream::BufStream<std::net::TcpStream>,
    next_id: u16,
}

fn connect_with_retries(
    host: &str,
    port: u16,
    max_attempts: u32,
    timeout: std::time::Duration,
) -> std::io::Result<std::net::TcpStream> {
    for attempt in 0..max_attempts {
        let socket_addr = (host, port)
            .to_socket_addrs()?
            .next()
            .expect("unable to resolve hostname");
        match std::net::TcpStream::connect_timeout(&socket_addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                log::debug!("Failed to connect to {}:{}: {}", host, port, e);
                if attempt == max_attempts - 1 {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}

impl Client {
    pub fn connect(host: &str, port: u16) -> std::io::Result<Self> {
        log::debug!("Connecting to {}:{}...", host, port);
        let start = std::time::Instant::now();
        let tcp_stream =
            connect_with_retries(host, port, 150 / 3, std::time::Duration::from_millis(300))?;
        log::debug!("Connected in {:?}", start.elapsed());
        tcp_stream
            .set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .expect("set_read_timeout call failed");
        tcp_stream
            .set_write_timeout(Some(std::time::Duration::from_millis(200)))
            .expect("set_write_timeout call failed");
        let stream = bufstream::BufStream::new(tcp_stream);
        Ok(Client { stream, next_id: 1 })
    }

    pub fn send_command(
        &mut self,
        method: &str,
        params: Vec<Param>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let message = Message {
            id: self.next_id,
            method: method.to_string(),
            params,
        };
        self.next_id += 1;
        let json_message = serde_json::to_string(&message)?;
        log::debug!("Sending: {}", json_message);
        let start = std::time::Instant::now();
        self.stream
            .write_all(format!("{}\r\n", json_message).as_bytes())?;
        self.stream.flush()?;

        let mut bytes = Vec::new();
        match self.stream.read_until(b'\n', &mut bytes) {
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                log::debug!("Re-sending: {}", json_message);
                self.stream
                    .write_all(format!("{}\r\n", json_message).as_bytes())?;
                self.stream.flush()?;
                self.stream.read_until(b'\n', &mut bytes)?;
            }
            Err(e) => return Err(Box::from(e)),
            Ok(_) => {}
        }

        let mut response = String::from_utf8(bytes)?;
        response.truncate(response.trim_end().len());
        log::debug!("Received (after {:?}): {}", start.elapsed(), response);
        Ok(response)
    }
}

fn process(
    host: &String,
    port: u16,
    main: Option<&String>,
    ambient: Option<&String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect(host.as_str(), port)?;

    std::thread::sleep(std::time::Duration::from_millis(5));

    if let Some(str) = main {
        let (mode, v) = parse_main(str)?;

        if v == 0 {
            client.send_command(
                "set_power",
                vec![
                    Param::Str(String::from("off")),
                    Param::Str(String::from("smooth")),
                    Param::Uint16(500),
                ],
            )?;
        } else {
            client.send_command(
                "set_power",
                vec![
                    Param::Str(String::from("on")),
                    Param::Str(String::from("smooth")),
                    Param::Uint16(500),
                    Param::Uint8(mode as u8),
                ],
            )?;

            client.send_command(
                "set_bright",
                vec![
                    Param::Uint8(v),
                    Param::Str(String::from("smooth")),
                    Param::Uint16(500),
                ],
            )?;
        }
    }

    if let Some(str) = ambient {
        let (h, s, v) = parse_hsv(str)?;

        if v == 0 {
            client.send_command(
                "bg_set_power",
                vec![
                    Param::Str(String::from("off")),
                    Param::Str(String::from("smooth")),
                    Param::Uint16(500),
                ],
            )?;
        } else {
            client.send_command(
                "bg_set_power",
                vec![
                    Param::Str(String::from("on")),
                    Param::Str(String::from("smooth")),
                    Param::Uint16(500),
                ],
            )?;

            client.send_command(
                "bg_set_hsv",
                vec![
                    Param::Uint16(h),
                    Param::Uint8(s),
                    Param::Str(String::from("smooth")),
                    Param::Uint16(500),
                ],
            )?;

            client.send_command(
                "bg_set_bright",
                vec![
                    Param::Uint8(v),
                    Param::Str(String::from("smooth")),
                    Param::Uint16(500),
                ],
            )?;
        }
    }

    Ok(())
}

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let matches = clap::Command::new("App")
        .arg(
            clap::Arg::new("main")
                .long("main")
                .value_name("X|off|moonlight:V|normal:V")
                .help("Set main light (X is between 0 and 200, V is between 1 and 100)"),
        )
        .arg(
            clap::Arg::new("ambient")
                .long("ambient")
                .value_name("H,S,V|off")
                .help("Set ambient light"),
        )
        .arg(clap::Arg::new("host").required(true))
        .get_matches();

    let host = matches.get_one::<String>("host").expect("required");
    let port: u16 = 55443;

    match process(
        host,
        port,
        matches.get_one::<String>("main"),
        matches.get_one::<String>("ambient"),
    ) {
        Err(err) => {
            eprintln!("Error: {}", err);
            std::process::ExitCode::from(1)
        }
        Ok(_) => std::process::ExitCode::from(0),
    }
}
