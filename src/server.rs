//! Asks a DayZ server about itself over its query port.
//!
//! Servers answer Steam's public `A2S_INFO` query, the one server browsers use.
//! DayZ puts the in-game time in the reply's keywords, for example
//! `battleye,shard001,etm3.290000,entm3.290000,07:45`, where `etm` and `entm`
//! are how fast time runs by day and by night.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use flume::{Receiver, Sender};

const REQUEST: &[u8] = b"\xff\xff\xff\xffTSource Engine Query\0";
const SINGLE_PACKET: &[u8] = &[0xff; 4];
const INFO_REPLY: u8 = b'I';
const CHALLENGE_REPLY: u8 = b'A';
const CHALLENGE_LENGTH: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_REPLY_BYTES: usize = 1400;

/// The running clock carries the time between polls, so these can be slow.
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const STOP_CHECK: Duration = Duration::from_millis(100);

const SECONDS_PER_MINUTE: f64 = 60.0;
const SECONDS_PER_DAY: f64 = 86_400.0;
/// Speed to assume for a server that doesn't report one.
const DEFAULT_SPEED: f64 = 1.0;
/// How old a reported time can be, in real seconds. Servers were measured
/// refreshing it every 25 to 60 seconds.
const MAX_REPORT_AGE: f64 = 70.0;

const EXTRA_GAME_PORT: u8 = 0x80;
const EXTRA_STEAM_ID: u8 = 0x10;
const EXTRA_SPECTATOR: u8 = 0x40;
const EXTRA_KEYWORDS: u8 = 0x20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameTime {
    pub hour: u8,
    pub minute: u8,
}

impl GameTime {
    fn seconds(self) -> f64 {
        f64::from(self.hour) * 3_600.0 + f64::from(self.minute) * SECONDS_PER_MINUTE
    }
}

impl std::fmt::Display for GameTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    pub name: String,
    pub map: String,
    pub version: String,
    pub players: u8,
    pub max_players: u8,
    pub time: Option<GameTime>,
    /// How many times faster than real time the day runs.
    pub day_speed: Option<f32>,
    pub night_speed: Option<f32>,
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8]> {
        if self.bytes.len() < count {
            bail!("the server's reply was cut short");
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn text(&mut self) -> Result<String> {
        let end = self
            .bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| anyhow!("the server's reply has an unterminated field"))?;
        let text = String::from_utf8_lossy(&self.bytes[..end]).into_owned();
        self.bytes = &self.bytes[end + 1..];
        Ok(text)
    }
}

fn parse_time(tag: &str) -> Option<GameTime> {
    let (hour, minute) = tag.split_once(':')?;
    let is_digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !is_digits(hour) || minute.len() != 2 || !is_digits(minute) {
        return None;
    }
    let (hour, minute): (u8, u8) = (hour.parse().ok()?, minute.parse().ok()?);
    (hour < 24 && minute < 60).then_some(GameTime { hour, minute })
}

/// Parses an `A2S_INFO` reply, including its packet header.
pub fn parse_info(packet: &[u8]) -> Result<ServerInfo> {
    let body = packet
        .strip_prefix(SINGLE_PACKET)
        .context("the server sent an unexpected packet")?;
    let mut reader = Reader { bytes: body };
    if reader.byte()? != INFO_REPLY {
        bail!("the server did not send server information");
    }
    reader.take(1)?; // Protocol version.
    let name = reader.text()?;
    let map = reader.text()?;
    reader.text()?; // Folder.
    reader.text()?; // Game.
    reader.take(2)?; // App ID.
    let players = reader.byte()?;
    let max_players = reader.byte()?;
    reader.take(5)?; // Bots, server type, OS, visibility and VAC.
    let version = reader.text()?;

    let mut keywords = String::new();
    if let Ok(extras) = reader.byte() {
        if extras & EXTRA_GAME_PORT != 0 {
            reader.take(2)?;
        }
        if extras & EXTRA_STEAM_ID != 0 {
            reader.take(8)?;
        }
        if extras & EXTRA_SPECTATOR != 0 {
            reader.take(2)?;
            reader.text()?;
        }
        if extras & EXTRA_KEYWORDS != 0 {
            keywords = reader.text()?;
        }
    }

    let tags = || keywords.split(',').map(str::trim);
    let speed = |prefix: &str| {
        tags()
            .find_map(|tag| tag.strip_prefix(prefix)?.parse::<f32>().ok())
            .filter(|speed| speed.is_finite() && *speed > 0.0)
    };
    Ok(ServerInfo {
        name,
        map,
        version,
        players,
        max_players,
        time: tags().find_map(parse_time),
        day_speed: speed("etm"),
        night_speed: speed("entm"),
    })
}

