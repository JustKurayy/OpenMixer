use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::{IpAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const PAIRING_LIFETIME: Duration = Duration::from_secs(60);
const MAX_PAIRING_ATTEMPTS: u8 = 5;
const AUDIO_QUEUE_FRAMES: usize = 9_600;
const CLIENT_QUEUE_BLOCKS: usize = 8;
const FRAMES_PER_BLOCK: usize = 960;

#[derive(Clone)]
pub struct AudioTap {
    sender: SyncSender<[i16; 2]>,
}

impl AudioTap {
    pub fn push(&self, frame: [f32; 2]) -> bool {
        self.sender
            .try_send(frame.map(float_to_pcm))
            .is_ok()
    }
}

fn float_to_pcm(value: f32) -> i16 {
    if value <= -1.0 {
        i16::MIN
    } else {
        (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
    }
}

fn audio_channel(capacity: usize) -> (AudioTap, Receiver<[i16; 2]>) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    (AudioTap { sender }, receiver)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDevice {
    pub address: IpAddr,
    pub code: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LanSnapshot {
    pub running: bool,
    pub url: Option<String>,
    pub pending: Option<PendingDevice>,
    pub clients: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct PairRequest {
    id: String,
    code: u8,
    peer: IpAddr,
    created: Instant,
    attempts: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairingError {
    UnknownRequest,
    Expired,
    WrongCode,
    TooManyAttempts,
}

#[derive(Default)]
struct AuthState {
    pending: HashMap<String, PairRequest>,
    sessions: HashSet<String>,
}

impl AuthState {
    fn begin_pairing(&mut self, peer: IpAddr, now: Instant) -> Result<PairRequest, String> {
        self.pending
            .retain(|_, request| now.duration_since(request.created) <= PAIRING_LIFETIME);
        if let Some(request) = self.pending.values().find(|request| request.peer == peer) {
            return Ok(request.clone());
        }

        let mut random = [0_u8; 17];
        getrandom::fill(&mut random).map_err(|error| format!("Random generator failed: {error}"))?;
        let request = PairRequest {
            id: hex(&random[1..]),
            code: 10 + random[0] % 90,
            peer,
            created: now,
            attempts: 0,
        };
        self.pending.insert(request.id.clone(), request.clone());
        Ok(request)
    }

    fn verify(
        &mut self,
        id: &str,
        code: u8,
        now: Instant,
    ) -> Result<String, PairingError> {
        let Some(request) = self.pending.get_mut(id) else {
            return Err(PairingError::UnknownRequest);
        };
        if now.duration_since(request.created) > PAIRING_LIFETIME {
            self.pending.remove(id);
            return Err(PairingError::Expired);
        }
        if request.code != code {
            request.attempts += 1;
            if request.attempts >= MAX_PAIRING_ATTEMPTS {
                self.pending.remove(id);
                return Err(PairingError::TooManyAttempts);
            }
            return Err(PairingError::WrongCode);
        }

        self.pending.remove(id);
        let mut random = [0_u8; 24];
        if getrandom::fill(&mut random).is_err() {
            return Err(PairingError::UnknownRequest);
        }
        let token = hex(&random);
        self.sessions.insert(token.clone());
        Ok(token)
    }

    fn is_authorized(&self, token: &str) -> bool {
        !token.is_empty() && self.sessions.contains(token)
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    value
}

struct SharedState {
    snapshot: LanSnapshot,
    auth: AuthState,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            snapshot: LanSnapshot::default(),
            auth: AuthState::default(),
        }
    }
}

type AudioBlock = Arc<[u8]>;
type ClientList = Arc<Mutex<Vec<SyncSender<AudioBlock>>>>;

pub struct LanServer {
    shared: Arc<Mutex<SharedState>>,
    clients: ClientList,
    stop: Option<Arc<AtomicBool>>,
    listener_thread: Option<JoinHandle<()>>,
}

impl Default for LanServer {
    fn default() -> Self {
        Self::new().0
    }
}

impl LanServer {
    pub fn new() -> (Self, AudioTap) {
        let (tap, receiver) = audio_channel(AUDIO_QUEUE_FRAMES);
        let shared = Arc::new(Mutex::new(SharedState::default()));
        let clients = Arc::new(Mutex::new(Vec::new()));
        spawn_audio_dispatch(receiver, Arc::clone(&clients));
        (
            Self {
                shared,
                clients,
                stop: None,
                listener_thread: None,
            },
            tap,
        )
    }

    pub fn start(&mut self, sample_rate: u32) -> Result<(), String> {
        self.stop();
        let listener = TcpListener::bind(("0.0.0.0", 0))
            .map_err(|error| format!("Could not start LAN listener: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("Could not configure LAN listener: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let address = local_ipv4().ok_or("Could not determine this computer's LAN address")?;
        let url = format!("http://{address}:{port}");
        let stop = Arc::new(AtomicBool::new(false));

        if let Ok(mut state) = self.shared.lock() {
            state.auth = AuthState::default();
            state.snapshot = LanSnapshot {
                running: true,
                url: Some(url),
                ..LanSnapshot::default()
            };
        }

        let shared = Arc::clone(&self.shared);
        let clients = Arc::clone(&self.clients);
        let thread_stop = Arc::clone(&stop);
        self.listener_thread = Some(thread::spawn(move || {
            run_listener(listener, sample_rate, shared, clients, thread_stop)
        }));
        self.stop = Some(stop);
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Release);
        }
        if let Some(thread) = self.listener_thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut clients) = self.clients.lock() {
            clients.clear();
        }
        if let Ok(mut state) = self.shared.lock() {
            state.auth = AuthState::default();
            state.snapshot = LanSnapshot::default();
        }
    }

    pub fn snapshot(&self) -> LanSnapshot {
        self.shared
            .lock()
            .map(|state| state.snapshot.clone())
            .unwrap_or_default()
    }
}

impl Drop for LanServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_audio_dispatch(receiver: Receiver<[i16; 2]>, clients: ClientList) {
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(FRAMES_PER_BLOCK * 4);
        while let Ok(frame) = receiver.recv() {
            bytes.extend_from_slice(&frame[0].to_le_bytes());
            bytes.extend_from_slice(&frame[1].to_le_bytes());
            if bytes.len() < FRAMES_PER_BLOCK * 4 {
                continue;
            }
            let block: AudioBlock = Arc::from(std::mem::take(&mut bytes));
            bytes = Vec::with_capacity(FRAMES_PER_BLOCK * 4);
            if let Ok(mut clients) = clients.lock() {
                clients.retain(|client| match client.try_send(Arc::clone(&block)) {
                    Ok(()) | Err(TrySendError::Full(_)) => true,
                    Err(TrySendError::Disconnected(_)) => false,
                });
            }
        }
    });
}

fn run_listener(
    listener: TcpListener,
    sample_rate: u32,
    shared: Arc<Mutex<SharedState>>,
    clients: ClientList,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let shared = Arc::clone(&shared);
                let clients = Arc::clone(&clients);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let _ = handle_connection(
                        stream,
                        peer.ip(),
                        sample_rate,
                        shared,
                        clients,
                        stop,
                    );
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                if let Ok(mut state) = shared.lock() {
                    state.snapshot.error = Some(format!("LAN listener failed: {error}"));
                }
                break;
            }
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: String,
    session: String,
}

