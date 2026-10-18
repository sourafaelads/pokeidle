//! Conexão leve ao chat da Twitch (IRC sobre TLS), uma por conta.
//! Sem vídeo, sem página: só uma conexão de texto entrando nos chats dos canais.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct TwHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for TwHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Estado reportado para a interface (JSON pequeno).
fn st(state: &str, joined: usize, total: usize, msg: &str) -> String {
    serde_json::json!({ "st": state, "j": joined, "t": total, "m": msg }).to_string()
}

enum End {
    Stop,
    AuthFail,
    Retry(String),
}

pub fn spawn<F>(login: String, token: String, channels: Vec<String>, report: F) -> TwHandle
where
    F: Fn(String) + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("twitch".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let chans: Vec<String> = channels
                .iter()
                .map(|c| c.trim().trim_start_matches('#').to_lowercase())
                .filter(|c| !c.is_empty())
                .collect();
            let mut backoff = 5u64;
            loop {
                if stop2.load(Ordering::Relaxed) { return; }
                report(st("conectando", 0, chans.len(), ""));
                let started = Instant::now();
                match session(&login, &token, &chans, &stop2, &report) {
                    End::Stop => return,
                    End::AuthFail => {
                        report(st("auth", 0, chans.len(), "login da Twitch expirou"));
                        return;
                    }
                    End::Retry(why) => {
                        if started.elapsed() > Duration::from_secs(120) { backoff = 5; }
                        report(st("caiu", 0, chans.len(), &why));
                        let until = Instant::now() + Duration::from_secs(backoff);
                        while Instant::now() < until {
                            if stop2.load(Ordering::Relaxed) { return; }
                            std::thread::sleep(Duration::from_millis(500));
                        }
                        backoff = (backoff * 2).min(120);
                    }
                }
            }
        })
        .ok();
    TwHandle { stop }
}

fn session<F: Fn(String)>(login: &str, token: &str, chans: &[String], stop: &AtomicBool, report: &F) -> End {
    let tcp = match TcpStream::connect(("irc.chat.twitch.tv", 6697)) {
        Ok(t) => t,
        Err(e) => return End::Retry(format!("rede: {e}")),
    };
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(10)));
    let tls = match native_tls::TlsConnector::new() {
        Ok(c) => c,
        Err(e) => return End::Retry(format!("tls: {e}")),
    };
    let stream = match tls.connect("irc.chat.twitch.tv", tcp) {
        Ok(s) => s,
        Err(e) => return End::Retry(format!("tls: {e}")),
    };
    let mut rd = BufReader::new(stream);
    let send = |rd: &mut BufReader<native_tls::TlsStream<TcpStream>>, s: &str| -> bool {
        rd.get_mut().write_all(format!("{s}\r\n").as_bytes()).is_ok()
    };
    if !send(&mut rd, &format!("PASS oauth:{token}")) || !send(&mut rd, &format!("NICK {}", login.to_lowercase())) {
        return End::Retry("falha ao enviar login".into());
    }

    let me = login.to_lowercase();
    let mut joined: std::collections::HashSet<String> = Default::default();
    let mut welcomed = false;
    let mut last_rx = Instant::now();
    let mut ping_sent: Option<Instant> = None;
    let mut line = String::new();

    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = send(&mut rd, "QUIT");
            return End::Stop;
        }
        match rd.read_line(&mut line) {
            Ok(0) => return End::Retry("conexão fechada".into()),
            Ok(_) => {
                last_rx = Instant::now();
                ping_sent = None;
                let l = line.trim_end().to_string();
                line.clear();
                if l.starts_with("PING") {
                    let _ = send(&mut rd, &l.replacen("PING", "PONG", 1));
                } else if l.contains(" NOTICE * :") && (l.contains("authentication failed") || l.contains("Improperly formatted auth")) {
                    return End::AuthFail;
                } else if l.contains(" 001 ") && !welcomed {
                    welcomed = true;
                    // entra nos canais em lotes (limite da Twitch: 20 JOIN a cada 10 s)
                    for chunk in chans.chunks(10) {
                        let list: Vec<String> = chunk.iter().map(|c| format!("#{c}")).collect();
                        let _ = send(&mut rd, &format!("JOIN {}", list.join(",")));
                    }
                    report(st("entrando", 0, chans.len(), ""));
                } else if l.contains(" JOIN #") {
                    // :login!login@login.tmi.twitch.tv JOIN #canal
                    let who = l.trim_start_matches(':').split('!').next().unwrap_or("");
                    if who == me {
                        if let Some(c) = l.rsplit('#').next() { joined.insert(c.to_string()); }
                        report(st("ok", joined.len(), chans.len(), ""));
                    }
                } else if l.contains(" PART #") {
                    let who = l.trim_start_matches(':').split('!').next().unwrap_or("");
                    if who == me {
                        if let Some(c) = l.rsplit('#').next() { joined.remove(c); }
                        report(st("ok", joined.len(), chans.len(), ""));
                    }
                } else if l.contains(" RECONNECT") {
                    return End::Retry("servidor pediu reconexão".into());
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                // sem dados: mantém a linha parcial e confere se a conexão segue viva
                if last_rx.elapsed() > Duration::from_secs(240) && ping_sent.is_none() {
                    let _ = send(&mut rd, "PING :keepalive");
                    ping_sent = Some(Instant::now());
                }
                if let Some(t) = ping_sent {
                    if t.elapsed() > Duration::from_secs(30) { return End::Retry("sem resposta".into()); }
                }
            }
            Err(e) => return End::Retry(format!("leitura: {e}")),
        }
    }
}


/// Descobre o login (nome de usuário) dono do token e confirma que ele é válido.
pub fn validate(token: &str) -> Result<String, String> {
    let tcp = TcpStream::connect(("id.twitch.tv", 443)).map_err(|e| format!("rede: {e}"))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    let mut s = tls.connect("id.twitch.tv", tcp).map_err(|e| e.to_string())?;
    let req = format!("GET /oauth2/validate HTTP/1.1\r\nHost: id.twitch.tv\r\nAuthorization: OAuth {token}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut body = String::new();
    let _ = std::io::Read::read_to_string(&mut s, &mut body);
    let status_ok = body.starts_with("HTTP/1.1 200");
    let json = body.split("\r\n\r\n").nth(1).unwrap_or("");
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap_or_default();
    match v.get("login").and_then(|l| l.as_str()) {
        Some(l) if status_ok => Ok(l.to_string()),
        _ => Err("token inválido".into()),
    }
}