fn resolve(address: &str) -> Result<SocketAddr> {
    let usage = || format!("enter the server as address:query port, not {address:?}");
    address
        .trim()
        .to_socket_addrs()
        .with_context(usage)?
        .next()
        .with_context(usage)
}

/// Asks a server for its information once.
pub fn query(address: &str) -> Result<ServerInfo> {
    let target = resolve(address)?;
    let local: SocketAddr = if target.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        ([0u16; 8], 0).into()
    };
    let socket = UdpSocket::bind(local).context("opening a network socket")?;
    socket.connect(target).context("reaching the server")?;
    socket.set_read_timeout(Some(TIMEOUT))?;

    let receive = |buffer: &mut [u8]| -> Result<usize> {
        socket.recv(buffer).map_err(|_| {
            anyhow!("no answer from {target}. Check it's the query port, not the game port")
        })
    };

    let mut buffer = [0; MAX_REPLY_BYTES];
    socket.send(REQUEST)?;
    let mut length = receive(&mut buffer)?;

    // Servers answer with a challenge first, to be repeated in a second request.
    let challenge_end = SINGLE_PACKET.len() + 1 + CHALLENGE_LENGTH;
    if length >= challenge_end && buffer[SINGLE_PACKET.len()] == CHALLENGE_REPLY {
        let mut request = REQUEST.to_vec();
        request.extend_from_slice(&buffer[SINGLE_PACKET.len() + 1..challenge_end]);
        socket.send(&request)?;
        length = receive(&mut buffer)?;
    }
    parse_info(&buffer[..length])
}

/// The server's time, run forward between polls at the server's clock speed.
///
/// A reported time is whole minutes and can be up to a minute of real time old,
/// so it's never ahead of the true time but can be several game minutes behind.
/// A running time that fits that is left alone. Anything else means the clock
/// or the server changed, and the clock goes to the report. Because reports can
/// only pull it forward within that range, the clock settles on the freshest
/// ones and doesn't jump back to each stale one.
#[derive(Clone, Debug, PartialEq)]
pub struct GameClock {
    /// Game seconds since midnight at `anchor`.
    seconds: f64,
    anchor: Instant,
    speed: f64,
    last_report: (Instant, GameTime),
}

impl GameClock {
    /// Starts a clock from a server's reply, if it reported a time.
    pub fn start(now: Instant, server: &ServerInfo) -> Option<Self> {
        let time = server.time?;
        let speed = server
            .day_speed
            .or(server.night_speed)
            .map_or(DEFAULT_SPEED, f64::from);
        Some(Self {
            seconds: time.seconds(),
            anchor: now,
            speed,
            last_report: (now, time),
        })
    }

    /// Game seconds since midnight.
    fn seconds_at(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.anchor).as_secs_f64();
        (self.seconds + elapsed * self.speed).rem_euclid(SECONDS_PER_DAY)
    }

    /// The time as `HH:MM:SS`.
    pub fn text(&self, now: Instant) -> String {
        let seconds = self.seconds_at(now) as u32;
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3_600,
            seconds / 60 % 60,
            seconds % 60
        )
    }

    /// How fast the clock is running, in game seconds per real second.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Corrects the clock from a new reply. Returns how far the time moved, in
    /// game seconds.
    pub fn correct(&mut self, now: Instant, server: &ServerInfo) -> f64 {
        let running = self.seconds_at(now);
        let speed = self.choose_speed(now, server);
        let Some(time) = server.time else {
            self.seconds = running;
            self.anchor = now;
            self.speed = speed;
            return 0.0;
        };

        let reported = time.seconds();
        let ahead = (running - reported + SECONDS_PER_DAY / 2.0).rem_euclid(SECONDS_PER_DAY)
            - SECONDS_PER_DAY / 2.0;
        let expected_lag = SECONDS_PER_MINUTE + MAX_REPORT_AGE * speed;
        let corrected = if (0.0..expected_lag).contains(&ahead) {
            running
        } else {
            reported
        };

        self.seconds = corrected.rem_euclid(SECONDS_PER_DAY);
        self.anchor = now;
        self.speed = speed;
        if time != self.last_report.1 {
            self.last_report = (now, time);
        }
        corrected - running
    }

    /// Picks the speed to run at. Servers don't say when night starts, so when
    /// day and night differ, the one closest to how far the time really moved
    /// since the last report is the one in force.
    fn choose_speed(&self, now: Instant, server: &ServerInfo) -> f64 {
        let (day, night) = match (server.day_speed, server.night_speed) {
            (Some(day), Some(night)) => (f64::from(day), f64::from(night)),
            (Some(speed), None) | (None, Some(speed)) => return f64::from(speed),
            (None, None) => return self.speed,
        };
        let (Some(time), (reported_at, reported)) = (server.time, self.last_report) else {
            return self.speed;
        };
        let real_seconds = now.saturating_duration_since(reported_at).as_secs_f64();
        if day == night || real_seconds <= 0.0 {
            return day;
        }
        let moved = (time.seconds() - reported.seconds()).rem_euclid(SECONDS_PER_DAY);
        if moved == 0.0 {
            return self.speed;
        }
        let observed = moved / real_seconds;
        if (observed - day).abs() <= (observed - night).abs() {
            day
        } else {
            night
        }
    }
}