fn handle_connection(
    mut stream: TcpStream,
    peer: IpAddr,
    sample_rate: u32,
    shared: Arc<Mutex<SharedState>>,
    clients: ClientList,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return send_text(&mut stream, "400 Bad Request", "text/plain", "Bad request", ""),
    };

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => {
            let mut state = shared.lock().unwrap();
            if state.auth.is_authorized(&request.session) {
                send_text(&mut stream, "200 OK", "text/html; charset=utf-8", PLAYER_HTML, "")
            } else {
                match state.auth.begin_pairing(peer, Instant::now()) {
                    Ok(pairing) => {
                        state.snapshot.pending = Some(PendingDevice {
                            address: peer,
                            code: pairing.code,
                        });
                        let page = pairing_page(&pairing.id, "");
                        send_text(&mut stream, "200 OK", "text/html; charset=utf-8", &page, "")
                    }
                    Err(error) => send_text(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        &error,
                        "",
                    ),
                }
            }
        }
        ("POST", "/pair") => {
            let fields = form_fields(&request.body);
            let id = fields.get("request").map(String::as_str).unwrap_or("");
            let code = fields
                .get("code")
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(0);
            let mut state = shared.lock().unwrap();
            match state.auth.verify(id, code, Instant::now()) {
                Ok(token) => {
                    state.snapshot.pending = None;
                    send_text(
                        &mut stream,
                        "303 See Other",
                        "text/plain",
                        "Paired",
                        &format!(
                            "Location: /\r\nSet-Cookie: openmixer_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400\r\n"
                        ),
                    )
                }
                Err(error) => {
                    if !state.auth.pending.contains_key(id) {
                        state.snapshot.pending = None;
                    }
                    let message = match error {
                        PairingError::WrongCode => "Wrong code",
                        PairingError::Expired => "Code expired; reload to request another",
                        PairingError::TooManyAttempts => "Too many attempts; reload to try again",
                        PairingError::UnknownRequest => "Pairing request is no longer valid",
                    };
                    let page = pairing_page(id, message);
                    send_text(&mut stream, "403 Forbidden", "text/html; charset=utf-8", &page, "")
                }
            }
        }
        ("GET", "/stream") => {
            let authorized = shared
                .lock()
                .map(|state| state.auth.is_authorized(&request.session))
                .unwrap_or(false);
            if !authorized {
                return send_text(&mut stream, "401 Unauthorized", "text/plain", "Pair first", "");
            }
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            )?;
            stream.write_all(&wav_header(sample_rate))?;
            let (sender, receiver) = mpsc::sync_channel(CLIENT_QUEUE_BLOCKS);
            clients.lock().unwrap().push(sender);
            if let Ok(mut state) = shared.lock() {
                state.snapshot.clients += 1;
            }
            while !stop.load(Ordering::Acquire) {
                match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(block) => {
                        if stream.write_all(&block).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if let Ok(mut state) = shared.lock() {
                state.snapshot.clients = state.snapshot.clients.saturating_sub(1);
            }
            Ok(())
        }
        ("GET", "/favicon.ico") => send_text(&mut stream, "204 No Content", "text/plain", "", ""),
        _ => send_text(&mut stream, "404 Not Found", "text/plain", "Not found", ""),
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    let header_end = loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() > 8_192 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = headers.lines();
    let mut request_line = lines.next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("").to_owned();
    let path = request_line
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .to_owned();
    if method.is_empty() || path.is_empty() {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut content_length = 0;
    let mut session = String::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().unwrap_or(0).min(1_024);
            } else if name.eq_ignore_ascii_case("cookie") {
                session = cookie_value(value, "openmixer_session").unwrap_or_default();
            }
        }
    }
    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body_end = (header_end + content_length).min(bytes.len());
    Ok(HttpRequest {
        method,
        path,
        body: String::from_utf8_lossy(&bytes[header_end..body_end]).into_owned(),
        session,
    })
}

fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|cookie| {
        let (key, value) = cookie.trim().split_once('=')?;
        (key == name).then(|| value.to_owned())
    })
}

fn form_fields(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|field| {
            let (key, value) = field.split_once('=')?;
            Some((url_decode(key), url_decode(value)))
        })
        .collect()
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    decoded.push(byte);
                    index += 2;
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn send_text(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    extra_headers: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{extra_headers}Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn pairing_page(request: &str, message: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>OpenMixer</title><style>{MOBILE_STYLE}input,button{{font:inherit;padding:16px;margin:8px;border:1px solid #39403f;background:#171b1a;color:#eee}}input{{width:90px;text-align:center}}p{{color:#e45c55}}</style></head><body><main><form method="post" action="/pair"><input type="hidden" name="request" value="{request}"><input name="code" type="text" inputmode="numeric" pattern="[0-9]{{2}}" maxlength="2" autocomplete="one-time-code" autofocus aria-label="Pairing code"><button type="submit">PAIR</button><p>{message}</p></form></main></body></html>"#
    )
}

const MOBILE_STYLE: &str = "*{box-sizing:border-box}html,body,main{width:100%;height:100%;margin:0}body{background:#0d0f0f;color:#eee;font-family:system-ui,sans-serif}main{display:grid;place-items:center}";

const PLAYER_HTML: &str = r##"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="theme-color" content="#0d0f0f"><title>OpenMixer</title><style>*{box-sizing:border-box}html,body,main{width:100%;height:100%;margin:0}body{background:#0d0f0f;font-family:system-ui,sans-serif}main{display:grid;place-items:center}button{width:160px;height:160px;border:1px solid #3d4a47;border-radius:50%;background:#17201e;color:#63d6c5;font:700 18px system-ui,sans-serif;letter-spacing:.12em}</style></head><body><main><button id="toggle">START</button><audio id="audio" preload="none" playsinline></audio></main><script>const a=document.querySelector('#audio'),b=document.querySelector('#toggle');let live=false,retry=0;async function start(){a.src='/stream?fresh='+Date.now();try{await a.play();live=true;b.textContent='STOP';if('mediaSession'in navigator)navigator.mediaSession.playbackState='playing'}catch(e){live=false;b.textContent='START'}}function stop(){live=false;clearTimeout(retry);a.pause();a.removeAttribute('src');a.load();b.textContent='START';if('mediaSession'in navigator)navigator.mediaSession.playbackState='paused'}b.onclick=()=>live?stop():start();a.onerror=()=>{if(live){clearTimeout(retry);retry=setTimeout(start,1000)}};if('mediaSession'in navigator){navigator.mediaSession.metadata=new MediaMetadata({title:'OpenMixer',artist:'LAN Listen'});navigator.mediaSession.setActionHandler('play',start);navigator.mediaSession.setActionHandler('pause',stop);navigator.mediaSession.setActionHandler('stop',stop)}</script></body></html>"##;

fn local_ipv4() -> Option<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_loopback() && !address.is_unspecified()).then_some(address)
}

