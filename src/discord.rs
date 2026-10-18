//! Discord Rich Presence pelo "IPC" local do Discord (named pipe \\.\pipe\discord-ipc-N).
//! Sem bibliotecas, sem internet: só mensagens JSON pequenas para o Discord aberto no PC.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, PartialEq)]
pub struct Presence {
    pub details: String,
    pub state: String,
    /// lista de imagens candidatas (a primeira que existir de verdade é usada)
    pub large_image: Vec<String>,
    pub large_text: String,
    pub small_image: String,
    pub small_text: String,
    /// se houver: contagem regressiva até este horário (unix, s); senão, tempo decorrido
    pub end: Option<u64>,
    /// início da sessão de hunt (unix, s) -> tempo decorrido igual ao do jogo
    pub start: Option<u64>,
}

fn frame(op: u32, json: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + json.len());
    v.extend_from_slice(&op.to_le_bytes());
    v.extend_from_slice(&(json.len() as u32).to_le_bytes());
    v.extend_from_slice(json.as_bytes());
    v
}

fn read_frame(f: &mut File) -> std::io::Result<(u32, String)> {
    let mut h = [0u8; 8];
    f.read_exact(&mut h)?;
    let op = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
    let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
    let mut b = vec![0u8; len.min(1 << 20)];
    f.read_exact(&mut b)?;
    Ok((op, String::from_utf8_lossy(&b).into_owned()))
}

fn connect(client_id: &str) -> Option<File> {
    for n in 0..10 {
        let path = format!(r"\\.\pipe\discord-ipc-{n}");
        if let Ok(mut f) = OpenOptions::new().read(true).write(true).open(&path) {
            let hs = serde_json::json!({ "v": 1, "client_id": client_id }).to_string();
            if f.write_all(&frame(0, &hs)).is_err() { continue; }
            match read_frame(&mut f) {
                Ok((1, _)) => return Some(f), // READY
                _ => continue,
            }
        }
    }
    None
}

/// Confere se um link de imagem existe (HEAD -> 200). Só https, com tempo limite curto.
fn url_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let (host, path) = match rest.find('/') { Some(i) => (&rest[..i], &rest[i..]), None => (rest, "/") };
    let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, 443)) else { return false };
    let Some(addr) = addrs.into_iter().next() else { return false };
    let Ok(tcp) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)) else { return false };
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(tls) = native_tls::TlsConnector::new() else { return false };
    let Ok(mut s) = tls.connect(host, tcp) else { return false };
    let req = format!("HEAD {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: PokeIdleHub\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() { return false; }
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]);
    head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200") || head.starts_with("HTTP/2 200")
}

fn pick_image(cands: &[String], cache: &mut std::collections::HashMap<String, bool>) -> String {
    for u in cands {
        if u.is_empty() { continue; }
        let ok = *cache.entry(u.clone()).or_insert_with(|| url_ok(u));
        if ok { return u.clone(); }
    }
    "logo".into()
}

fn send_activity(f: &mut File, p: Option<&Presence>, start: u64, nonce: u64, img: &str, small: &str) -> bool {
    let activity = p.map(|p| {
        let mut assets = serde_json::Map::new();
        let put = |m: &mut serde_json::Map<String, serde_json::Value>, k: &str, v: &str| {
            if !v.is_empty() { m.insert(k.into(), serde_json::Value::String(v.chars().take(127).collect())); }
        };
        put(&mut assets, "large_image", img);
        put(&mut assets, "large_text", &p.large_text);
        if !small.is_empty() {
            put(&mut assets, "small_image", small);
            put(&mut assets, "small_text", &p.small_text);
        }
        let ts = match (p.start, p.end) {
            (Some(s0), _) => serde_json::json!({ "start": s0 }),
            (None, Some(e)) => serde_json::json!({ "end": e }),
            _ => serde_json::json!({ "start": start }),
        };
        serde_json::json!({
            "details": p.details.chars().take(127).collect::<String>(),
            "state": p.state.chars().take(127).collect::<String>(),
            "timestamps": ts,
            "assets": assets,
        })
    });
    let msg = serde_json::json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": nonce.to_string(),
    })
    .to_string();
    if f.write_all(&frame(1, &msg)).is_err() { return false; }
    read_frame(f).is_ok() // lê a resposta para o "cano" não encher
}

/// Inicia a thread do Rich Presence. Mande `Some(Presence)` para atualizar, `None` para limpar.
pub fn spawn(client_id: String) -> Sender<Option<Presence>> {
    let (tx, rx): (Sender<Option<Presence>>, Receiver<Option<Presence>>) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("discord".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            let start = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let mut pipe: Option<File> = None;
            let mut want: Option<Presence> = None;
            let mut sent: Option<Option<Presence>> = None;
            let mut last_send = Instant::now() - Duration::from_secs(60);
            let mut last_try = Instant::now() - Duration::from_secs(60);
            let mut nonce = 0u64;
            let mut cache: std::collections::HashMap<String, bool> = Default::default();
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(p) => want = p,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                while let Ok(p) = rx.try_recv() { want = p; }
                // Discord aberto? (tenta reconectar a cada 30 s)
                if pipe.is_none() && last_try.elapsed() > Duration::from_secs(30) {
                    last_try = Instant::now();
                    pipe = connect(&client_id);
                    sent = None;
                }
                // limite do Discord: ~5 atualizações a cada 20 s -> no máximo 1 a cada 9 s
                if let Some(f) = pipe.as_mut() {
                    if sent.as_ref() != Some(&want) && last_send.elapsed() > Duration::from_secs(9) {
                        nonce += 1;
                        let img = want.as_ref().map(|p| pick_image(&p.large_image, &mut cache)).unwrap_or_default();
                        let small = want.as_ref().map(|p| pick_image(&[p.small_image.clone()], &mut cache)).filter(|u| u != "logo").unwrap_or_default();
                        if send_activity(f, want.as_ref(), start, nonce, &img, &small) {
                            sent = Some(want.clone());
                            last_send = Instant::now();
                        } else {
                            pipe = None; // Discord fechou: tenta de novo depois
                        }
                    }
                }
            }
        })
        .ok();
    tx
}