/// Keeps asking a server for its information until dropped.
pub struct ServerWatch {
    stop: Arc<AtomicBool>,
}

impl ServerWatch {
    pub fn start(address: String) -> (Self, Receiver<Result<ServerInfo, String>>) {
        let (results, receiver) = flume::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        std::thread::spawn(move || watch(&address, &worker_stop, &results));
        (Self { stop }, receiver)
    }
}

impl Drop for ServerWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch(address: &str, stop: &AtomicBool, results: &Sender<Result<ServerInfo, String>>) {
    while !stop.load(Ordering::Relaxed) {
        let result = query(address).map_err(|error| format!("{error:#}"));
        if stop.load(Ordering::Relaxed) || results.send(result).is_err() {
            return;
        }
        let asked = Instant::now();
        while asked.elapsed() < POLL_INTERVAL {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(STOP_CHECK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(extras: u8, keywords: &str) -> Vec<u8> {
        let mut packet = SINGLE_PACKET.to_vec();
        packet.extend([INFO_REPLY, 17]);
        for field in [
            "Test server",
            "nasdara",
            "dayz",
            "Official DayZ game server",
        ] {
            packet.extend(field.as_bytes());
            packet.push(0);
        }
        packet.extend([0, 0]); // App ID.
        packet.extend([42, 60, 0, b'd', b'w', 0, 1]);
        packet.extend(b"1.30.164289\0");
        packet.push(extras);
        if extras & EXTRA_GAME_PORT != 0 {
            packet.extend(11600u16.to_le_bytes());
        }
        if extras & EXTRA_STEAM_ID != 0 {
            packet.extend(90296808731405323u64.to_le_bytes());
        }
        if extras & EXTRA_SPECTATOR != 0 {
            packet.extend(1u16.to_le_bytes());
            packet.extend(b"spectators\0");
        }
        if extras & EXTRA_KEYWORDS != 0 {
            packet.extend(keywords.as_bytes());
            packet.push(0);
        }
        // The game ID that follows is not read, and must not get in the way.
        packet.extend(221100u64.to_le_bytes());
        packet
    }

    const DAYZ_EXTRAS: u8 = 0xb1;
    const DAYZ_TAGS: &str = "battleye,shard001,lqs6,etm3.290000,entm3.290000,isDLC4,07:45";

    #[test]
    fn reads_the_time_and_details_from_a_dayz_reply() {
        let info = parse_info(&reply(DAYZ_EXTRAS, DAYZ_TAGS)).unwrap();
        assert_eq!(info.name, "Test server");
        assert_eq!(info.map, "nasdara");
        assert_eq!(info.version, "1.30.164289");
        assert_eq!((info.players, info.max_players), (42, 60));
        assert_eq!(info.time.unwrap().to_string(), "07:45");
        assert_eq!(info.day_speed, Some(3.29));
        assert_eq!(info.night_speed, Some(3.29));
    }

    #[test]
    fn day_and_night_speeds_are_told_apart() {
        let tags = "entm12.000000,etm2.000000,23:05";
        let info = parse_info(&reply(EXTRA_KEYWORDS, tags)).unwrap();
        assert_eq!(info.day_speed, Some(2.0));
        assert_eq!(info.night_speed, Some(12.0));
    }

    #[test]
    fn every_optional_field_is_skipped_correctly() {
        let packet = reply(
            EXTRA_GAME_PORT | EXTRA_STEAM_ID | EXTRA_SPECTATOR | EXTRA_KEYWORDS,
            "9:05",
        );
        let time = parse_info(&packet).unwrap().time.unwrap();
        assert_eq!(time, GameTime { hour: 9, minute: 5 });
        assert_eq!(time.to_string(), "09:05");
    }

    #[test]
    fn a_server_without_a_time_tag_has_no_time() {
        let info = parse_info(&reply(DAYZ_EXTRAS, "battleye,no3rd")).unwrap();
        assert_eq!(info.time, None);
        assert_eq!(info.day_speed, None);
        assert_eq!(parse_info(&reply(0, "")).unwrap().time, None);
    }

    #[test]
    fn only_a_real_clock_time_counts() {
        for tag in ["24:00", "12:60", "7:5", "ab:cd", ":30", "12:", "1:2:3"] {
            assert_eq!(parse_time(tag), None, "{tag}");
        }
        assert_eq!(parse_time("00:00"), Some(GameTime { hour: 0, minute: 0 }));
        assert_eq!(
            parse_time("23:59"),
            Some(GameTime {
                hour: 23,
                minute: 59
            })
        );
    }

    #[test]
    fn damaged_replies_are_errors_not_panics() {
        let whole = reply(DAYZ_EXTRAS, DAYZ_TAGS);
        for length in 0..whole.len() - 8 {
            let _ = parse_info(&whole[..length]);
        }
        assert!(parse_info(b"not a server").is_err());
        let mut wrong_type = whole.clone();
        wrong_type[4] = b'X';
        assert!(parse_info(&wrong_type).is_err());
    }

    fn server_at(hour: u8, minute: u8, day: f32, night: f32) -> ServerInfo {
        ServerInfo {
            name: String::new(),
            map: String::new(),
            version: String::new(),
            players: 0,
            max_players: 0,
            time: Some(GameTime { hour, minute }),
            day_speed: Some(day),
            night_speed: Some(night),
        }
    }

    fn after(start: Instant, seconds: f64) -> Instant {
        start + Duration::from_secs_f64(seconds)
    }

    #[test]
    fn the_clock_runs_at_the_server_speed() {
        let start = Instant::now();
        let clock = GameClock::start(start, &server_at(7, 45, 3.25, 3.25)).unwrap();
        assert_eq!(clock.text(start), "07:45:00");
        assert_eq!(clock.text(after(start, 10.0)), "07:45:32");
        assert_eq!(clock.text(after(start, 100.0)), "07:50:25");
    }

    #[test]
    fn the_clock_wraps_at_midnight() {
        let start = Instant::now();
        let clock = GameClock::start(start, &server_at(23, 59, 1.0, 1.0)).unwrap();
        assert_eq!(clock.text(after(start, 59.0)), "23:59:59");
        assert_eq!(clock.text(after(start, 61.0)), "00:00:01");
    }

    #[test]
    fn a_running_time_inside_the_reported_minute_is_left_alone() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(10, 0, 3.0, 3.0)).unwrap();
        let later = after(start, 60.0);
        assert_eq!(clock.text(later), "10:03:00");
        assert_eq!(clock.correct(later, &server_at(10, 3, 3.0, 3.0)), 0.0);
        assert_eq!(clock.text(after(start, 70.0)), "10:03:30");
    }

    #[test]
    fn a_clock_running_slow_jumps_to_the_start_of_the_reported_minute() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(10, 0, 3.0, 3.0)).unwrap();
        let later = after(start, 60.0);
        clock.correct(later, &server_at(10, 5, 3.0, 3.0));
        assert_eq!(clock.text(later), "10:05:00");
    }

    #[test]
    fn a_stale_report_does_not_pull_the_clock_back() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(10, 0, 3.0, 3.0)).unwrap();
        let later = after(start, 120.0);
        assert_eq!(clock.text(later), "10:06:00");
        // A report 60 real seconds old, three game minutes behind.
        assert_eq!(clock.correct(later, &server_at(10, 3, 3.0, 3.0)), 0.0);
        assert_eq!(clock.text(later), "10:06:00");
    }

    #[test]
    fn a_clock_too_far_ahead_for_a_stale_report_goes_to_the_report() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(10, 0, 3.0, 3.0)).unwrap();
        let later = after(start, 120.0);
        clock.correct(later, &server_at(9, 30, 3.0, 3.0));
        assert_eq!(clock.text(later), "09:30:00");
    }

    #[test]
    fn repeated_stale_reports_settle_on_the_freshest_one() {
        // True time starts at 10:00:40 and runs at 3x, with reports up to a
        // minute old, as measured on a real server.
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(10, 0, 3.0, 3.0)).unwrap();
        let true_seconds = |real: f64| 40.0 + real * 3.0;
        let mut moves = Vec::new();
        for (poll, age) in [
            (60.0, 50.0),
            (120.0, 5.0),
            (180.0, 40.0),
            (240.0, 20.0),
            (300.0, 55.0),
        ] {
            let reported = (true_seconds(poll - age) / 60.0) as u8;
            moves.push(clock.correct(after(start, poll), &server_at(10, reported, 3.0, 3.0)));
        }
        let (first, rest) = moves.split_at(2);
        assert!(first.iter().all(|moved| *moved >= 0.0), "{moves:?}");
        assert!(rest.iter().all(|moved| *moved == 0.0), "{moves:?}");
        let real = after(start, 300.0);
        let expected = 36_000.0 + true_seconds(300.0);
        assert!((clock.seconds_at(real) - expected).abs() < 60.0);
    }

    #[test]
    fn correction_works_across_midnight() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(23, 59, 1.0, 1.0)).unwrap();
        let later = after(start, 30.0);
        assert_eq!(clock.text(later), "23:59:30");
        clock.correct(later, &server_at(0, 2, 1.0, 1.0));
        assert_eq!(clock.text(later), "00:02:00");
    }

    #[test]
    fn a_new_speed_takes_over_from_the_correction() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(10, 0, 3.0, 3.0)).unwrap();
        let later = after(start, 60.0);
        clock.correct(later, &server_at(10, 3, 6.0, 6.0));
        assert_eq!(clock.speed(), 6.0);
        assert_eq!(clock.text(after(start, 70.0)), "10:04:00");
    }

    #[test]
    fn the_speed_in_force_is_worked_out_when_day_and_night_differ() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(20, 0, 2.0, 12.0)).unwrap();
        // Twelve game minutes in one real minute: it's night.
        clock.correct(after(start, 60.0), &server_at(20, 12, 2.0, 12.0));
        assert_eq!(clock.speed(), 12.0);
        // Two game minutes in the next: it's day again.
        clock.correct(after(start, 120.0), &server_at(20, 14, 2.0, 12.0));
        assert_eq!(clock.speed(), 2.0);
        // A report that hasn't refreshed yet says nothing about the speed.
        clock.correct(after(start, 150.0), &server_at(20, 14, 2.0, 12.0));
        assert_eq!(clock.speed(), 2.0);
    }

    #[test]
    fn a_reply_without_a_time_leaves_the_clock_running() {
        let start = Instant::now();
        let mut clock = GameClock::start(start, &server_at(10, 0, 3.0, 3.0)).unwrap();
        let mut silent = server_at(0, 0, 3.0, 3.0);
        silent.time = None;
        clock.correct(after(start, 60.0), &silent);
        assert_eq!(clock.text(after(start, 60.0)), "10:03:00");
        assert_eq!(GameClock::start(start, &silent), None);
    }

    #[test]
    fn a_challenge_is_answered_with_a_second_request() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(TIMEOUT)).unwrap();
        let address = server.local_addr().unwrap().to_string();
        let client = std::thread::spawn(move || query(&address));

        let mut buffer = [0; 64];
        let (length, from) = server.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[..length], REQUEST);
        server
            .send_to(b"\xff\xff\xff\xffA\x01\x02\x03\x04", from)
            .unwrap();

        let (length, from) = server.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[length - 4..length], [1, 2, 3, 4]);
        assert_eq!(&buffer[..length - 4], REQUEST);
        server
            .send_to(&reply(DAYZ_EXTRAS, DAYZ_TAGS), from)
            .unwrap();

        let info = client.join().unwrap().unwrap();
        assert_eq!(
            info.time,
            Some(GameTime {
                hour: 7,
                minute: 45
            })
        );
    }

    #[test]
    fn an_address_needs_a_port() {
        let error = query("127.0.0.1").unwrap_err();
        assert!(format!("{error:#}").contains("query port"), "{error:#}");
    }

    #[test]
    fn a_silent_port_says_what_to_check() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = silent.local_addr().unwrap().to_string();
        let error = query(&address).unwrap_err();
        assert!(format!("{error:#}").contains("query port"), "{error:#}");
    }
}