fn wav_header(sample_rate: u32) -> [u8; 44] {
    let mut header = [0_u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&2_u16.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&(sample_rate * 4).to_le_bytes());
    header[32..34].copy_from_slice(&4_u16.to_le_bytes());
    header[34..36].copy_from_slice(&16_u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::{Duration, Instant};

    #[test]
    fn pairing_code_is_two_digits_and_creates_a_session() {
        let now = Instant::now();
        let mut auth = AuthState::default();
        let peer = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        let request = auth.begin_pairing(peer, now).unwrap();

        assert!((10..=99).contains(&request.code));
        let token = auth
            .verify(&request.id, request.code, now + Duration::from_secs(1))
            .unwrap();
        assert!(auth.is_authorized(&token));
        assert!(auth.pending.is_empty());
    }

    #[test]
    fn pairing_expires_and_locks_after_five_bad_attempts() {
        let now = Instant::now();
        let mut auth = AuthState::default();
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let expired = auth.begin_pairing(peer, now).unwrap();
        assert_eq!(
            auth.verify(
                &expired.id,
                expired.code,
                now + Duration::from_secs(61)
            ),
            Err(PairingError::Expired)
        );

        let locked = auth.begin_pairing(peer, now).unwrap();
        let wrong = if locked.code == 99 { 98 } else { 99 };
        for _ in 0..4 {
            assert_eq!(
                auth.verify(&locked.id, wrong, now),
                Err(PairingError::WrongCode)
            );
        }
        assert_eq!(
            auth.verify(&locked.id, wrong, now),
            Err(PairingError::TooManyAttempts)
        );
        assert_eq!(
            auth.verify(&locked.id, locked.code, now),
            Err(PairingError::UnknownRequest)
        );
    }

    #[test]
    fn wav_header_describes_live_stereo_pcm() {
        let header = wav_header(48_000);
        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([header[22], header[23]]), 2);
        assert_eq!(u32::from_le_bytes(header[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u16::from_le_bytes([header[34], header[35]]), 16);
        assert_eq!(&header[36..40], b"data");
    }

    #[test]
    fn audio_tap_is_bounded_and_converts_to_pcm() {
        let (tap, receiver) = audio_channel(1);
        assert!(tap.push([1.0, -1.0]));
        assert!(!tap.push([0.5, 0.5]));
        assert_eq!(receiver.try_recv().unwrap(), [i16::MAX, i16::MIN]);
    }
}
