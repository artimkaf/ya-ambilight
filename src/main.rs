use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{Read, Write},
    net::{TcpStream, UdpSocket},
    time::{Duration, Instant},
};

#[derive(Debug, Deserialize)]
#[serde(default)]
struct Config {
    bind: String,
    max_input_blocks: usize,
    frame_rate: u32,
    stream_timeout_ms: u64,
    loss: String,
    brightness: f32,
    gamma: f32,
    red_gain: f32,
    green_gain: f32,
    blue_gain: f32,
    channel_order: String,
    ring_offset: usize,
    ring_reverse: bool,
    test_mode: String,
    logo_mode: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:5568".into(),
            max_input_blocks: 490,
            frame_rate: 15,
            stream_timeout_ms: 1500,
            loss: "release".into(),
            brightness: 0.35,
            gamma: 1.0,
            red_gain: 1.0,
            green_gain: 1.0,
            blue_gain: 1.0,
            channel_order: "RGB".into(),
            ring_offset: 0,
            ring_reverse: false,
            test_mode: "off".into(),
            logo_mode: "average".into(),
        }
    }
}
impl Config {
    fn validate(&self) -> Result<(), String> {
        if self.max_input_blocks == 0
            || self.max_input_blocks > 490
            || self.frame_rate == 0
            || self.frame_rate > 60
            || self.stream_timeout_ms < 100
            || self.stream_timeout_ms > 60000
            || ![
                self.brightness,
                self.gamma,
                self.red_gain,
                self.green_gain,
                self.blue_gain,
            ]
            .iter()
            .all(|x| x.is_finite())
            || !(0.0..=1.0).contains(&self.brightness)
            || !(0.1..=4.0).contains(&self.gamma)
            || [self.red_gain, self.green_gain, self.blue_gain]
                .iter()
                .any(|x| !(0.0..=4.0).contains(x))
            || !["release", "black"].contains(&self.loss.as_str())
            || !["off", "zones", "rgb"].contains(&self.test_mode.as_str())
            || !["off", "average", "zones"].contains(&self.logo_mode.as_str())
            || !["RGB", "RBG", "GRB", "GBR", "BRG", "BGR"].contains(&self.channel_order.as_str())
        {
            return Err("invalid config.toml value".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Layout {
    ring: usize,
    logo: bool,
}
#[derive(Serialize)]
struct Frame {
    colors: Vec<String>,
}

struct Api {
    port: u16,
    base: String,
    token: String,
    ring_profile_confirmed: bool,
    logo_active: bool,
}
impl Api {
    fn new() -> Result<Self, String> {
        let url = env::var("YAM_API_URL")
            .map_err(|_| "YAM_API_URL missing: run as a YAM binary package")?;
        let rest = url
            .strip_prefix("http://127.0.0.1:")
            .ok_or("YAM_API_URL must use loopback HTTP")?;
        let (port, path) = rest.split_once('/').ok_or("invalid YAM_API_URL")?;
        let port = port.parse().map_err(|_| "invalid YAM API port")?;
        if !path.starts_with("api/runtime/") || path.contains(['\r', '\n']) {
            return Err("invalid YAM runtime path".into());
        }
        let token = env::var("YAM_API_TOKEN").map_err(|_| "YAM_API_TOKEN missing")?;
        if token.contains(['\r', '\n']) {
            return Err("invalid YAM API token".into());
        }
        Ok(Self {
            port,
            base: format!("/{path}"),
            token,
            ring_profile_confirmed: false,
            logo_active: false,
        })
    }
    fn call(&self, method: &str, suffix: &str, body: Option<&str>) -> Result<String, String> {
        let mut stream = TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", self.port)
                .parse()
                .map_err(|_| "invalid address")?,
            Duration::from_millis(300),
        )
        .map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| e.to_string())?;
        let body = body.unwrap_or("");
        let request = format!("{method} {}{suffix} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-YAM-Token: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", self.base, self.token, body.len());
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut response = Vec::new();
        stream
            .take(65536)
            .read_to_end(&mut response)
            .map_err(|e| e.to_string())?;
        let response = String::from_utf8(response).map_err(|e| e.to_string())?;
        let (header, payload) = response.split_once("\r\n\r\n").ok_or("bad HTTP response")?;
        let status = header.split_whitespace().nth(1).ok_or("bad HTTP status")?;
        if !status.starts_with('2') {
            return Err(format!("YAM HTTP {status}: {payload}"));
        }
        Ok(payload.into())
    }
    fn layout(&mut self) -> Result<Layout, String> {
        let raw = self.call("GET", "/device", None)?;
        let device: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let features = device
            .get("features")
            .and_then(|v| v.as_array())
            .ok_or("YAM device has no feature list")?;
        let has = |feature| features.iter().any(|v| v.as_str() == Some(feature));
        if !has("led.ring.rgb") {
            return Err("YAM does not report an RGB ring".into());
        }
        let ring = device
            .get("led_count")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .or_else(|| {
                let counts: Vec<usize> = features
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(|v| v.strip_prefix("led.ring."))
                    .filter_map(|v| v.parse::<usize>().ok())
                    .collect();
                (counts.len() == 1).then_some(counts[0])
            })
            .ok_or("YAM does not report a numeric LED count")?;
        if !(1..=4096).contains(&ring) {
            return Err(format!(
                "unsupported or inconsistent YAM RGB ring count: {ring}"
            ));
        }
        self.ring_profile_confirmed = true;
        Ok(Layout {
            ring,
            logo: ring == 24 && has("led.logo.rgb"),
        })
    }
    fn frame(&self, colors: Vec<String>) -> Result<(), String> {
        let json = serde_json::to_string(&Frame { colors }).map_err(|e| e.to_string())?;
        self.call("POST", "/led", Some(&json))?;
        Ok(())
    }
    fn release(&self) -> Result<(), String> {
        self.call("POST", "/led/off", Some("{}")).map(|_| ())
    }
    fn logo_frame(&mut self, colors: Vec<String>) -> Result<(), String> {
        let json = serde_json::to_string(&Frame { colors }).map_err(|e| e.to_string())?;
        self.call("POST", "/logo", Some(&json))?;
        self.logo_active = true;
        Ok(())
    }
    fn release_logo(&mut self) -> Result<(), String> {
        if self.logo_active {
            self.call("POST", "/logo/off", Some("{}"))?;
            self.logo_active = false;
        }
        Ok(())
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        let _ = self.release_logo();
        if self.ring_profile_confirmed {
            let _ = self.release();
        }
    }
}

fn parse_packet(packet: &[u8], max: usize) -> Option<Vec<[u8; 3]>> {
    if packet.is_empty() || packet.len() % 3 != 0 || packet.len() > max * 3 {
        return None;
    }
    Some(packet.chunks_exact(3).map(|x| [x[0], x[1], x[2]]).collect())
}
fn sample(input: &[[u8; 3]], pos: f32) -> [f32; 3] {
    if input.len() == 1 {
        return input[0].map(|x| x as f32);
    }
    let x = pos.rem_euclid(1.0) * input.len() as f32;
    let lo = x.floor() as usize % input.len();
    let hi = (lo + 1) % input.len();
    let t = x.fract();
    std::array::from_fn(|c| input[lo][c] as f32 * (1.0 - t) + input[hi][c] as f32 * t)
}
fn sample_area(input: &[[u8; 3]], start: f32, width: f32) -> [f32; 3] {
    let from = start * input.len() as f32;
    let to = (start + width) * input.len() as f32;
    let mut sum = [0.0; 3];
    for bin in from.floor() as usize..to.ceil() as usize {
        let overlap = (to.min((bin + 1) as f32) - from.max(bin as f32)).max(0.0);
        if overlap > 0.0 {
            for channel in 0..3 {
                sum[channel] += input[bin % input.len()][channel] as f32 * overlap;
            }
        }
    }
    sum.map(|value| value / (to - from))
}
fn correct(rgb: [f32; 3], c: &Config) -> String {
    let gains = [c.red_gain, c.green_gain, c.blue_gain];
    let out: [u8; 3] = std::array::from_fn(|i| {
        ((rgb[i] / 255.0).powf(c.gamma) * c.brightness * gains[i] * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    });
    let order = c.channel_order.as_bytes();
    let pick = |b| {
        out[match b {
            b'R' => 0,
            b'G' => 1,
            _ => 2,
        }]
    };
    format!(
        "#{:02X}{:02X}{:02X}",
        pick(order[0]),
        pick(order[1]),
        pick(order[2])
    )
}
fn map_frame(input: &[[u8; 3]], layout: &Layout, c: &Config) -> Vec<String> {
    let mut out = Vec::with_capacity(layout.ring);
    for i in 0..layout.ring {
        let index = if c.ring_reverse {
            layout.ring - 1 - i
        } else {
            i
        };
        let pos = ((index + c.ring_offset % layout.ring) % layout.ring) as f32 / layout.ring as f32;
        let rgb = if input.len() > layout.ring {
            sample_area(input, pos, 1.0 / layout.ring as f32)
        } else {
            sample(input, pos)
        };
        out.push(correct(rgb, c));
    }
    out
}
fn map_logo(input: &[[u8; 3]], c: &Config) -> Vec<String> {
    let average: [f32; 3] = std::array::from_fn(|channel| {
        input.iter().map(|rgb| rgb[channel] as f32).sum::<f32>() / input.len() as f32
    });
    (0..6)
        .map(|index| {
            let rgb = if c.logo_mode == "zones" {
                sample_area(input, index as f32 / 6.0, 1.0 / 6.0)
            } else {
                average
            };
            correct(rgb, c)
        })
        .collect()
}
fn test_logo(c: &Config, step: usize) -> Vec<String> {
    let mut colors = vec!["#000000".to_string(); 6];
    let palette = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    if c.test_mode == "rgb" {
        colors.fill(correct(palette[step % 3].map(|x| x as f32), c));
    } else {
        colors[step % 6] = correct([255.0; 3], c);
    }
    colors
}
fn test_frame(layout: &Layout, c: &Config, step: usize) -> Vec<String> {
    let mut colors = vec!["#000000".to_string(); layout.ring];
    let palette = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    if c.test_mode == "rgb" {
        for color in &mut colors {
            *color = correct(palette[step % 3].map(|x| x as f32), c);
        }
    } else {
        let index = step % colors.len();
        colors[index] = correct([255.0, 255.0, 255.0], c);
    }
    colors
}
fn run() -> Result<(), String> {
    let cfg: Config =
        toml::from_str(&fs::read_to_string("config.toml").map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    cfg.validate()?;
    let mut api = Api::new()?;
    let api_wait_started = Instant::now();
    let layout = loop {
        match api.layout() {
            Ok(layout) => break layout,
            Err(error)
                if error == "YAM does not report an RGB ring"
                    || error == "YAM does not report a numeric LED count"
                    || error.starts_with("unsupported or inconsistent YAM RGB ring count:") =>
            {
                return Err(error);
            }
            Err(error) if api_wait_started.elapsed() >= Duration::from_secs(10) => {
                return Err(format!("YAM runtime API unavailable: {error}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let mut logo_enabled = layout.logo && cfg.logo_mode != "off";
    let socket = UdpSocket::bind(&cfg.bind).map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_millis(20)))
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; cfg.max_input_blocks * 3 + 1];
    let mut last_rx = Instant::now();
    let mut last_tx = Instant::now() - Duration::from_secs(1);
    let mut latest: Option<Vec<[u8; 3]>> = None;
    let mut active = false;
    let test_start = Instant::now();
    loop {
        if cfg.test_mode == "off" {
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => {
                    if let Some(frame) = parse_packet(&buf[..n], cfg.max_input_blocks) {
                        latest = Some(frame);
                        last_rx = Instant::now();
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    ()
                }
                Err(e) => return Err(e.to_string()),
            }
            socket.set_nonblocking(true).map_err(|e| e.to_string())?;
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((n, _)) => {
                        if let Some(frame) = parse_packet(&buf[..n], cfg.max_input_blocks) {
                            latest = Some(frame);
                            last_rx = Instant::now();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.to_string()),
                }
            }
            socket.set_nonblocking(false).map_err(|e| e.to_string())?;
        }
        let interval = Duration::from_secs_f32(1.0 / cfg.frame_rate as f32);
        if last_tx.elapsed() >= interval {
            let frames = if cfg.test_mode != "off" {
                let step = (test_start.elapsed().as_millis() / 500) as usize;
                Some((test_frame(&layout, &cfg, step), test_logo(&cfg, step)))
            } else {
                latest
                    .take()
                    .map(|f| (map_frame(&f, &layout, &cfg), map_logo(&f, &cfg)))
            };
            if let Some((ring, logo)) = frames {
                api.frame(ring)?;
                if logo_enabled {
                    if let Err(error) = api.logo_frame(logo) {
                        eprintln!("ya-ambilight-universal: logo API unavailable: {error}; continuing with ring");
                        logo_enabled = false;
                    }
                }
                active = true;
                last_tx = Instant::now();
            }
        }
        if active
            && cfg.test_mode == "off"
            && last_rx.elapsed() >= Duration::from_millis(cfg.stream_timeout_ms)
        {
            if cfg.loss == "black" {
                api.frame(vec!["#000000".into(); layout.ring])?;
                if logo_enabled {
                    if let Err(error) = api.logo_frame(vec!["#000000".into(); 6]) {
                        eprintln!("ya-ambilight-universal: logo API unavailable: {error}; continuing with ring");
                        logo_enabled = false;
                    }
                }
            } else {
                api.release()?;
                api.release_logo()?;
            }
            active = false;
            latest = None;
        }
        if cfg.test_mode != "off" {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("ya-ambilight-universal: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packets() {
        assert!(parse_packet(&[], 490).is_none());
        assert!(parse_packet(&[1, 2], 490).is_none());
        assert!(parse_packet(&[1, 2, 3], 1).is_some());
        assert!(parse_packet(&[1, 2, 3, 4, 5, 6], 1).is_none());
    }
    #[test]
    fn one_color_reaches_all_ring_zones() {
        let c = Config::default();
        for ring in [1, 3, 12, 24] {
            let l = Layout { ring, logo: false };
            let out = map_frame(&[[255, 0, 0]], &l, &c);
            assert_eq!(out.len(), ring);
            assert!(out.iter().all(|x| x == &out[0]));
        }
    }
    #[test]
    fn full_perimeter_is_sampled() {
        let c = Config::default();
        let l = Layout {
            ring: 24,
            logo: true,
        };
        let out = map_frame(
            &[[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]],
            &l,
            &c,
        );
        assert_ne!(out[0], out[6]);
        assert_ne!(out[0], out[12]);
    }
    #[test]
    fn interpolation_uses_entire_input_and_wraps() {
        let input = [[255, 0, 0], [0, 255, 0]];
        let middle = sample(&input, 0.25);
        assert_eq!(middle, [127.5, 127.5, 0.0]);
        let seam = sample(&input, 0.75);
        assert_eq!(seam, [127.5, 127.5, 0.0]);
    }
    #[test]
    fn offset_wraps_to_actual_ring_size() {
        let mut c = Config::default();
        c.ring_offset = 24;
        let l = Layout {
            ring: 3,
            logo: false,
        };
        let input = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
        assert_eq!(
            map_frame(&input, &l, &c),
            map_frame(&input, &l, &Config::default())
        );
        c.ring_offset = 1;
        assert_eq!(
            map_frame(&input, &l, &c)[0],
            map_frame(&input, &l, &Config::default())[1]
        );
    }
    #[test]
    fn smaller_ring_averages_all_input_zones() {
        let mut c = Config::default();
        c.brightness = 1.0;
        let input = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 0],
            [0, 0, 0],
            [0, 0, 0],
        ];
        assert_eq!(
            map_frame(
                &input,
                &Layout {
                    ring: 3,
                    logo: false
                },
                &c
            ),
            ["#808000", "#808080", "#000000"]
        );
    }
    #[test]
    fn logo_has_six_average_colors() {
        let mut c = Config::default();
        c.brightness = 1.0;
        let output = map_logo(&[[255, 0, 0], [0, 0, 255]], &c);
        assert_eq!(output, vec!["#800080"; 6]);
    }
}
