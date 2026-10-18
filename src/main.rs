#![windows_subsystem = "windows"]
//! PokeIdle Hub — 4 contas do PokeIdle numa janela só, cada uma com login isolado.

mod discord;
mod twitch;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tao::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{Rect, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

/// Versão com Kick só quando compilado com `--features kick`.
const KICK: bool = cfg!(feature = "kick");

const TOOLBAR_H: f64 = 38.0; // altura lógica da barra
const OFFSCREEN: i32 = -30000; // onde ficam as contas "escondidas" (continuam rodando)

// Flags do motor (WebView2/Edge) para nunca congelar abas fora de foco.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling \
--disable-background-timer-throttling --disable-renderer-backgrounding \
--disable-backgrounding-occluded-windows --autoplay-policy=no-user-gesture-required";

#[derive(Serialize, Deserialize, Clone)]
struct Config {
    url: String,
    contas: Vec<String>,
    zoom_grade: f64,
    zoom_normal: f64,
    mutar: bool,
    #[serde(default = "yes")]
    flags_extras: bool,
    /// Grade mostra só o painel "Modo Economia" de cada conta, ampliado.
    #[serde(default = "yes")]
    grade_foco: bool,
    /// Largura (em px de página) que o jogo "acha" que tem no modo grade.
    #[serde(default = "largura")]
    largura_layout: f64,
    /// Um motor só (WebView2) para tudo, com um perfil por conta = muito menos RAM.
    #[serde(default = "yes")]
    motor_unico: bool,
    /// Contas fora da vista param de DESENHAR (o jogo continua rodando) = menos CPU.
    #[serde(default = "yes")]
    pausar_desenho: bool,
    /// Pede ao motor para enxugar a memória das contas fora da vista.
    /// Desligado por padrão: pode fazer o jogo "reconectar" e cair no aviso de conta aberta em outro lugar.
    #[serde(default)]
    memoria_baixa: bool,
    /// Canais oficiais: cada conta conectada na Twitch entra no chat de todos eles.
    #[serde(default = "canais_padrao")]
    canais: Vec<String>,
    /// IDs de extensões do Chrome/Brave para carregar nas contas (copiadas do Brave/Chrome instalado).
    #[serde(default = "extensoes_padrao")]
    extensoes: Vec<String>,
    /// Mostra o resumo no Discord ("Jogando ...") — precisa do Discord aberto no PC.
    #[serde(default = "yes")]
    discord: bool,
    #[serde(default = "discord_id_padrao")]
    discord_id: String,
    /// Nick no Discord: "mostrar", "borrar" (ph••••) ou "esconder" (Conta 2)
    #[serde(default = "nick_padrao")]
    discord_nick: String,
    /// segundos para trocar de conta no Discord
    #[serde(default = "intervalo_padrao")]
    discord_intervalo: u64,
    /// emoji antes do nick no Discord ("" = nenhum)
    #[serde(default = "emoji_padrao")]
    discord_emoji: String,
    /// Canais oficiais da Kick (o app abre a live dos que estiverem ao vivo)
    #[serde(default = "kick_canais_padrao")]
    kick_canais: Vec<String>,
    /// Quantas lives da Kick manter abertas por conta conectada (cada uma ~150-250 MB)
    #[serde(default = "um")]
    kick_lives: usize,
    /// Máximo de lives da Kick abertas ao mesmo tempo por conta (padrão 1; 0 = todas as que estiverem ao vivo)
    #[serde(default = "um")]
    kick_max: usize,
    /// Botão do Instagram do autor no meio da barra (false = esconder)
    #[serde(default = "yes")]
    mostrar_insta: bool,
    /// Repositório do GitHub consultado ao abrir para avisar de versão nova ("" = não consultar)
    #[serde(default = "repo_padrao")]
    repo_github: String,
}

const INSTA: &str = "sourafaelads";
/// Versão deste app (compare com a "tag" da Release no GitHub, ex.: v0.37)
const VERSAO: &str = "0.37";
/// Repositório onde saem as versões novas (Releases)
const REPO_PADRAO: &str = "sourafaelads/pokeidle";
fn repo_padrao() -> String { REPO_PADRAO.into() }

fn ver_num(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches(['v', 'V']).split(|c: char| !c.is_ascii_digit()).filter(|x| !x.is_empty()).filter_map(|x| x.parse().ok()).collect()
}


fn um() -> usize { 1 }
fn kick_canais_padrao() -> Vec<String> {
    ["surskity", "axt", "rodil", "guaxilol1", "yetz", "patopapao2", "clovaobengalalol", "biscoitao", "matta", "cogu",
     "tftoddy", "luccatv", "techzz", "dersin2k"].iter().map(|s| s.to_string()).collect()
}

/// A conta está logada na Kick (neste perfil)?
fn kick_logged(wv: &WebView) -> bool {
    wv.cookies_for_url("https://kick.com/").map(|cs| cs.iter().any(|c| c.name() == "session_token" && c.value().len() > 10)).unwrap_or(false)
}

/// Script das janelas escondidas da Kick.
/// slot 0 = "gerente": a cada 2 min vê quais canais da lista estão ao vivo e avisa o app (que abre/fecha as outras janelas).
/// Todas: mantêm o player tocando (mudo) e vão para o canal que o app mandar (window.__hubKickGo).
fn kick_script(canais: &[String], slot: usize) -> String {
    let ch: Vec<String> = canais.iter().map(|c| c.trim().to_lowercase()).filter(|c| !c.is_empty()).collect();
    format!(r#"(function(){{
  if (window !== window.top || window.__hubKick) return; window.__hubKick = true;
  const CH = {ch}, SLOT = {slot};
  const say = o => {{ try {{ window.ipc && window.ipc.postMessage('kick:' + JSON.stringify(Object.assign({{slot: SLOT}}, o))); }} catch (_) {{}} }};
  const cur = () => location.pathname.replace(/^\//, '').split('/')[0].toLowerCase();
  async function live(slug){{
    try {{ const r = await fetch('/api/v2/channels/' + slug, {{ credentials: 'include' }}); if (!r.ok) return false;
          const j = await r.json(); return !!(j && j.livestream); }} catch (_) {{ return false; }}
  }}
  function keepPlaying(){{
    for (const b of document.querySelectorAll('button')) {{
      const t = (b.innerText || '').trim();
      if (/^(start watching|come[cç]ar a assistir|assistir|i'?m 18|tenho 18|aceitar|accept|continuar)/i.test(t)) b.click();
    }}
    const v = document.querySelector('video'); if (v) {{ v.muted = true; if (v.paused) v.play().catch(()=>{{}}); }}
    return !!(v && !v.paused);
  }}
  window.__hubKickGo = function(c){{ if (c && cur() !== c) location.href = '/' + c; }};
  window.__hubDump = function(){{ try {{ window.ipc && window.ipc.postMessage('kickdump:' + document.documentElement.outerHTML); }} catch (_) {{}} }};
  // qualidade mínima no player (160p): abre o menu de configurações do player e escolhe a menor
  const sleep = ms => new Promise(r => setTimeout(r, ms));
  let Q = 'auto', qTries = 0;
  const qn = e => {{ const m = /^\s*(\d{{3,4}})p/i.exec((e.textContent || '').trim()); return m ? +m[1] : 0; }};
  function qOpts(){{ return [...document.querySelectorAll('[role=menuitemradio],[role=menuitem],[role=option],li,button,label,div')].filter(e => e.children.length <= 3 && qn(e) && (e.textContent || '').trim().length < 20); }}
  async function lowQ(){{
    if (Q !== 'auto' || qTries++ > 8 || !document.querySelector('video')) return;
    try {{
      const btns = [...document.querySelectorAll('button')];
      const gear = btns.find(b => /settings|configura|quality|qualidade|ajustes/i.test((b.getAttribute('aria-label') || '') + ' ' + (b.title || '')));
      if (!gear) return;
      gear.click(); await sleep(500);
      let opts = qOpts();
      if (!opts.length) {{
        const sub = [...document.querySelectorAll('[role=menuitem],button,li,div')].find(e => e.children.length <= 4 && /^(quality|qualidade)/i.test((e.textContent || '').trim()));
        if (sub) {{ sub.click(); await sleep(500); opts = qOpts(); }}
      }}
      if (opts.length) {{
        opts.sort((a, b) => qn(a) - qn(b));
        opts[0].click(); Q = qn(opts[0]) + 'p';
      }}
      await sleep(300);
      document.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Escape', bubbles: true }}));
      if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
    }} catch (_) {{}}
  }}
  setInterval(lowQ, 15000); setTimeout(lowQ, 9000);
  function report(){{ say({{ st: keepPlaying() ? 'ok' : 'carregando', ch: cur(), q: Q }}); }}
  async function manager(){{
    const L = []; for (const c of CH) {{ if (await live(c)) L.push(c); }}
    say({{ lives: L }});
    if (L.length && !L.includes(cur())) {{ location.href = '/' + L[0]; return; }}
    if (!L.length) say({{ st: 'esperando' }}); else report();
    setTimeout(manager, 120000);
  }}
  setInterval(keepPlaying, 30000);
  setInterval(report, 60000);
  setTimeout(SLOT === 0 ? manager : report, 6000);
}})();"#, ch = serde_json::to_string(&ch).unwrap(), slot = slot)
}

fn emoji_padrao() -> String { "🧢".into() }

fn intervalo_padrao() -> u64 { 60 }

fn nick_padrao() -> String { "mostrar".into() }

fn discord_id_padrao() -> String { "1560507776709566525".into() }

/// "Charizard" -> https://play.pokemonshowdown.com/sprites/ani/charizard.gif
fn showdown_sprite(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    if lower.is_empty() { return String::new(); }
    let mut id = String::new();
    for c in lower.chars() {
        let c = match c { 'á'|'à'|'â'|'ã'|'ä' => 'a', 'é'|'è'|'ê'|'ë' => 'e', 'í'|'ì'|'î'|'ï' => 'i', 'ó'|'ò'|'ô'|'õ'|'ö' => 'o', 'ú'|'ù'|'û'|'ü' => 'u', 'ç' => 'c', '♀' => 'f', '♂' => 'm', x => x };
        if c.is_ascii_alphanumeric() { id.push(c); }
    }
    // formas Mega: "Mega Charizard X" -> charizard-megax
    if let Some(rest) = id.strip_prefix("mega") {
        if !rest.is_empty() && rest != "nium" {
            let (base, suf) = if rest.ends_with('x') || rest.ends_with('y') { (&rest[..rest.len() - 1], &rest[rest.len() - 1..]) } else { (rest, "") };
            id = format!("{}-mega{}", base, suf);
        }
    }
    format!("https://play.pokemonshowdown.com/sprites/ani/{}.gif", id)
}

fn mask_nick(n: &str, mode: &str, i: usize) -> String {
    match mode {
        "esconder" => format!("Conta {}", i + 1),
        "borrar" => {
            let c: Vec<char> = n.chars().collect();
            let keep: String = c.iter().take(2).collect();
            format!("{}{}", keep, "•".repeat(c.len().saturating_sub(2).max(3)))
        }
        _ => n.to_string(),
    }
}

fn pnum(s: &str) -> f64 {
    let t: String = s.split('/').next().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
    let neg = t.starts_with('−') || t.starts_with('-');
    let t = t.trim_start_matches(['−', '-', '+']).replace('.', "").replace(',', ".");
    let v: f64 = t.parse().unwrap_or(0.0);
    if neg { -v } else { v }
}

fn short(v: f64) -> String {
    let (a, sign) = (v.abs(), if v < 0.0 { "−" } else { "+" });
    let s = if a >= 1e9 { format!("{:.1}B", a / 1e9) } else if a >= 1e6 { format!("{:.1}M", a / 1e6) } else if a >= 1e3 { format!("{:.0}k", a / 1e3) } else { format!("{:.0}", a) };
    format!("{}{}", sign, s.replace('.', ","))
}

/// Discord, uma conta por vez:
///   imagem: Pokémon em campo (mouse: "Charizard Nv 95 · Nota 3,256")
///   linha 1: "treinador · Nv 95"   linha 2: "📍 Forretress · nível 80"
///   ícone pequeno: pokébola (mouse: "3,1M XP/h · +27k/h")   relógio: contagem até upar
fn presence_from(stats: &[Option<serde_json::Value>], names: &[String], mode: &str, rot: usize, emoji: &str) -> Option<discord::Presence> {
    let str_of = |v: &serde_json::Value, path: &[&str]| -> String {
        let mut cur = v;
        for k in path { match cur.get(*k) { Some(x) => cur = x, None => return String::new() } }
        cur.as_str().unwrap_or("").trim().to_string()
    };
    let ativos: Vec<usize> = (0..stats.len())
        .filter(|&i| stats[i].as_ref().and_then(|v| v.get("ok")).and_then(|x| x.as_bool()) == Some(true))
        .collect();
    if ativos.is_empty() { return None; }
    let i = ativos[rot % ativos.len()];
    let v = stats[i].as_ref()?;
    let (mut xp, mut lucro) = (0.0f64, 0.0f64);
    for st in v.get("stats").and_then(|x| x.as_array()).into_iter().flatten() {
        let l = st.get("l").and_then(|x| x.as_str()).unwrap_or("").to_uppercase();
        let r = pnum(st.get("r").and_then(|x| x.as_str()).unwrap_or(""));
        if l.contains("TREINADOR") { xp = r; } else if l.contains("LUCRO") { lucro = r; }
    }
    let nick = mask_nick(names.get(i).map(|x| x.as_str()).unwrap_or(""), mode, i);
    let lvl = str_of(v, &["pl", "lvl"]);
    let nick = if emoji.trim().is_empty() { nick } else { format!("{} {}", emoji.trim(), nick) };
    let details = if lvl.is_empty() { nick } else { format!("{} · Nv {}", nick, lvl) };
    let hunt = str_of(v, &["hunt"]);
    let alvo = hunt.split('·').next().unwrap_or("").trim().to_string(); // "Ancient Pupitar · nível 150" -> "Ancient Pupitar"
    let pk = str_of(v, &["pk", "name"]);
    let pkl = str_of(v, &["pk", "lvl"]);
    let meu = if pk.is_empty() { String::new() } else if pkl.is_empty() { pk.clone() } else { format!("{} Nv {}", pk, pkl) };
    let state = match (meu.is_empty(), alvo.is_empty()) {
        (false, false) => format!("{} ⚔️ {}", meu, alvo),
        (false, true) => meu.clone(),
        (true, false) => format!("⚔️ {}", alvo),
        _ => "Caçando".to_string(),
    };
    let nota = str_of(v, &["pk", "n"]);
    let mut large_text = pk.clone();
    if !pkl.is_empty() { large_text += &format!(" Nv {}", pkl); }
    if !nota.is_empty() { large_text += &format!(" · Nota {}", nota); }
    let anim = showdown_sprite(&pk);
    let img: Vec<String> = if anim.is_empty() { vec![] } else { vec![anim.clone(), anim.replace("/sprites/ani/", "/sprites/gen5/").replace(".gif", ".png")] };
    // relógio = tempo da sessão de hunt do jogo ("1:12:34")
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let sess: u64 = str_of(v, &["timer"]).split(':').fold(0u64, |a, x| a * 60 + x.trim().parse::<u64>().unwrap_or(0));
    let start = if sess > 0 { Some(now.saturating_sub(sess)) } else { None };
    Some(discord::Presence {
        details,
        state,
        large_image: img,
        large_text,
        small_image: "https://play.pokemonshowdown.com/sprites/itemicons/poke-ball.png".into(),
        small_text: format!("{} XP/h · {}/h", short(xp).trim_start_matches('+'), short(lucro)),
        end: None,
        start,
    })
}

fn extensoes_padrao() -> Vec<String> {
    vec![
        "mhnplloljgchpfpcpbhmcokmnefodjnh".into(), // /hunt for PokeIdle
        "olccbpdjdkbfgondiiaicmlifhbcggmk".into(), // Rotom Sniper
    ]
}



fn canais_padrao() -> Vec<String> {
    ["Surskity11", "ClovaoBengala", "aXtLOL", "Rodil", "matta", "Biscoitao", "cogu", "Guaxilol1",
     "LuccaTV", "faasii", "PatoPapao", "TFToddy", "Techzz", "dersin2k"].iter().map(|s| s.to_string()).collect()
}

fn yes() -> bool { true }
fn largura() -> f64 { 1920.0 }
fn default_name(i: usize) -> String { format!("Conta {}", i + 1) }

impl Default for Config {
    fn default() -> Self {
        Self {
            url: "https://pokeidle.io/".into(),
            contas: vec!["Conta 1".into(), "Conta 2".into(), "Conta 3".into(), "Conta 4".into()],
            zoom_grade: 0.6,
            zoom_normal: 1.0,
            mutar: true,
            flags_extras: true,
            grade_foco: true,
            largura_layout: 1920.0,
            motor_unico: true,
            pausar_desenho: true,
            memoria_baixa: false,
            canais: canais_padrao(),
            extensoes: extensoes_padrao(),
            discord: true,
            discord_id: discord_id_padrao(),
            discord_nick: nick_padrao(),
            discord_intervalo: intervalo_padrao(),
            discord_emoji: emoji_padrao(),
            kick_canais: kick_canais_padrao(),
            kick_lives: 1,
            kick_max: 1,
            mostrar_insta: true,
            repo_github: repo_padrao(),
        }
    }
}

// ---------- diagnóstico: log.txt ao lado do .exe ----------
static LOG: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
fn log_init() {
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(base_dir);
    if let Ok(f) = std::fs::File::create(dir.join("log.txt")) { *LOG.lock().unwrap() = Some(f); }
    T0.get_or_init(std::time::Instant::now);
}
fn log(msg: &str) {
    use std::io::Write;
    let t = T0.get().map(|t| t.elapsed().as_millis()).unwrap_or(0);
    if let Ok(mut g) = LOG.lock() { if let Some(f) = g.as_mut() { let _ = writeln!(f, "[{:>8}ms] {}", t, msg); let _ = f.flush(); } }
}
macro_rules! logf { ($($a:tt)*) => { log(&format!($($a)*)) } }

fn base_dir() -> PathBuf {
    let root = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    let d = root.join("PokeIdleHub");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn ver_key(name: &str) -> Vec<u64> {
    name.split(|c: char| c == '.' || c == '_').map(|p| p.parse().unwrap_or(0)).collect()
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        if name.to_string_lossy().starts_with('_') { continue; } // "_metadata" é da loja; extensão descompactada não aceita
        let (src, dst) = (e.path(), to.join(&name));
        if e.file_type()?.is_dir() { copy_dir(&src, &dst)?; } else { std::fs::copy(&src, &dst)?; }
    }
    Ok(())
}

/// Procura as extensões instaladas no Brave/Chrome e copia a versão mais nova para a pasta do app.
/// Retorna a pasta-raiz (cada subpasta = 1 extensão), ou None se nada foi encontrado.
fn prepare_extensions(ids: &[String]) -> Option<PathBuf> {
    if ids.is_empty() { return None; }
    let local = PathBuf::from(std::env::var("LOCALAPPDATA").ok()?);
    let roots = [
        local.join("BraveSoftware").join("Brave-Browser").join("User Data"),
        local.join("Google").join("Chrome").join("User Data"),
        local.join("Microsoft").join("Edge").join("User Data"),
    ];
    let dest_root = base_dir().join("extensoes");
    let mut any = false;
    for id in ids {
        // acha a versão mais nova em qualquer perfil de qualquer navegador
        let mut best: Option<(Vec<u64>, PathBuf, String)> = None;
        for r in &roots {
            let Ok(profiles) = std::fs::read_dir(r) else { continue };
            for p in profiles.flatten() {
                let ext = p.path().join("Extensions").join(id);
                let Ok(vers) = std::fs::read_dir(&ext) else { continue };
                for v in vers.flatten() {
                    let name = v.file_name().to_string_lossy().to_string();
                    if !v.path().join("manifest.json").is_file() { continue; }
                    let k = ver_key(&name);
                    if best.as_ref().map_or(true, |b| k > b.0) { best = Some((k, v.path(), name)); }
                }
            }
        }
        let dest = dest_root.join(id);
        match best {
            Some((_, src, ver)) => {
                let marker = dest.join(".versao");
                let cur = std::fs::read_to_string(&marker).unwrap_or_default();
                if cur.trim() != ver || !dest.join("manifest.json").is_file() {
                    let _ = std::fs::remove_dir_all(&dest);
                    match copy_dir(&src, &dest) {
                        Ok(_) => { let _ = std::fs::write(&marker, &ver); logf!("extensao {}: copiada versao {} de {:?}", id, ver, src); }
                        Err(e) => { logf!("extensao {}: erro ao copiar: {}", id, e); continue; }
                    }
                } else {
                    logf!("extensao {}: versao {} ja copiada", id, ver);
                }
                any = true;
            }
            None => {
                if dest.join("manifest.json").is_file() { any = true; logf!("extensao {}: usando copia antiga", id); }
                else { logf!("extensao {}: nao encontrada no Brave/Chrome/Edge", id); }
            }
        }
    }
    if any { Some(dest_root) } else { None }
}

/// Consulta a última Release no GitHub. Devolve (tag, link) se for mais nova que esta versão.
fn check_update(repo: &str) -> Option<(String, String)> {
    use std::io::{Read, Write};
    let host = "api.github.com";
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&(host, 443)).ok()?.next()?;
    let tcp = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(8)).ok()?;
    let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(8)));
    let mut s = native_tls::TlsConnector::new().ok()?.connect(host, tcp).ok()?;
    let req = format!("GET /repos/{repo}/releases/latest HTTP/1.0\r\nHost: {host}\r\nUser-Agent: PokeIdleHub/{VERSAO}\r\nAccept: application/vnd.github+json\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let _ = s.take(512 * 1024).read_to_end(&mut buf);
    let txt = String::from_utf8_lossy(&buf);
    if !txt.starts_with("HTTP/1.1 200") && !txt.starts_with("HTTP/1.0 200") { logf!("atualizacao: sem release ({})", txt.lines().next().unwrap_or("")); return None; }
    let v: serde_json::Value = serde_json::from_str(txt.split("\r\n\r\n").nth(1)?.trim()).ok()?;
    let tag = v.get("tag_name")?.as_str()?.to_string();
    let url = v.get("html_url").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let url = if url.starts_with("https://github.com/") { url } else { format!("https://github.com/{repo}/releases/latest") };
    logf!("atualizacao: ultima {} (esta {})", tag, VERSAO);
    if ver_num(&tag) > ver_num(VERSAO) { Some((tag, url)) } else { None }
}

fn save_config(c: &Config) {
    let _ = std::fs::write(base_dir().join("config.json"), serde_json::to_string_pretty(c).unwrap());
}

fn load_config() -> Config {
    let p = base_dir().join("config.json");
    match std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Config>(&s).ok()) {
        Some(mut c) => {
            // canais novos que vierem no app entram mesmo com config antigo
            for d in canais_padrao() { if !c.canais.iter().any(|x| x.eq_ignore_ascii_case(&d)) { c.canais.push(d); } }
            for d in extensoes_padrao() { if !c.extensoes.iter().any(|x| x == &d) { c.extensoes.push(d); } }
            for d in kick_canais_padrao() { if !c.kick_canais.iter().any(|x| x.eq_ignore_ascii_case(&d)) { c.kick_canais.push(d); } }
            if c.repo_github == "sourafaelads/pokeidle-hub" { c.repo_github = repo_padrao(); } // nome antigo
            c.contas.truncate(4); // máximo de 4 contas (regras do jogo)
            if c.contas.is_empty() { c.contas = Config::default().contas; }
            c
        }
        None => {
            let c = Config::default();
            let _ = std::fs::write(&p, serde_json::to_string_pretty(&c).unwrap());
            c
        }
    }
}

#[derive(Debug)]
enum Msg {
    Tab(usize),
    Grid,
    Reload(Option<usize>), // None = todas
    OpenConfig,
    DevTools,
    OpenDevTools,
    DiscordMode,
    TaskMgr,
    Rotom,
    Insta,
    UpdateAvail(String, String),
    OpenUpdate,
    Nick(usize, String),
    Stats(usize, String),
    Drop(usize),
    Kick(usize, bool),
    TwLogin(usize),
    KickLogin(usize),
    KickShow(usize),
    KickStatus(usize, String),
    TwCancel,
    TwStatus(usize, String),
    Tick,
    Rename(usize, String),
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (b.get(i + 1).and_then(|c| (*c as char).to_digit(16)), b.get(i + 2).and_then(|c| (*c as char).to_digit(16))) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_msg(s: &str) -> Option<Msg> {
    let s = s.trim();
    if s == "grid" { return Some(Msg::Grid); }
    if s == "config" { return Some(Msg::OpenConfig); }
    if s == "devtools" { return Some(Msg::DevTools); }
    if s == "opendevtools" { return Some(Msg::OpenDevTools); }
    if s == "dmode" { return Some(Msg::DiscordMode); }
    if s == "taskmgr" { return Some(Msg::TaskMgr); }
    if s == "rotom" { return Some(Msg::Rotom); }
    if s == "insta" { return Some(Msg::Insta); }
    if s == "update" { return Some(Msg::OpenUpdate); }
    if s == "reload:all" { return Some(Msg::Reload(None)); }
    if let Some(n) = s.strip_prefix("tab:") { return n.parse().ok().map(Msg::Tab); }
    if let Some(n) = s.strip_prefix("drop:") { return n.parse().ok().map(Msg::Drop); }
    if let Some(r) = s.strip_prefix("kick:") {
        let (i, on) = r.split_once(':')?;
        return i.parse().ok().map(|i| Msg::Kick(i, on == "1"));
    }
    if let Some(n) = s.strip_prefix("twlogin:") { return n.parse().ok().map(Msg::TwLogin); }
    if s == "twcancel" { return Some(Msg::TwCancel); }
    if let Some(n) = s.strip_prefix("kicklogin:") { return n.parse().ok().map(Msg::KickLogin); }
    if let Some(n) = s.strip_prefix("kickshow:") { return n.parse().ok().map(Msg::KickShow); }
    if let Some(n) = s.strip_prefix("reload:") { return n.parse().ok().map(|i| Msg::Reload(Some(i))); }
    if let Some(r) = s.strip_prefix("rename:") {
        let (i, name) = r.split_once(':')?;
        let name: String = pct_decode(name).trim().chars().take(24).collect();
        return i.parse().ok().map(|i| Msg::Rename(i, name));
    }
    None
}

// Script injetado SÓ na página principal de cada conta: apenas atalhos de teclado.
// (Nada de mexer em visibilidade/foco: isso quebra a verificação da Cloudflare.
//  O "nunca congelar" fica a cargo das flags do motor.)
const GAME_SCRIPT: &str = r#"(function(){
  if (window !== window.top || window.__hub) return; window.__hub = true;
  window.addEventListener('keydown', e => {
    if (!window.ipc) return;
    if (e.ctrlKey && e.key >= '1' && e.key <= '9') { window.ipc.postMessage('tab:' + (+e.key - 1)); e.preventDefault(); }
    else if (e.ctrlKey && (e.key === 'g' || e.key === 'G')) { window.ipc.postMessage('grid'); e.preventDefault(); }
    else if (e.ctrlKey && (e.key === 'p' || e.key === 'P')) { window.ipc.postMessage('rotom'); e.preventDefault(); e.stopPropagation(); }
    else if (e.key === 'F12') { window.ipc.postMessage('opendevtools'); e.preventDefault(); }
  }, true);

  // --- Nick da conta: o "Nv NN" mais alto da coluna da esquerda é o do jogador;
  //     o nick é o texto na mesma linha, à esquerda dele.
  function leafText(x){ return (x.children.length ? '' : (x.textContent || '').trim()); }
  const NV = /^Nv\.?\s*\d+$/i;
  // "Nv NN" da coluna da esquerda, de cima para baixo: [0] = jogador, [1] = Pokémon em campo
  function leftNvs(){
    const vw = innerWidth, out = [];
    for (const e of document.querySelectorAll('body *')) {
      if (e.children.length > 3) continue;
      const t = (e.textContent || '').trim();
      if (!NV.test(t)) continue;
      if ([...e.children].some(c => NV.test((c.textContent || '').trim()))) continue;
      const r = e.getBoundingClientRect();
      if (r.width && r.left < vw * 0.22) out.push([r, e, t]);
    }
    return out.sort((a, b) => a[0].top - b[0].top);
  }
  // texto na mesma linha, à esquerda do elemento
  function ownText(x){ let t = ''; for (const n of x.childNodes) if (n.nodeType === 3) t += n.textContent; return t.trim(); }
  function okLabel(t){ return t.length >= 2 && t.length <= 24 && !/^Nv/i.test(t) && !/\d[.,]\d/.test(t) && !t.includes('/'); }
  function rowLabel(r, e){
    for (let p = e.parentElement, k = 0; p && k < 2; p = p.parentElement, k++) {
      const t = ownText(p); if (t && okLabel(t)) return t;
    }
    let p = e; for (let k = 0; k < 4 && p.parentElement; k++) p = p.parentElement;
    const cy = r.top + r.height / 2;
    let best = null, bestRight = -1e9;
    for (const x of p.querySelectorAll('*')) {
      if (x === e || e.contains(x)) continue;
      const t = leafText(x) || ownText(x);
      if (!okLabel(t)) continue;
      const q = x.getBoundingClientRect();
      if (!q.width || Math.abs(q.top + q.height / 2 - cy) > Math.max(r.height, 14)) continue;
      if (q.right > r.left + 4) continue;
      if (q.right > bestRight) { bestRight = q.right; best = t; }
    }
    return best;
  }
  function findNick(){
    try {
      const e = document.getElementById('tr-nick');
      const t = e ? (e.textContent || '').trim() : '';
      if (t && /[A-Za-z0-9]/.test(t)) return t;
      if (e) return null; // o jogo ainda está carregando o nick ("—")
      const n = leftNvs()[0]; return n ? rowLabel(n[0], n[1]) : null;
    } catch (_) { return null; }
  }
  // Barras da coluna da esquerda: XP do jogador, HP e XP do Pokémon em campo
  function leftBars(){
    const vw = innerWidth, xp = [], hp = [];
    for (const e of document.querySelectorAll('body *')) {
      const t = leafText(e);
      if (!t) continue;
      const r = e.getBoundingClientRect();
      if (!r.width || r.left > vw * 0.22) continue;
      if (/^[\d.,]+\s*\/\s*[\d.,]+\s*xp$/i.test(t)) xp.push([r.top, t.replace(/\s*xp$/i, '')]);
      else if (/^[\d.,]+\s*\/\s*[\d.,]+$/.test(t)) hp.push([r.top, t]);
    }
    xp.sort((a, b) => a[0] - b[0]); hp.sort((a, b) => a[0] - b[0]);
    return { xp, hp };
  }
  // Bônus da coluna da esquerda: linhas com "+NN% XP" (VIP, Guild, Twitch...)
  const PCT = /\+\s*\d+(?:[.,]\d+)?\s*%/;
  function leftBonus(){
    const vw = innerWidth, cand = [];
    for (const e of document.querySelectorAll('body *')) {
      const raw = e.textContent || '';
      if (raw.length > 120 || !PCT.test(raw)) continue; // filtro barato antes do innerText
      const r = e.getBoundingClientRect();
      if (!r.width || r.right > vw * 0.2 || r.height > 60) continue;
      const t = (e.innerText || '').replace(/\s+/g, ' ').trim();
      if (!t || t.length > 60 || !PCT.test(t)) continue;
      cand.push({ e, r, t });
    }
    // fica só com o elemento mais externo de cada linha (o "VIP +50% XP 30 dias", não o "+50% XP" de dentro)
    const keep = cand.filter(c => !cand.some(o => o !== c && o.e.contains(c.e)));
    const seen = {}, out = [];
    keep.sort((x, y) => x.r.top - y.r.top).forEach(c => { if (!seen[c.t]) { seen[c.t] = 1; out.push(c.t); } });
    return out.slice(0, 6);
  }
  function collectLeft(){
    const o = {};
    try { o.bon = leftBonus(); } catch (_) {}
    // cartão "Em campo" do jogo: #ativo-card (.ativo-nome, .pl-lv, .pk-selo-n, .barra.hp/.barra.xp)
    try {
      const ac = document.getElementById('ativo-card');
      const nomeEl = ac && ac.querySelector('.ativo-nome');
      if (nomeEl) {
        const lv = nomeEl.querySelector('.pl-lv');
        const sp = s => { const e = ac.querySelector(s); return e ? (e.textContent || '').trim() : ''; };
        o.pk = {
          name: ownText(nomeEl) || (nomeEl.textContent || '').replace(lv ? lv.textContent : '', '').trim(),
          lvl: ((lv && lv.textContent) || '').replace(/\D+/g, ''),
          n: sp('.pk-selo-n').replace(/^N\s*/i, ''),
          hp: sp('.barra.hp span'),
          xp: sp('.barra.xp span').replace(/\s*xp$/i, ''),
          img: '',
        };
      }
    } catch (_) {}
    try {
      const nv = leftNvs(), bars = leftBars();
      const num = t => (/(\d+)/.exec(t) || [])[1] || '';
      if (nv[0]) o.pl = { lvl: num(nv[0][2]), xp: bars.xp[0] ? bars.xp[0][1] : '' };
      if (nv[1] && !o.pk) {
        const top = nv[1][0].top;
        const hp = bars.hp.find(h => h[0] > top - 5);
        const xp = bars.xp.find(x => x[0] > top - 5 && (!bars.xp[0] || x !== bars.xp[0]));
        o.pk = { name: rowLabel(nv[1][0], nv[1][1]) || 'Pokémon', lvl: num(nv[1][2]), hp: hp ? hp[1] : '', xp: xp ? xp[1] : '', img: '' };
        // sprite = a MAIOR imagem do cartão "Em campo" (ícones de item/tipo são pequenos)
        { const r0 = nv[1][0]; let box = nv[1][1], best = null, area = 0;
          for (let k = 0; k < 5 && box.parentElement; k++) box = box.parentElement;
          for (const im of box.querySelectorAll('img')) {
            if (!/^https:/.test(im.src)) continue;
            const q = im.getBoundingClientRect(), a = q.width * q.height;
            if (q.width < 28 || Math.abs((q.top + q.height / 2) - (r0.top + r0.height / 2)) > 120) continue;
            if (a > area) { area = a; best = im; }
          }
          if (best) o.pk.img = best.src; }
      }
    } catch (_) {}
    return o;
  }
  // o nick só é lido até ser encontrado (2 leituras iguais seguidas); depois fica fixo até a página recarregar
  let lastNick = null, cand = null;
  function nickLoop(){
    if (!lastNick) {
      const n = kicked() ? null : findNick(); // com o aviso de "outro lugar" na tela, não confia na leitura
      if (n && n === cand && window.ipc) { lastNick = n; window.ipc.postMessage('nick:' + n); }
      cand = n;
      setTimeout(nickLoop, 4000);
    }
  }
  setTimeout(nickLoop, 3000); // assíncrono: as funções/constantes abaixo já existem quando rodar

  // --- Estatísticas do painel "Modo Economia" (para o painel de grade do app)
  function findPanel(){
    let h = null;
    for (const e of document.querySelectorAll('body *')) { if (/^modo economia$/i.test(leafText(e))) { h = e; break; } }
    if (!h) return null;
    const top = h.getBoundingClientRect().top;
    let best = h, p = h.parentElement;
    while (p && p !== document.body) {
      const r = p.getBoundingClientRect();
      if (r.width > innerWidth * 0.8 || r.top < top - 40) break;
      best = p; p = p.parentElement;
    }
    return best;
  }
  const LABELS = ['XP TREINADOR', 'XP POKÉMON', 'XP POKEMON', 'LUCRO LÍQUIDO', 'LUCRO LIQUIDO', 'ABATES', 'CAPTURAS'];
  const NUM = /^[−\-+]?[\d.,]+[kKmM]?$/;
  // aviso de "conta logada em outro dispositivo/aba"
  const KICK = /(logad[ao]|conectad[ao]|abert[ao]|aces?sad[ao]|entrou|sess[aã]o)\b[^.\n]{0,40}\b(em\s+)?outr[oa]\s+(dispositivo|aparelho|lugar|aba|navegador|janela|computador|local)|another (device|tab|location)/i;
  // só conta se a tela de aviso estiver VISÍVEL (o jogo deixa ela escondida na página o tempo todo)
  function shown(e){
    if (!e.getClientRects().length) return false;
    for (let x = e; x && x !== document.documentElement; x = x.parentElement) {
      const cs = getComputedStyle(x);
      if (cs.display === 'none' || cs.visibility === 'hidden' || parseFloat(cs.opacity) === 0) return false;
    }
    const r = e.getBoundingClientRect();
    return r.width > 20 && r.height > 20;
  }
  function kicked(){
    try {
      for (const e of document.querySelectorAll('body *')) {
        if (e.children.length > 2) continue;
        const t = e.textContent || '';
        if (t.length > 300 || !KICK.test(t)) continue;
        if (shown(e)) return t.replace(/\s+/g, ' ').trim().slice(0, 120);
      }
    } catch (_) {}
    return '';
  }
  // ---------- leitura pelos identificadores do próprio jogo (não depende da conta estar visível) ----------
  const Q = (sel, root) => (root || document).querySelector(sel);
  const QA = (sel, root) => [...(root || document).querySelectorAll(sel)];
  const TX = e => e ? (e.textContent || '').replace(/\s+/g, ' ').trim() : '';
  const ABS = u => { try { return u ? new URL(u, location.href).href : ''; } catch (_) { return ''; } };
  function collectIds(){
    if (!Q('#eco-placar')) return null;
    const out = { ok: true, src: 'ids' };
    out.kick = kicked();
    // treinador
    const lvl = TX(Q('#tr-level')), xpt = TX(Q('#tr-xp-txt')).replace(/\s*xp$/i, '');
    if (lvl || xpt) out.pl = { lvl: lvl.replace(/\D+/g, ''), xp: xpt };
    // Pokémon em campo
    const ac = Q('#ativo-card'), nomeEl = ac && Q('.ativo-nome', ac);
    if (nomeEl) {
      const lv = Q('.pl-lv', nomeEl);
      out.pk = { name: ownText(nomeEl) || TX(nomeEl).replace(TX(lv), '').trim(), lvl: TX(lv).replace(/\D+/g, ''),
                 n: TX(Q('.pk-selo-n', ac)).replace(/^N\s*/i, ''), hp: TX(Q('.barra.hp span', ac)), xp: TX(Q('.barra.xp span', ac)).replace(/\s*xp$/i, ''), img: '' };
    }
    // bônus ativos (lado esquerdo)
    out.bon = QA('#tr-ativos .tr-ativo').map(e => {
      const t = [...e.children].map(TX).filter(Boolean).join(' ') || TX(e);
      return (/conectar|vincular/i.test(e.className) && !/conectar|vincular/i.test(t) ? 'Conectar ' : '') + t;
    }).filter(Boolean).slice(0, 6);
    // sessão
    out.hunt = TX(Q('#eco-onde')) || TX(Q('#hud-hunt'));
    out.timer = TX(Q('.eco-tempo'));
    out.estado = TX(Q('.eco-estado'));
    const bt = TX(Q('.bxp-total b')); if (bt) out.bonus = bt;
    out.stats = QA('.eco-kpi').map(k => ({ l: TX(Q('.eco-kpi-rot', k)), v: TX(Q('.eco-kpi-v', k)), r: TX(Q('.eco-kpi-h', k)), s: TX(Q('.eco-kpi-sub', k)) })).filter(x => x.l && x.v);
    // achados: só os slots visíveis e com número
    out.found = [];
    out.foundIn = TX(Q('.eco-achados-lugar')).replace(/^[·\s]+/, '');
    for (const sl of QA('.eco-slot')) {
      if (sl.hidden || sl.hasAttribute('hidden')) continue;
      const c = TX(Q('b', sl));
      if (!c || c === '0') continue;
      const im = Q('img', sl);
      out.found.push({ c, n: sl.getAttribute('title') || '', img: im ? ABS(im.getAttribute('src')) : '', shiny: /shiny/.test(sl.className) });
    }
    // itens raros (aba) e o "+N" da sessão
    const rr = Q('.eco-cat[data-eco-cat="raros"]');
    if (rr) out.rare = { t: TX(Q('em', rr)), p: TX(Q('.eco-cat-mais', rr)).replace(/\D+/g, '') };
    // itens da bolsa (aba aberta: pokébolas, poções...)
    out.items = QA('.eco-grade-itens .eco-item').slice(0, 8).map(it => { const im = Q('img', it); return { c: TX(Q('.eco-item-q', it)).replace(/^[×x]\s*/, ''), n: it.getAttribute('title') || '', img: im ? ABS(im.getAttribute('src')) : '' }; }).filter(x => x.c);
    // gastos da sessão
    out.spent = QA('.eco-gasto').map(g => ({ c: TX(Q('b', g)), n: (g.getAttribute('title') || '').split(':')[0] }));
    // melhor captura = a linha marcada como recorde (maior Nota)
    const rec = Q('.capturas-linha.recorde') || Q('.capturas-linha');
    if (rec) out.best = { name: TX(Q('.cap-nome', rec)), nv: TX(Q('.cap-nv', rec)).replace(/\D+/g, ''), n: TX(Q('.pk-selo-n', rec)).replace(/^N\s*/i, ''),
                          iv: TX(Q('.pk-selo-iv', rec)).replace(/^IV\s*/i, ''), q: TX(Q('.pk-selo-q', rec)).replace(/^Q\s*/i, ''), p: TX(Q('.cm-pot', rec)), img: '' };
    return out;
  }
  function collect(){
    try { const o = collectIds(); if (o) return o; } catch (_) {}
    return collectOld();
  }
  function collectOld(){
    const out = { ok: false };
    out.kick = kicked();
    try {
      const P = findPanel();
      Object.assign(out, collectLeft());
      if (!P) { out.logged = !!document.querySelector('[class*="inventario" i], [class*="inventory" i]') || /abrir invent/i.test(document.body.innerText.slice(0, 4000)); return out; }
      out.ok = true;
      const txt = P.innerText || '';
      const lines = txt.split('\n').map(x => x.trim()).filter(Boolean);
      const hunt = lines.find(l => /n[ií]vel\s*\d+/i.test(l) && l.length < 60);
      if (hunt) out.hunt = hunt;
      const tm = /(\d+:\d{2}(?::\d{2})?)\s*ca[cç]ando/i.exec(txt); if (tm) out.timer = tm[1];
      const xp = /XP\s*\+?\s*([\d.,]+\s*%)/i.exec(txt); if (xp) out.bonus = '+' + xp[1].replace(/\s/g, '');
      out.stats = [];
      const seen = {};
      for (let k = 0; k < lines.length; k++) {
        const L = lines[k].toUpperCase();
        if (!LABELS.includes(L) || seen[L]) continue;
        const v = lines[k + 1] || '', r = lines[k + 2] || '', sub = lines[k + 3] || '';
        if (!NUM.test(v.replace(/\s/g, ''))) continue;
        seen[L] = 1;
        out.stats.push({ l: lines[k], v, r: /\/h$/.test(r) ? r : '', s: (sub && !LABELS.includes(sub.toUpperCase()) && !/^(achados|gastos|capturas|itens)\b/i.test(sub)) ? sub : '' });
      }
      // Achados (drops raros da sessão)
      out.found = [];
      try {
        let hd = null;
        for (const e of P.querySelectorAll('*')) { if (/^achados\b/i.test(leafText(e) || ownText(e))) { hd = e; break; } }
        if (hd) {
          out.foundIn = leafText(hd).replace(/^achados\s*[·•\-]?\s*/i, '');
          // área dos achados: abaixo do título, à esquerda do título "GASTOS"
          const hr = hd.getBoundingClientRect();
          let xmax = hr.left + 320;
          for (const e of P.querySelectorAll('*')) { if (/^gastos\b/i.test(leafText(e) || ownText(e))) { const g = e.getBoundingClientRect(); if (g.left > hr.left) xmax = g.left - 4; break; } }
          for (const img of P.querySelectorAll('img')) {
            const q = img.getBoundingClientRect();
            if (!q.width || q.left < hr.left - 8 || q.right > xmax || q.top < hr.bottom - 2 || q.top > hr.bottom + 140) continue;
            let tile = img.parentElement, cnt = '';
            for (let k = 0; k < 3 && tile; k++, tile = tile.parentElement) {
              const m = /(\d+)\s*$/.exec((tile.innerText || '').trim()); if (m) { cnt = m[1]; break; }
            }
            const src = /^(https?:|data:image\/)/.test(img.src) ? img.src : '';
            out.found.push({ img: src, n: img.alt || img.title || '', c: cnt || '1' });
            if (out.found.length >= 8) break;
          }
        }
      } catch (_) {}
      // "Itens Raros N" (+M desde que ligou)
      try {
        for (const e of document.querySelectorAll('body *')) {
          if (!/^itens raros$/i.test(leafText(e))) continue;
          let p = e.parentElement; let t = '';
          for (let k = 0; k < 3 && p; k++, p = p.parentElement) { t = (p.innerText || '').replace(/\s+/g, ' '); if (/\d/.test(t)) break; }
          const total = /itens raros\s*(\d+)/i.exec(t), plus = /\+\s*(\d+)/.exec(t);
          out.rare = { t: total ? total[1] : '', p: plus ? plus[1] : '' };
          break;
        }
      } catch (_) {}
      // Melhor captura da sessão (maior Nota "N x,xxx" da lista de capturas)
      try {
        const nval = t => parseFloat(String(t).replace(/^N\s*/i, '').replace(/\./g, '').replace(',', '.')) || 0;
        let best = null;
        for (const e of P.querySelectorAll('*')) {
          const t = leafText(e);
          if (!/^N\s*[\d.,]+$/.test(t)) continue;
          // sobe até o "cartão" da captura (que tem nome + Nv + IV)
          let card = e.parentElement;
          for (let k = 0; k < 5 && card; k++, card = card.parentElement) {
            const it = card.innerText || '';
            if (/Nv\s*\d+/.test(it) && /IV\s*\d+/.test(it)) break;
          }
          if (!card) continue;
          const lines = (card.innerText || '').split('\n').map(x => x.trim()).filter(Boolean);
          const g = re => { const l = lines.find(x => re.test(x)); return l ? l : ''; };
          const name = lines.find(x => /^[A-Za-zÀ-ÿ][A-Za-zÀ-ÿ .'\-]{1,30}$/.test(x) && !/^(Nv|IV|Q|N|P\d)\b/.test(x)) || '';
          const nv = (/Nv\s*(\d+)/.exec(card.innerText) || [])[1] || '';
          const cap = { name, nv, n: t.replace(/^N\s*/i, ''), iv: g(/^IV\s*\d+/).replace(/^IV\s*/, ''), q: g(/^Q\s*[\d.,]+/).replace(/^Q\s*/, ''),
                        p: g(/^P\d+$/), img: '' };
          const im = card.querySelector('img'); if (im && /^(https?:|data:image\/)/.test(im.src)) cap.img = im.src;
          cap.v = nval(t);
          if (!best || cap.v > best.v) best = cap;
        }
        if (best) { delete best.v; out.best = best; }
      } catch (_) {}
      out.items = [];
      for (const e of P.querySelectorAll('*')) {
        const t = leafText(e);
        if (!/^[×x]\s*[\d.,]+$/.test(t)) continue;
        let p = e.parentElement, img = null;
        for (let k = 0; k < 3 && p && !img; k++, p = p.parentElement) img = p.querySelector('img');
        let src = img ? img.src : '';
        if (!/^(https?:|data:image\/)/.test(src)) src = '';
        out.items.push({ c: t.replace(/^[×x]\s*/, ''), img: src, n: img ? (img.alt || img.title || '') : '' });
        if (out.items.length >= 8) break;
      }
    } catch (e) { out.err = String(e).slice(0, 80); }
    return out;
  }
  window.__hubDump = function(){ if (window.ipc) window.ipc.postMessage('dump:' + document.documentElement.outerHTML); };
  let lastStats = '', watch = false, timer = null;
  function statsTick(){
    if (window.ipc) {
      const j = JSON.stringify(collect());
      if (j !== lastStats) { lastStats = j; window.ipc.postMessage('stats:' + j); }
    }
    timer = setTimeout(statsTick, watch ? 2000 : 20000); // grade aberta: 3 s; fechada: 20 s (só p/ alerta de drop)
  }
  window.__hubWatch = function(on){
    on = !!on; if (on === watch) return; watch = on;
    clearTimeout(timer); timer = setTimeout(statsTick, on ? 50 : 20000);
  };
  timer = setTimeout(statsTick, 3000);
})();"#;

// Comandos da barra viram uma "navegação" falsa para https://hub.cmd/..., que o app intercepta e cancela.
const CMD_PREFIX: &str = "https://hub.cmd/";

fn toolbar_html(names: &[String], insta: bool) -> String {
    let up = r#"<button id="up" onclick="send('update')" title="Baixar a versão nova no GitHub" style="display:none;background:#2f8f4e;color:#fff">⬆ Nova versão</button>"#;
    let insta = if insta { format!(r#"<span class="sp"></span><button id="ig" onclick="send('insta')" title="Me segue no Instagram: @{INSTA}"><span class="ap">quer me<br>apoiar?</span><span class="ic"><svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="18" height="18" rx="5"/><circle cx="12" cy="12" r="4"/><circle cx="17.5" cy="6.5" r="0.6" fill="currentColor"/></svg></span></button>"#) } else { String::new() };
    let names: Vec<String> = names.to_vec();
    let tabs: String = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let n = n.replace('<', "&lt;").replace('>', "&gt;");
            format!(r#"<button class="tab" id="t{i}" onclick="send('tab:{i}')" ondblclick="ren({i})" title="Ctrl+{k} · duplo clique para renomear"><span class="k">{k}</span><span class="nm" id="n{i}">{n}</span><span class="r" title="Recarregar esta conta" onclick="event.stopPropagation();send('reload:{i}')">↻</span></button>"#, k = i + 1)
        })
        .collect();
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{{box-sizing:border-box}}
html,body{{margin:0;height:100%;background:#1b1418;color:#eee;font:13px 'Segoe UI',system-ui,sans-serif;overflow:hidden;user-select:none}}
.bar{{display:flex;align-items:center;gap:6px;height:100%;padding:0 8px}}
.brand{{font-weight:700;color:#f5c542;margin-right:6px;white-space:nowrap}}
button{{border:0;border-radius:6px;background:#3a2a33;color:#ddd;height:28px;padding:0 10px;font:inherit;cursor:pointer;display:flex;align-items:center;gap:6px;white-space:nowrap}}
button:hover{{background:#4b3642}}
.tab.on{{background:#7b4fd6;color:#fff}}
#rt.on{{background:#e05a5a;color:#fff}}
#ig{{background:transparent;padding:0 4px;gap:7px;color:#8f7a8a}}
#ig:hover{{background:transparent;color:#e9d9e4}}
#ig .ap{{font:600 11px/12px 'Segoe UI',sans-serif;text-align:right;letter-spacing:.02em;color:#ff9fd8}}
#ig:hover .ap{{color:#ffd1ec}}
#ig .ic{{display:flex;align-items:center;justify-content:center;width:24px;height:24px;border-radius:7px;color:#fff;background:linear-gradient(135deg,#7b4fd6,#c13584 60%,#f5a142);opacity:.85;transition:transform .15s,opacity .15s}}
#ig:hover .ic{{opacity:1;transform:scale(1.08)}}
.tab.drop{{box-shadow:0 0 0 2px #f5c542 inset}}
.tab.drop .nm::after{{content:' ★';color:#f5c542}}
.tab.kick{{background:#7a1f2b;color:#fff;box-shadow:0 0 0 2px #ff6b6b inset}}
.tab.kick .nm::before{{content:'⚠ '}}
.k{{font-size:11px;opacity:.6}}
.r{{opacity:.55;padding:0 2px}} .r:hover{{opacity:1}}
.sp{{flex:1}}
#g.on{{background:#d6a63a;color:#1b1418}}
</style></head><body><div class="bar">
<span class="brand">PokeIdle Hub</span>{tabs}
{insta}{up}<span class="sp"></span>
<button id="twx" onclick="send('twcancel')" style="display:none;background:#9146ff;color:#fff">✕ Fechar</button>
<button id="g" onclick="send('grid')" title="Ctrl+G">▦ Grade</button>
<button id="rt" onclick="send('rotom')" title="Rotom Sniper da conta aberta (Ctrl+P)">🎯 Rotom</button>
<button onclick="send('reload:all')" title="Recarregar todas">↻ Todas</button>
<button id="dm" onclick="send('dmode')" title="Como seu nick aparece no Discord (clique para trocar)">Discord: mostrar</button>
<button onclick="send('taskmgr')" title="Gerenciador do motor: RAM e CPU de cada página (contas, lives da Kick...)">📊</button>
<button onclick="send('devtools')" title="Salvar diagnóstico (cópia da página) para o suporte">🐞</button>
<button onclick="send('config')" title="Abrir pasta de configuração">⚙</button>
</div><script>
function send(m){{
  var t=/^tab:(\d+)$/.exec(m); if(t) window.__state(+t[1],false);
  if(m==='grid') window.__state(window.__a||0,!window.__g);
  window.ipc.postMessage(m);
}}
window.__state=function(active,grid){{
  window.__a=active; window.__g=grid;
  document.querySelectorAll('.tab').forEach((b,i)=>b.classList.toggle('on',!grid&&i===active));
  document.getElementById('g').classList.toggle('on',grid);
}};
window.__update=function(tag){{var b=document.getElementById('up');b.textContent='⬆ Nova versão '+tag;b.style.display='flex';}};
window.__rotomMsg=function(){{var b=document.getElementById('rt');b.textContent='🎯 sem janela';setTimeout(function(){{b.textContent='🎯 Rotom'}},2500);}};
window.__rotom=function(on){{document.getElementById('rt').classList.toggle('on',!!on);}};
window.__dmode=function(t){{document.getElementById('dm').textContent='Discord: '+t;}};
window.__twl=function(on){{document.getElementById('twx').style.display=on?'flex':'none';}};
window.__kick=function(i,on){{var b=document.getElementById('t'+i); if(b) b.classList.toggle('kick',!!on);}};
window.__drop=function(i,on){{var b=document.getElementById('t'+i); if(b) b.classList.toggle('drop',!!on);}};
window.__setName=function(i,n){{var e=document.getElementById('n'+i); if(e&&!e.querySelector('input')) e.textContent=n;}};
function ren(i){{
  var sp=document.getElementById('n'+i); if(sp.querySelector('input')) return;
  var old=sp.textContent, inp=document.createElement('input');
  inp.value=old; inp.style.cssText='width:110px;font:inherit;background:#1b1418;color:#fff;border:1px solid #7b4fd6;border-radius:4px;padding:1px 4px';
  sp.textContent=''; sp.appendChild(inp); inp.focus(); inp.select();
  var done=false;
  function fin(save){{ if(done) return; done=true; var v=inp.value.trim(); sp.textContent=save?(v||old):old; if(save) send('rename:'+i+':'+encodeURIComponent(v)); }}
  inp.onkeydown=function(e){{ e.stopPropagation(); if(e.key==='Enter') fin(true); else if(e.key==='Escape') fin(false); }};
  inp.onclick=function(e){{e.stopPropagation()}};
  inp.onblur=function(){{fin(true)}};
}}
window.addEventListener('keydown',e=>{{
  if(e.ctrlKey&&e.key>='1'&&e.key<='9'){{send('tab:'+(+e.key-1));e.preventDefault()}}
  else if(e.ctrlKey&&(e.key==='g'||e.key==='G')){{send('grid');e.preventDefault()}}
  else if(e.ctrlKey&&(e.key==='p'||e.key==='P')){{send('rotom');e.preventDefault()}}
}});
</script></body></html>"#
    )
}

const DASH_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box}
html,body{margin:0;height:100%;background:#140f13;color:#eee;font:14px 'Segoe UI',system-ui,sans-serif;overflow:hidden}
#g{display:grid;grid-template-columns:1fr 1fr;grid-auto-rows:1fr;gap:10px;height:100%;padding:10px}
.c{background:#2a1d25;border:2px solid #4b3642;border-radius:12px;padding:10px 14px;display:flex;flex-direction:column;gap:8px;cursor:pointer;min-height:0;overflow:auto;transition:border-color .15s}
.c::-webkit-scrollbar{width:6px}.c::-webkit-scrollbar-thumb{background:#4b3642;border-radius:3px}
.c:hover{border-color:#7b4fd6}
.h{display:flex;align-items:baseline;gap:10px;flex-wrap:wrap}
.num{font-size:12px;color:#a58ea0;background:#3a2a33;border-radius:5px;padding:1px 7px}
.nick{font-size:20px;font-weight:700;color:#f5c542}
.hunt{color:#d8c7d2}
.sp{flex:1}
.tm{font-weight:600;color:#7ee08a}
.tm::before{content:'';display:inline-block;width:8px;height:8px;border-radius:50%;background:#3fbf55;margin-right:6px;vertical-align:1px}
.bonus{color:#b9a2ff;font-weight:600}
.st{display:grid;grid-template-columns:repeat(5,1fr);gap:8px}
.s{background:#1b1418;border-radius:8px;padding:8px 10px;min-width:0}
.s .l{font-size:11px;letter-spacing:.04em;color:#a58ea0;text-transform:uppercase;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.s .v{font-size:clamp(16px,2vw,24px);font-weight:700;margin-top:2px;white-space:nowrap}
.s .r{font-size:13px;color:#e0a5ff;white-space:nowrap}
.s .x{font-size:11px;color:#9a8a95;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.neg .v,.neg .r{color:#ff7a7a}
.pos .v{color:#8be28f}
.it{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
.row{display:flex;gap:18px;flex-wrap:wrap;align-items:center}
.i{display:flex;align-items:center;gap:6px;background:#1b1418;border-radius:8px;padding:5px 10px 5px 6px;font-weight:700}
.i img{width:28px;height:28px;image-rendering:pixelated}
.i.low{outline:2px solid #ff7a7a;color:#ff9a9a}
.bars{display:grid;grid-template-columns:repeat(3,1fr);gap:8px}
.b{background:#1b1418;border-radius:8px;padding:7px 10px;min-width:0}
.b .t{display:flex;justify-content:space-between;gap:8px;font-size:12px;color:#d8c7d2;white-space:nowrap;overflow:hidden}
.b .t>span:first-child{overflow:hidden;text-overflow:ellipsis}
.b .t b{color:#fff}
.bar{height:8px;border-radius:4px;background:#3a2a33;margin-top:5px;overflow:hidden}
.bar>i{display:block;height:100%;border-radius:4px}
.bar.xp>i{background:linear-gradient(90deg,#4aa3ff,#7ec8ff)}
.bar.hp>i{background:linear-gradient(90deg,#3fbf55,#8be28f)}
.bar.hp.lowhp>i{background:#ff6b6b}
.bon{display:flex;gap:6px;flex-wrap:wrap}
.chip{font-size:11px;background:#33244a;color:#d9c8ff;border:1px solid #5b3f8f;border-radius:999px;padding:1px 8px;white-space:nowrap}
.chip.dim{background:transparent;color:#7d6b78;border-color:#3a2a33}
.fd{display:flex;gap:8px;align-items:center;flex-wrap:wrap}
.fd .lbl{font-size:11px;letter-spacing:.04em;color:#a58ea0;text-transform:uppercase}
.fd .i{background:#2e2410;border:1px solid #6b5520}
.rare{font-size:12px;color:#f5c542;font-weight:600}
.c.alert{border-color:#f5c542;box-shadow:0 0 18px rgba(245,197,66,.35)}
.c.kicked{border-color:#ff6b6b;box-shadow:0 0 18px rgba(255,107,107,.35)}
.warn{background:#5a1a24;color:#ffd1d1;border:1px solid #ff6b6b;border-radius:8px;padding:6px 10px;font-size:13px;font-weight:600}
.newdrop{background:#f5c542;color:#1b1418;font-weight:800;border-radius:6px;padding:2px 10px;font-size:13px}
.tw{display:flex;align-items:center;gap:8px;font-size:12px}
.tw .dot{width:8px;height:8px;border-radius:50%;background:#6f5f69;display:inline-block}
.tw.ok .dot{background:#3fbf55}.tw.wait .dot{background:#f5c542}.tw.bad .dot{background:#ff6b6b}
.tw b{color:#c9b3ff}
.twbtn{background:#9146ff;color:#fff;border:0;border-radius:6px;padding:3px 10px;font:inherit;font-size:12px;font-weight:600;cursor:pointer}
.twbtn:hover{background:#a970ff}
.best{display:flex;align-items:center;gap:10px;background:#1b1418;border:1px solid #6b5520;border-radius:8px;padding:6px 12px;font-size:13px}
.best img{width:40px;height:40px;image-rendering:pixelated}
.best .bn{font-weight:700;color:#fff}
.best .nota{font-weight:800;color:#f5c542;font-size:15px}
.best .tag{font-size:11px;color:#a58ea0;background:#2a1d25;border-radius:4px;padding:1px 6px}
.best .lbl{font-size:11px;letter-spacing:.04em;color:#a58ea0;text-transform:uppercase}
.off{color:#a58ea0;margin:auto;text-align:center;line-height:1.6}
.age{font-size:11px;color:#6f5f69}
</style></head><body><div id="g"></div><script>
var NAMES=__NAMES__, KICKON=__KICK__, DATA=[], SEEN=[], BASE=[], ALERT=[], TW=[], KK=[], KKN=[];
function kkLine(i){
  var w=el('div','tw'), t=KK[i];
  var b=el('button','twbtn',t?'Trocar conta':'Conectar Kick'); b.style.background=t?'transparent':'#53fc18'; b.style.color=t?'#9be37a':'#0b0e0f'; if(t){b.style.border='1px solid #3a7a1e';}
  b.onclick=function(e){e.stopPropagation(); window.ipc.postMessage('kicklogin:'+i);};
  w.appendChild(el('span','dot'));
  if(!t){ w.appendChild(el('span',null,'Kick: não conectada')); w.appendChild(b); return w; }
  var n=KKN[i]||1, on=[], wait=false;
  for(var k=0;k<n;k++){ var x=t[k]; if(!x) continue; if(x.st==='esperando') wait=true; if(x.ch && x.st==='ok') on.push(x.ch+(x.q?' · '+x.q:'')); }
  var lab = on.length ? 'assistindo '+on.join(', ')+' ('+on.length+')' : (wait ? 'nenhum canal ao vivo, esperando' : 'abrindo lives…');
  w.className='tw '+(on.length?'ok':'wait');
  var s=el('span'); var kb=el('b',null,'Kick '); kb.style.color='#53fc18'; s.appendChild(kb); s.appendChild(document.createTextNode(lab)); w.appendChild(s);
  var vb=el('button','twbtn','👁 Ver'); vb.title='Mostrar a live aqui (para resgatar pontos). Feche no ✕ lá em cima.'; vb.style.background='#1f3a12'; vb.style.color='#9be37a';
  vb.onclick=function(e){e.stopPropagation(); window.ipc.postMessage('kickshow:'+i);}; w.appendChild(vb);
  w.appendChild(b);
  return w;
}
function sdSprite(n){ var id=String(n||'').toLowerCase().normalize('NFD').replace(/[\u0300-\u036f]/g,'').replace(/♀/g,'f').replace(/♂/g,'m').replace(/[^a-z0-9]/g,''); return id?'https://play.pokemonshowdown.com/sprites/ani/'+id+'.gif':''; }
// números no formato do jogo: "5.397.479", "−197.020", "7.406.851/h", "0,0/h"
function pnum(x){ var t=String(x||'').replace(/\/h.*$/,'').replace(/\s/g,''); var neg=/^[−-]/.test(t); t=t.replace(/^[−+-]/,'').replace(/\./g,'').replace(',','.'); var v=parseFloat(t)||0; return neg?-v:v; }
function fmtN(v){ var r=Math.round(v), s=Math.abs(r).toLocaleString('pt-BR'); return (r<0?'−':'')+s; }
function tsec(t){ var p=String(t).split(':').map(Number); var s=0; p.forEach(function(x){ s=s*60+(x||0); }); return s; }
function fmtT(s){ s=Math.floor(s); var h=Math.floor(s/3600), m=Math.floor(s%3600/60), x=s%60; function z(n){return (n<10?'0':'')+n;} return h? h+':'+z(m)+':'+z(x) : m+':'+z(x); }
setInterval(function(){
  var now=Date.now();
  document.querySelectorAll('.v[data-rate]').forEach(function(e){
    var dt=(now-(+e.getAttribute('data-t0')))/3600000; e.textContent=fmtN(+e.getAttribute('data-base') + (+e.getAttribute('data-rate'))*dt);
  });
  document.querySelectorAll('.tm[data-sec]').forEach(function(e){
    e.textContent=fmtT(+e.getAttribute('data-sec') + (now-(+e.getAttribute('data-t0')))/1000);
  });
  document.querySelectorAll('.b[data-rate]').forEach(function(w){
    var a=+w.getAttribute('data-xa'), b=+w.getAttribute('data-xb'), dt=(now-(+w.getAttribute('data-t0')))/3600000;
    var cur=Math.min(b, a + (+w.getAttribute('data-rate'))*dt), pct=Math.max(0,Math.min(100,cur/b*100));
    var sp=w.querySelector('.t>span:last-child'); if(sp) sp.textContent=fmtN(cur)+' / '+fmtN(b)+'  ·  '+pct.toFixed(1).replace('.',',')+'%';
    var f=w.querySelector('.bar>i'); if(f) f.style.width=pct+'%';
  });
}, 1000);
function twLine(i){
  var t=TW[i], w=el('div','tw');
  function btn(txt){ var b=el('button','twbtn',txt); b.onclick=function(e){e.stopPropagation(); window.ipc.postMessage('twlogin:'+i);}; return b; }
  if(!t){ w.appendChild(el('span','dot')); w.appendChild(el('span',null,'Twitch: não conectada')); w.appendChild(btn('Conectar Twitch')); return w; }
  var label={ok:'conectada',entrando:'entrando nos chats…',conectando:'conectando…',caiu:'reconectando…',auth:'login expirou'}[t.st]||t.st;
  w.className='tw '+(t.st==='ok'?'ok':(t.st==='auth'?'bad':'wait'));
  w.appendChild(el('span','dot'));
  var s=el('span'); s.appendChild(el('b',null,'Twitch ')); s.appendChild(document.createTextNode(label+(t.st==='ok'?' · '+t.j+'/'+t.t+' canais':''))); w.appendChild(s);
  if(t.st==='auth') w.appendChild(btn('Entrar de novo'));
  else { var tb=btn('Trocar conta'); tb.style.background='transparent'; tb.style.border='1px solid #5b3f8f'; tb.style.color='#c9b3ff'; w.appendChild(tb); }
  return w;
}
function foundSig(d){ if(!d||!d.rare||d.rare.t==='') return null; var n=parseInt(d.rare.t,10); return isNaN(n)?null:n; }
var g=document.getElementById('g');
function el(t,c,txt){var e=document.createElement(t); if(c) e.className=c; if(txt!=null) e.textContent=txt; return e;}
function render(i){
  var c=document.getElementById('c'+i);
  if(!c){ c=el('div','c'); c.id='c'+i; c.onclick=function(){window.ipc.postMessage('tab:'+i)}; g.appendChild(c); }
  c.textContent='';
  var d=DATA[i]||{};
  c.classList.toggle('alert',!!ALERT[i]);
  c.classList.toggle('kicked',!!(d.kick));
  var h=el('div','h'); h.appendChild(el('span','num',String(i+1))); h.appendChild(el('span','nick',NAMES[i]||('Conta '+(i+1))));
  if(d.hunt) h.appendChild(el('span','hunt',d.hunt));
  h.appendChild(el('span','sp'));
  if(ALERT[i]) h.appendChild(el('span','newdrop','★ Novo drop raro!'));
  if(d.bonus) h.appendChild(el('span','bonus','XP '+d.bonus));
  if(d.timer){ var tme=el('span','tm',d.timer); tme.setAttribute('data-sec',String(tsec(d.timer))); tme.setAttribute('data-t0',String(SEEN[i]||Date.now())); h.appendChild(tme); }
  c.appendChild(h);
  if(d.kick) c.appendChild(el('div','warn','⚠ Conta desconectada: aberta em outro lugar. O farm desta conta pode ter parado. Clique para abrir.'));
  var dup=[]; NAMES.forEach(function(n,j){ if(j!==i && n && n===NAMES[i] && !/^Conta \d+$/.test(n)) dup.push(j+1); });
  if(dup.length) c.appendChild(el('div','warn','⚠ Mesma conta da aba '+dup.join(', ')+': uma derruba a outra. Entre com outro alt numa delas.'));
  c.appendChild(twLine(i));
  if(KICKON) c.appendChild(kkLine(i));
  if(d.bon&&d.bon.length){
    var bn=el('div','bon');
    d.bon.forEach(function(t){ bn.appendChild(el('span','chip'+(/conectar|vincular|ativar|assista/i.test(t)?' dim':''),t)); });
    c.appendChild(bn);
  }
  var bars=el('div','bars');
  function num(x){return parseFloat(String(x).replace(/\./g,'').replace(',','.'))||0;}
  function bar(title,val,kind,rate){
    var w=el('div','b'), t=el('div','t'), parts=String(val||'').split('/');
    var a=num(parts[0]), b=num(parts[1]), pct=b>0?Math.max(0,Math.min(100,a/b*100)):0;
    var lb=el('span'); lb.appendChild(el('b',null,title)); t.appendChild(lb);
    var vs=el('span',null,val?(fmtN(a)+' / '+fmtN(b)+'  ·  '+pct.toFixed(1).replace('.',',')+'%'):'—');
    t.appendChild(vs);
    w.appendChild(t);
    var bb=el('div','bar '+kind+(kind==='hp'&&pct<30?' lowhp':'')), fill=el('i'); fill.style.width=pct+'%'; bb.appendChild(fill); w.appendChild(bb);
    if(rate>0 && b>0){ w.setAttribute('data-xa',String(a)); w.setAttribute('data-xb',String(b)); w.setAttribute('data-rate',String(rate)); w.setAttribute('data-t0',String(SEEN[i]||Date.now())); }
    return w;
  }
  // "upa em X": usa o texto do jogo ("Nv 152 em 8m 20s"); se faltar, calcula pelo XP/h
  function stat(re){ var r=null; (d.stats||[]).forEach(function(s){ if(!r&&re.test(s.l)) r=s; }); return r; }
  function fmtH(h){ if(!(h>0)||!isFinite(h)) return ''; var m=Math.round(h*60); if(m<1) return 'menos de 1m'; var hh=Math.floor(m/60), mm=m%60; return hh? hh+'h '+mm+'m' : mm+'m'; }
  function eta(st,val){
    if(st&&st.s){ var m=/\bem\s+(.+)$/i.exec(st.s); if(m) return m[1]; }
    var p=String(val||'').split('/'), a=num(p[0]), b=num(p[1]), r=st?num(String(st.r).replace(/\/h.*$/,'')):0;
    return (b>a&&r>0)? fmtH((b-a)/r) : '';
  }
  var eT=eta(stat(/treinador/i), d.pl&&d.pl.xp), eP=eta(stat(/pok[eé]mon/i), d.pk&&d.pk.xp);
  var sT=stat(/treinador/i), sP=stat(/pok[eé]mon/i);
  if(d.pl) bars.appendChild(bar('Treinador Nv '+(d.pl.lvl||'?')+(eT?' · upa em '+eT:''),d.pl.xp,'xp',sT?pnum(sT.r):0));
  if(d.pk){
    bars.appendChild(bar((d.pk.name||'Pokémon')+' Nv '+(d.pk.lvl||'?')+(eP?' · upa em '+eP:''),d.pk.xp,'xp',sP?pnum(sP.r):0));
    if(d.pk.hp){ var hpw=bar((d.pk.name||'Pokémon')+' · HP',d.pk.hp,'hp'); bars.appendChild(hpw); }
  }
  if(bars.children.length) c.appendChild(bars);
  if(!d.ok){
    c.appendChild(el('div','off', DATA[i]===undefined ? 'Carregando…' : 'Fora do Modo Economia\nclique para abrir a conta'));
    return;
  }
  var st=el('div','st');
  (d.stats||[]).forEach(function(s){
    var neg=/^[−-]/.test(s.v), b=el('div','s'+(/lucro/i.test(s.l)?(neg?' neg':' pos'):''));
    b.appendChild(el('div','l',s.l));
    var vv=el('div','v',s.v);
    // conta ao vivo: valor base + taxa/h × tempo desde a leitura (capturas ficam exatas)
    if(s.r && !/captura/i.test(s.l)){ vv.setAttribute('data-base',String(pnum(s.v))); vv.setAttribute('data-rate',String(pnum(s.r))); vv.setAttribute('data-t0',String(SEEN[i]||Date.now())); }
    b.appendChild(vv);
    if(s.r) b.appendChild(el('div','r',s.r)); if(s.x||s.s) b.appendChild(el('div','x',s.s||''));
    st.appendChild(b);
  });
  c.appendChild(st);
  if(d.best&&d.best.n){
    var bc=el('div','best'); bc.appendChild(el('span','lbl','Melhor captura'));
    var bsrc=d.best.img||sdSprite(d.best.name); if(bsrc){var bim=document.createElement('img'); bim.src=bsrc; bim.onerror=function(){this.remove()}; bc.appendChild(bim);}
    bc.appendChild(el('span','bn',(d.best.name||'?')+(d.best.nv?' Nv '+d.best.nv:'')));
    bc.appendChild(el('span','nota','Nota '+d.best.n));
    if(d.best.iv) bc.appendChild(el('span','tag','IV '+d.best.iv));
    if(d.best.q) bc.appendChild(el('span','tag','Q '+d.best.q));
    if(d.best.p) bc.appendChild(el('span','tag',d.best.p));
    c.appendChild(bc);
  }
  var row=el('div','row');
  if(d.items&&d.items.length){
    var it=el('div','it');
    d.items.forEach(function(x){
      var n=parseInt(String(x.c).replace(/[.,]/g,''),10);
      var b=el('div','i'+(n<100?' low':'')); b.title=x.n||'';
      if(x.img){var im=document.createElement('img'); im.src=x.img; b.appendChild(im);}
      b.appendChild(el('span',null,'×'+x.c)); it.appendChild(b);
    });
    row.appendChild(it);
  }
  if((d.found&&d.found.length)||(d.rare&&d.rare.t)){
    var fd=el('div','fd'); fd.appendChild(el('span','lbl','Achados'+(d.foundIn?' · '+d.foundIn:'')));
    (d.found||[]).forEach(function(x){
      var b=el('div','i'); b.title=x.n||'';
      if(x.img){var im=document.createElement('img'); im.src=x.img; b.appendChild(im);}
      b.appendChild(el('span',null,'×'+x.c)); fd.appendChild(b);
    });
    if(d.rare&&d.rare.t) fd.appendChild(el('span','rare','Itens raros: '+d.rare.t+(d.rare.p?' (+'+d.rare.p+' nesta sessão)':'')));
    row.appendChild(fd);
  }
  if(row.children.length) c.appendChild(row);
}
var KEEP=['stats','items','pl','pk','bon','found','rare','hunt','timer','bonus'];
function emptyVal(v){ return v===undefined||v===null||v===''||(Array.isArray(v)&&v.length===0); }
var KICKED=[];
window.__upd=function(i,d){
  var old=DATA[i];
  var k=!!(d&&d.kick); if(k!==!!KICKED[i]){ KICKED[i]=k; window.ipc.postMessage('kick:'+i+':'+(k?1:0)); }
  if(old&&old.ok&&d&&d.ok){
    // o jogo às vezes redesenha o painel no meio da leitura: mantém o último valor bom
    KEEP.forEach(function(k){ if(emptyVal(d[k])&&!emptyVal(old[k])) d[k]=old[k]; });
  }
  var sig=foundSig(d);
  if(sig!==null){
    if(BASE[i]===undefined) BASE[i]=sig;
    else if(sig>BASE[i]){ BASE[i]=sig; ALERT[i]=true; window.ipc.postMessage('drop:'+i); }
    else if(sig<BASE[i]) BASE[i]=sig; // sessão resetada
  }
  SEEN[i]=Date.now(); DATA[i]=d; render(i);
};
window.__clear=function(i){ALERT[i]=false; render(i);};
window.__tw=function(i,t){TW[i]=t; render(i);};
window.__kk=function(i,t){ if(t&&t.lives) { if(!KK[i]) KK[i]={}; render(i); return; } if(!KK[i]) KK[i]={}; KK[i][(t&&t.slot)||0]=t; render(i);};
window.__kkN=function(i,n){KKN[i]=n; if(KK[i]) for(var k in KK[i]) if(+k>=n) delete KK[i][k]; render(i);};
window.__setName=function(i,n){NAMES[i]=n; for(var j=0;j<NAMES.length;j++) render(j);};
for(var i=0;i<NAMES.length;i++) render(i);
if(NAMES.length<=1) g.style.gridTemplateColumns='1fr';
if(NAMES.length>4) g.style.gridTemplateColumns='1fr 1fr 1fr';
</script></body></html>"#;

struct Layout {
    active: usize,
    grid: bool,
}

fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
    Rect { position: PhysicalPosition::new(x, y).into(), size: PhysicalSize::new(w.max(1), h.max(1)).into() }
}

const ROTOM_ID: &str = "olccbpdjdkbfgondiiaicmlifhbcggmk";

/// Página do botão da extensão (popup) lida do manifest copiado: "popup.html" etc.
fn ext_popup(folder: &str) -> Option<String> {
    let m = std::fs::read_to_string(base_dir().join("extensoes").join(folder).join("manifest.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(m.trim_start_matches('\u{feff}')).ok()?;
    ["action", "browser_action", "page_action"].iter()
        .find_map(|k| v.get(*k).and_then(|a| a.get("default_popup")).and_then(|x| x.as_str()))
        .or_else(|| v.get("side_panel").and_then(|o| o.get("default_path")).and_then(|x| x.as_str()))
        .or_else(|| v.get("options_ui").and_then(|o| o.get("page")).and_then(|x| x.as_str()))
        .or_else(|| v.get("options_page").and_then(|x| x.as_str()))
        .map(|p| p.trim_start_matches('/').to_string())
}

/// Posição do painel da Rotom: canto superior direito, abaixo da barra.
fn rotom_rect(window: &tao::window::Window) -> Rect {
    let size = window.inner_size();
    let scale = window.scale_factor();
    let tb = (TOOLBAR_H * scale).round() as u32;
    // tamanho do Cockpit da Rotom (1160x760), limitado ao tamanho da janela
    let w = ((1160.0 * scale).round() as u32).min(size.width);
    let h = ((760.0 * scale).round() as u32).min(size.height.saturating_sub(tb));
    rect(size.width.saturating_sub(w) as i32, tb as i32, w, h)
}

fn apply_layout(window: &tao::window::Window, toolbar: &WebView, dash: &WebView, games: &[WebView], lay: &Layout, cfg: &Config, twl: Option<&WebView>) {
    let t = std::time::Instant::now();
    logf!("layout: inicio active={} grid={} twitch_login={}", lay.active, lay.grid, twl.is_some());
    apply_layout_inner(window, toolbar, dash, games, lay, cfg);
    let _ = toolbar.evaluate_script(&format!("window.__twl&&window.__twl({})", twl.is_some()));
    if let Some(l) = twl {
        // tela de login da Twitch por cima de tudo
        let size = window.inner_size();
        let tb = (TOOLBAR_H * window.scale_factor()).round() as u32;
        let _ = dash.set_bounds(rect(OFFSCREEN, tb as i32, size.width, size.height));
        for g in games { let _ = g.set_bounds(rect(OFFSCREEN, tb as i32, size.width, size.height)); }
        let _ = l.set_visible(true);
        let _ = l.set_bounds(rect(0, tb as i32, size.width, size.height.saturating_sub(tb)));
        let _ = l.focus();
    }
    logf!("layout: fim ({} ms)", t.elapsed().as_millis());
}

fn apply_layout_inner(window: &tao::window::Window, toolbar: &WebView, dash: &WebView, games: &[WebView], lay: &Layout, cfg: &Config) {
    let size = window.inner_size();
    let scale = window.scale_factor();
    let tb = (TOOLBAR_H * scale).round() as u32;
    let (w, h) = (size.width, size.height.saturating_sub(tb));
    let _ = toolbar.set_bounds(rect(0, 0, w, tb));

    let pause = cfg.pausar_desenho;
    let low = cfg.memoria_baixa;
    let watch = format!("window.__hubWatch&&window.__hubWatch({})", lay.grid);
    if lay.grid {
        // Painel com as 4 contas; os jogos continuam rodando fora da tela.
        let _ = dash.set_memory_low(false);
        let _ = dash.set_visible(true);
        let _ = dash.set_bounds(rect(0, tb as i32, w, h));
        for g in games {
            let _ = g.set_bounds(rect(OFFSCREEN, tb as i32, w, h));
            if pause { let _ = g.set_visible(false); } if low { let _ = g.set_memory_low(true); }
            let _ = g.evaluate_script(&watch);
        }
        let _ = dash.focus();
    } else {
        let _ = dash.set_bounds(rect(OFFSCREEN, tb as i32, w, h));
        let _ = dash.set_visible(false);
        if low { let _ = dash.set_memory_low(true); }
        for (i, g) in games.iter().enumerate() {
            let _ = g.evaluate_script(&watch);
            if i == lay.active {
                let _ = g.set_memory_low(false);
                let _ = g.set_visible(true);
                let _ = g.set_bounds(rect(0, tb as i32, w, h));
                let _ = g.zoom(cfg.zoom_normal);
                let _ = g.focus();
            } else {
                // Fora da tela. Com pausar_desenho, também para de desenhar (o jogo segue rodando).
                let _ = g.set_bounds(rect(OFFSCREEN, tb as i32, w, h));
                if pause { let _ = g.set_visible(false); } if low { let _ = g.set_memory_low(true); }
            }
        }
    }
    let _ = toolbar.evaluate_script(&format!("window.__state&&window.__state({},{})", lay.active, lay.grid));
}

/// Cria UMA janela escondida da Kick (no perfil da conta, muda, fora da tela mas "visível" para não pausar).
#[allow(clippy::too_many_arguments)]
fn make_kick_window(i: usize, slot: usize, canal: &str, cfg: &Config, contexts: &mut [WebContext], args: &str, ext_on: bool,
                    window: &tao::window::Window, proxy: &tao::event_loop::EventLoopProxy<Msg>) -> Option<WebView> {
    let b = if cfg.motor_unico {
        WebViewBuilder::new_with_web_context(&mut contexts[0]).with_profile_name(format!("conta{}", i + 1))
    } else {
        WebViewBuilder::new_with_web_context(&mut contexts[i])
    };
    let p = proxy.clone();
    let r = b.with_url(format!("https://kick.com/{}", canal.to_lowercase()))
        .with_bounds(rect(OFFSCREEN, 40, 480, 270))
        .with_initialization_script_for_main_only(kick_script(&cfg.kick_canais, slot), true)
        .with_additional_browser_args(args.to_string())
        .with_browser_extensions_enabled(ext_on)
        .with_ipc_handler(move |req| {
            if let Some(html) = req.body().strip_prefix("kickdump:") {
                let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(base_dir);
                let f = dir.join(format!("diagnostico-kick-conta{}-{}.html", i + 1, slot + 1));
                let r = std::fs::write(&f, html);
                logf!("dump kick conta{}: {} bytes ({:?})", i + 1, html.len(), r.err());
                return;
            }
            if let Some(j) = req.body().strip_prefix("kick:") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) { let _ = p.send_event(Msg::KickStatus(i, v.to_string())); }
            }
        })
        .build_as_child(window);
    match r {
        Ok(w) => { logf!("kick conta{}: janela {} ({}) criada", i + 1, slot + 1, canal); Some(w) }
        Err(e) => { logf!("ERRO kick conta{}: {:?}", i + 1, e); None }
    }
}

#[allow(clippy::too_many_arguments)]
fn make_kick_watchers(i: usize, cfg: &Config, contexts: &mut [WebContext], args: &str, ext_on: bool,
                      window: &tao::window::Window, proxy: &tao::event_loop::EventLoopProxy<Msg>) -> Vec<WebView> {
    let first = cfg.kick_canais.first().cloned().unwrap_or_default();
    make_kick_window(i, 0, &first, cfg, contexts, args, ext_on, window, proxy).into_iter().collect()
}

/// Lê o login da Twitch salvo no perfil da conta (cookies da twitch.tv).
fn tw_creds(wv: &WebView) -> Option<(String, String)> {
    let cookies = wv.cookies_for_url("https://www.twitch.tv/").ok()?;
    let token = cookies.iter().find(|c| c.name() == "auth-token").map(|c| c.value().to_string())?;
    if token.len() < 10 { return None; }
    let login = cookies.iter().find(|c| c.name() == "login").map(|c| c.value().to_string())
        .filter(|l| !l.is_empty())
        .or_else(|| twitch::validate(&token).ok())?;
    Some((login, token))
}

fn display_name(cfg: &Config, nicks: &[Option<String>], i: usize) -> String {
    let manual = cfg.contas.get(i).cloned().unwrap_or_default();
    if !manual.trim().is_empty() && manual != default_name(i) {
        return manual;
    }
    nicks.get(i).cloned().flatten().unwrap_or_else(|| default_name(i))
}

fn push_name(toolbar: &WebView, dash: &WebView, i: usize, name: &str) {
    let js = format!("window.__setName&&window.__setName({},{})", i, serde_json::to_string(name).unwrap());
    let _ = toolbar.evaluate_script(&js);
    let _ = dash.evaluate_script(&js);
}

fn main() {
    log_init();
    std::panic::set_hook(Box::new(|info| { logf!("PANICO: {}", info); }));
    logf!("inicio v{}{}", VERSAO, if KICK { " (com Kick)" } else { " (sem Kick)" });
    let mut cfg = load_config();
    logf!("config ok: {} contas, url={}", cfg.contas.len(), cfg.url);
    let base = base_dir();

    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("PokeIdle Hub")
        .with_inner_size(LogicalSize::new(1400.0, 900.0))
        .with_min_inner_size(LogicalSize::new(640.0, 420.0))
        .build(&event_loop)
        .expect("falha ao criar janela");

    // Argumentos do motor: precisam ser IDÊNTICOS para tudo que divide o mesmo motor.
    let mut args = if cfg.flags_extras {
        BROWSER_ARGS.to_string()
    } else {
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_string()
    };
    if cfg.mutar { args.push_str(" --mute-audio"); }

    // motor_unico: 1 pasta/motor, 1 perfil por conta.  Senão: 1 motor inteiro por conta (modo antigo).
    let mut contexts: Vec<WebContext> = if cfg.motor_unico {
        vec![WebContext::new(Some(base.join("motor")))]
    } else {
        (0..cfg.contas.len()).map(|i| WebContext::new(Some(base.join(format!("conta{}", i + 1))))).collect()
    };
    let mut ui_ctx = WebContext::new(None);
    logf!("motor_unico={} args={}", cfg.motor_unico, args);
    let ext_dir = prepare_extensions(&cfg.extensoes);
    let ext_on = ext_dir.is_some(); // precisa ser igual em tudo que divide o motor

    // Barra de cima
    let p = proxy.clone();
    let toolbar = if cfg.motor_unico {
        WebViewBuilder::new_with_web_context(&mut contexts[0]).with_additional_browser_args(args.clone()).with_browser_extensions_enabled(ext_on)
    } else {
        WebViewBuilder::new_with_web_context(&mut ui_ctx)
    }
        .with_html(toolbar_html(&(0..cfg.contas.len()).map(|i| display_name(&cfg, &[], i)).collect::<Vec<_>>(), cfg.mostrar_insta))
        .with_bounds(rect(0, 0, 100, 38))
        .with_ipc_handler({
            let p = proxy.clone();
            move |req| {
                let body = req.body();
                logf!("ipc barra: {}", body.chars().take(80).collect::<String>());
                if let Some(m) = parse_msg(body) { let _ = p.send_event(m); }
            }
        })
        .with_navigation_handler(move |url: String| {
            logf!("barra navegacao: {}", url.chars().take(120).collect::<String>());
            if let Some(cmd) = url.strip_prefix(CMD_PREFIX) {
                if let Some(m) = parse_msg(&pct_decode(cmd)) { let _ = p.send_event(m); }
                return false; // cancela a navegação: a barra continua onde está
            }
            true // deixa carregar a própria barra
        })
        .build_as_child(&window)
        .expect("falha ao criar barra");

    let mut games: Vec<WebView> = Vec::new();
    for i in 0..cfg.contas.len() {
        logf!("criando conta{}", i + 1);
        let p = proxy.clone();
        let mut builder = if cfg.motor_unico {
            WebViewBuilder::new_with_web_context(&mut contexts[0]).with_profile_name(format!("conta{}", i + 1))
        } else {
            WebViewBuilder::new_with_web_context(&mut contexts[i])
        };
        builder = builder.with_browser_extensions_enabled(ext_on);
        if let Some(d) = &ext_dir { builder = builder.with_extensions_path(d.clone()); }
        let wv = builder
            .with_url(cfg.url.clone())
            .with_bounds(rect(OFFSCREEN, 40, 800, 600))
            .with_initialization_script_for_main_only(GAME_SCRIPT, true)
            .with_devtools(true)
            .with_additional_browser_args(args.clone())
            .with_ipc_handler(move |req| {
                let body = req.body();
                if !body.starts_with("stats:") { logf!("ipc conta{}: {}", i + 1, body.chars().take(80).collect::<String>()); }
                if let Some(html) = body.strip_prefix("dump:") {
                    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(base_dir);
                    let f = dir.join(format!("diagnostico-conta{}.html", i + 1));
                    let r = std::fs::write(&f, html);
                    logf!("dump conta{}: {} bytes -> {:?} ({:?})", i + 1, html.len(), f, r.err());
                    return;
                }
                if let Some(j) = body.strip_prefix("stats:") {
                    // valida e normaliza o JSON antes de repassar ao painel
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
                        let _ = p.send_event(Msg::Stats(i, v.to_string()));
                    }
                    return;
                }
                if let Some(nick) = body.strip_prefix("nick:") {
                    let nick: String = nick.trim().chars().take(24).collect();
                    if !nick.is_empty() { let _ = p.send_event(Msg::Nick(i, nick)); }
                } else if let Some(m) = parse_msg(body) {
                    let _ = p.send_event(m);
                }
            })
            .build_as_child(&window);
        let wv = match wv {
            Ok(w) => w,
            Err(e) => { logf!("ERRO criando conta{}: {:?}", i + 1, e); panic!("falha ao criar conta {}: {:?}", i + 1, e); }
        };
        logf!("conta{} criada", i + 1);
        games.push(wv);
    }

    // Painel da grade (cartões com as estatísticas de cada conta)
    let names0: Vec<String> = (0..cfg.contas.len()).map(|i| display_name(&cfg, &[], i)).collect();
    let dash = if cfg.motor_unico {
        WebViewBuilder::new_with_web_context(&mut contexts[0]).with_additional_browser_args(args.clone()).with_browser_extensions_enabled(ext_on)
    } else {
        WebViewBuilder::new_with_web_context(&mut ui_ctx)
    }
        .with_html(DASH_HTML.replace("__NAMES__", &serde_json::to_string(&names0).unwrap()).replace("__KICK__", if KICK { "true" } else { "false" }))
        .with_bounds(rect(OFFSCREEN, 40, 800, 600))
        .with_ipc_handler({
            let p = proxy.clone();
            move |req| {
                let body = req.body();
                logf!("ipc painel: {}", body.chars().take(80).collect::<String>());
                if let Some(m) = parse_msg(body) { let _ = p.send_event(m); }
            }
        })
        .build_as_child(&window)
        .unwrap_or_else(|e| { logf!("ERRO criando painel: {:?}", e); panic!("painel: {:?}", e) });

    // ---- Twitch
    let mut tw: Vec<Option<twitch::TwHandle>> = (0..games.len()).map(|_| None).collect();
    let mut twl: Option<(usize, WebView)> = None;
    let mut rotom: Option<(usize, WebView)> = None; // painel da Rotom Sniper (conta, página)
    // Versão nova no GitHub? (uma consulta só, ao abrir)
    if !cfg.repo_github.trim().is_empty() {
        let (repo, up) = (cfg.repo_github.trim().to_string(), proxy.clone());
        let _ = std::thread::Builder::new().name("update".into()).stack_size(256 * 1024).spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if let Some((tag, url)) = check_update(&repo) { let _ = up.send_event(Msg::UpdateAvail(tag, url)); }
        });
    }
    let mut update_url: Option<String> = None;
    let tw_proxy = proxy.clone();
    let start_tw = move |i: usize, login: String, token: String, cfg: &Config| -> twitch::TwHandle {
        let p = tw_proxy.clone();
        logf!("twitch conta{}: iniciando como {}", i + 1, login);
        twitch::spawn(login, token, cfg.canais.clone(), move |st| { let _ = p.send_event(Msg::TwStatus(i, st)); })
    };
    for (i, g) in games.iter().enumerate() {
        if let Some((login, token)) = tw_creds(g) { tw[i] = Some(start_tw(i, login, token, &cfg)); }
    }
    {
        let p = proxy.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if p.send_event(Msg::Tick).is_err() { break; }
        });
    }

    // ---- Discord Rich Presence
    let rpc = if cfg.discord && !cfg.discord_id.trim().is_empty() { Some(discord::spawn(cfg.discord_id.trim().to_string())) } else { None };
    let mut last_stats: Vec<Option<serde_json::Value>> = vec![None; games.len()];
    let mut last_presence: Option<discord::Presence> = None;
    let (mut rpc_ticks, mut rpc_rot) = (0usize, 0usize);

    // ---- Kick: janelas escondidas tocando a live (1 ou mais por conta)
    let mut kk: Vec<Vec<WebView>> = (0..games.len()).map(|_| Vec::new()).collect();
    let mut kkl: Option<(usize, WebView)> = None; // tela de login da Kick
    let mut kshow: Option<usize> = None; // live da Kick trazida para a tela (👁 Ver)
    for i in 0..games.len() {
        if KICK && kick_logged(&games[i]) {
            kk[i] = make_kick_watchers(i, &cfg, &mut contexts, &args, ext_on, &window, &proxy);
        }
    }

    let mut nicks: Vec<Option<String>> = vec![None; games.len()];
    let mut lay = Layout { active: 0, grid: false };
    apply_layout(&window, &toolbar, &dash, &games, &lay, &cfg, twl.as_ref().map(|x| &x.1).or(kkl.as_ref().map(|x| &x.1)).or(kshow.and_then(|k| kk.get(k).and_then(|v| v.first()))));

    let _ = toolbar.evaluate_script(&format!("window.__dmode&&window.__dmode('{}')", cfg.discord_nick));
    logf!("entrando no loop");
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep = (&ui_ctx, &tw, &kk); // contextos precisam viver junto com as contas
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            Event::WindowEvent { event: WindowEvent::Resized(_), .. }
            | Event::WindowEvent { event: WindowEvent::ScaleFactorChanged { .. }, .. } => {
                if let Some((_, r)) = &rotom { let _ = r.set_bounds(rotom_rect(&window)); }
                apply_layout(&window, &toolbar, &dash, &games, &lay, &cfg, twl.as_ref().map(|x| &x.1).or(kkl.as_ref().map(|x| &x.1)).or(kshow.and_then(|k| kk.get(k).and_then(|v| v.first()))));
            }
            Event::UserEvent(m) => {
                if !matches!(m, Msg::Stats(..) | Msg::Tick | Msg::TwStatus(..) | Msg::KickStatus(..)) { logf!("evento: {:?}", m); }
                match m {
                    Msg::Tab(i) if i < games.len() => {
                        if rotom.as_ref().map_or(false, |r| r.0 != i) { rotom = None; let _ = toolbar.evaluate_script("window.__rotom&&window.__rotom(false)"); }
                        twl = None; kkl = None; if let Some(k) = kshow.take() { if let Some(w) = kk.get(k).and_then(|v| v.first()) { let _ = w.set_bounds(rect(OFFSCREEN, 40, 480, 270)); } }
                        lay.active = i; lay.grid = false;
                        let _ = toolbar.evaluate_script(&format!("window.__drop&&window.__drop({},false)", i));
                        let _ = dash.evaluate_script(&format!("window.__clear&&window.__clear({})", i));
                    }
                    Msg::Tab(_) => {}
                    Msg::Grid => lay.grid = !lay.grid,
                    Msg::Reload(Some(i)) => { if let Some(g) = games.get(i) { let _ = g.reload(); } }
                    Msg::Reload(None) => { for g in &games { let _ = g.reload(); } }
                    Msg::DevTools => {
                        // 🐞 = salva um diagnóstico (cópia da página) ao lado do .exe; F12 dentro do jogo abre o DevTools
                        for ws in &kk { for w in ws.iter().take(1) { let _ = w.evaluate_script("window.__hubDump&&window.__hubDump()"); } }
                        if lay.grid {
                            for g in &games { let _ = g.evaluate_script("window.__hubDump&&window.__hubDump()"); }
                        } else if let Some(g) = games.get(lay.active) {
                            let _ = g.evaluate_script("window.__hubDump&&window.__hubDump()");
                        }
                        return;
                    }
                    Msg::Rotom => {
                        if rotom.take().is_some() {
                            let _ = toolbar.evaluate_script("window.__rotom&&window.__rotom(false)");
                            if let Some(g) = games.get(lay.active) { let _ = g.focus(); }
                            return;
                        }
                        let i = lay.active.min(games.len().saturating_sub(1));
                        // A Rotom não tem popup: o clique no ícone abre o "Cockpit" (cockpit.html) numa janela própria.
                        let cockpit = base_dir().join("extensoes").join(ROTOM_ID).join("cockpit.html").is_file();
                        let Some(page) = (if cockpit { Some("cockpit.html".to_string()) } else { ext_popup(ROTOM_ID) }) else {
                            let m = std::fs::read_to_string(base_dir().join("extensoes").join(ROTOM_ID).join("manifest.json")).unwrap_or_default();
                            let resumo: String = serde_json::from_str::<serde_json::Value>(m.trim_start_matches('\u{feff}')).ok()
                                .map(|v| { let mut v = v; if let Some(o) = v.as_object_mut() { o.remove("key"); o.remove("description"); } v.to_string() })
                                .unwrap_or_else(|| "(sem manifest)".into());
                            logf!("rotom: sem pagina de popup; manifest: {}", resumo.chars().take(3000).collect::<String>());
                            let _ = toolbar.evaluate_script("window.__rotomMsg&&window.__rotomMsg()");
                            return;
                        };
                        let id = wry::browser_extension_id(ROTOM_ID).unwrap_or_else(|| ROTOM_ID.to_string());
                        let url = format!("chrome-extension://{}/{}", id, page);
                        logf!("rotom conta{}: abrindo {}", i + 1, url);
                        let b = if cfg.motor_unico {
                            WebViewBuilder::new_with_web_context(&mut contexts[0]).with_profile_name(format!("conta{}", i + 1))
                        } else {
                            WebViewBuilder::new_with_web_context(&mut contexts[i])
                        };
                        let p = proxy.clone();
                        let r = b.with_url(url)
                            .with_bounds(rotom_rect(&window))
                            .with_additional_browser_args(args.clone())
                            .with_browser_extensions_enabled(ext_on)
                            .with_devtools(true)
                            .with_initialization_script("window.addEventListener('keydown',function(e){if(!window.ipc)return;if(e.key==='Escape'||(e.ctrlKey&&(e.key==='p'||e.key==='P'))){window.ipc.postMessage('rotom');e.preventDefault();}},true);")
                            .with_ipc_handler(move |req| { if req.body() == "rotom" { let _ = p.send_event(Msg::Rotom); } })
                            .build_as_child(&window);
                        match r {
                            Ok(w) => { let _ = w.focus(); rotom = Some((i, w)); let _ = toolbar.evaluate_script("window.__rotom&&window.__rotom(true)"); }
                            Err(e) => logf!("ERRO rotom: {:?}", e),
                        }
                        return;
                    }
                    Msg::TaskMgr => {
                        if let Some(g) = games.first() { if let Err(e) = g.open_task_manager() { logf!("ERRO task manager: {:?}", e); } }
                        return;
                    }
                    Msg::DiscordMode => {
                        cfg.discord_nick = match cfg.discord_nick.as_str() { "mostrar" => "borrar", "borrar" => "esconder", _ => "mostrar" }.into();
                        save_config(&cfg);
                        let _ = toolbar.evaluate_script(&format!("window.__dmode&&window.__dmode('{}')", cfg.discord_nick));
                        last_presence = None; // força reenviar
                        return;
                    }
                    Msg::OpenDevTools => {
                        if let Some(g) = games.get(lay.active) { g.open_devtools(); }
                        return;
                    }
                    Msg::Nick(i, n) => {
                        if i < nicks.len() && nicks[i].as_deref() != Some(n.as_str()) {
                            nicks[i] = Some(n);
                            push_name(&toolbar, &dash, i, &display_name(&cfg, &nicks, i));
                        }
                        return; // não precisa refazer o layout
                    }
                    Msg::TwLogin(i) if i < games.len() => {
                        logf!("twitch conta{}: abrindo login", i + 1);
                        tw[i] = None; // derruba conexão antiga, se houver
                        // sai da Twitch neste perfil, para dar para entrar com outra conta
                        if let Ok(cs) = games[i].cookies_for_url("https://www.twitch.tv/") {
                            for c in cs.iter().filter(|c| c.domain().map_or(true, |d| d.contains("twitch.tv"))) { let _ = games[i].delete_cookie(c); }
                        }
                        let _ = dash.evaluate_script(&format!("window.__tw&&window.__tw({},null)", i));
                        let b = if cfg.motor_unico {
                            WebViewBuilder::new_with_web_context(&mut contexts[0]).with_profile_name(format!("conta{}", i + 1))
                        } else {
                            WebViewBuilder::new_with_web_context(&mut contexts[i])
                        };
                        match b.with_url("https://www.twitch.tv/login").with_additional_browser_args(args.clone()).with_browser_extensions_enabled(ext_on).build_as_child(&window) {
                            Ok(w) => twl = Some((i, w)),
                            Err(e) => logf!("ERRO login twitch: {:?}", e),
                        }
                    }
                    Msg::TwLogin(_) => {}
                    Msg::TwCancel => { twl = None; kkl = None; if let Some(k) = kshow.take() { if let Some(w) = kk.get(k).and_then(|v| v.first()) { let _ = w.set_bounds(rect(OFFSCREEN, 40, 480, 270)); } } }
                    Msg::KickShow(i) => {
                        if kk.get(i).map_or(false, |v| !v.is_empty()) { twl = None; kkl = None; kshow = Some(i); }
                    }
                    Msg::KickLogin(i) if KICK && i < games.len() => {
                        logf!("kick conta{}: abrindo login", i + 1);
                        kk[i].clear();
                        let b = if cfg.motor_unico {
                            WebViewBuilder::new_with_web_context(&mut contexts[0]).with_profile_name(format!("conta{}", i + 1))
                        } else {
                            WebViewBuilder::new_with_web_context(&mut contexts[i])
                        };
                        match b.with_url("https://kick.com/").with_additional_browser_args(args.clone()).with_browser_extensions_enabled(ext_on).build_as_child(&window) {
                            Ok(w) => { twl = None; kkl = Some((i, w)); }
                            Err(e) => logf!("ERRO login kick: {:?}", e),
                        }
                    }
                    Msg::KickLogin(_) => {}
                    Msg::KickStatus(i, st) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&st) {
                            if let (Some(lives), Some(ws)) = (v.get("lives").and_then(|x| x.as_array()), kk.get_mut(i)) {
                                let lives: Vec<String> = lives.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
                                let max = if cfg.kick_max == 0 { usize::MAX } else { cfg.kick_max };
                                let want = lives.len().min(max).max(1); // o gerente (janela 1) fica sempre
                                // fecha as que sobraram
                                while ws.len() > want { ws.pop(); logf!("kick conta{}: fechou uma live (agora {})", i + 1, ws.len()); }
                                // manda cada janela para o seu canal (janela k -> live k)
                                for (k, w) in ws.iter().enumerate().skip(1) {
                                    if let Some(c) = lives.get(k) { let _ = w.evaluate_script(&format!("window.__hubKickGo&&window.__hubKickGo({})", serde_json::to_string(c).unwrap())); }
                                }
                                // abre as que faltam
                                for k in ws.len()..want {
                                    if let Some(c) = lives.get(k) {
                                        if let Some(w) = make_kick_window(i, k, c, &cfg, &mut contexts, &args, ext_on, &window, &proxy) { ws.push(w); }
                                    }
                                }
                                let _ = dash.evaluate_script(&format!("window.__kkN&&window.__kkN({},{})", i, ws.len()));
                            }
                        }
                        let _ = dash.evaluate_script(&format!("window.__kk&&window.__kk({},{})", i, st));
                        return;
                    }
                    Msg::Tick => {
                        if let Some(tx) = &rpc {
                            rpc_ticks += 1;
                            let passo = (cfg.discord_intervalo.max(10) / 2) as usize; // Tick = 2 s
                            if rpc_ticks % passo == 0 { rpc_rot += 1; }
                            let names: Vec<String> = (0..games.len()).map(|k| display_name(&cfg, &nicks, k)).collect();
                            let p = presence_from(&last_stats, &names, &cfg.discord_nick, rpc_rot, &cfg.discord_emoji);
                            let quase_igual = match (&p, &last_presence) {
                                (Some(a), Some(b)) => {
                                    let perto = match (a.start, b.start) { (Some(x), Some(y)) => x.abs_diff(y) < 90, (None, None) => true, _ => false };
                                    perto && a.details == b.details && a.state == b.state && a.large_image == b.large_image && a.large_text == b.large_text
                                }
                                _ => false,
                            };
                            if p != last_presence && !quase_igual { let _ = tx.send(p.clone()); last_presence = p; }
                        }
                        // login da Kick: quando aparecer a sessão, fecha a tela e abre a live escondida
                        let kick_ok = kkl.as_ref().and_then(|(i, w)| if kick_logged(w) { Some(*i) } else { None });
                        if let Some(i) = kick_ok {
                            kkl = None;
                            logf!("kick conta{}: logada", i + 1);
                            kk[i] = make_kick_watchers(i, &cfg, &mut contexts, &args, ext_on, &window, &proxy);
                            apply_layout(&window, &toolbar, &dash, &games, &lay, &cfg, None);
                            return;
                        }
                        // enquanto a tela de login está aberta, espera o login aparecer nos cookies
                        let found = twl.as_ref().and_then(|(i, w)| tw_creds(w).map(|c| (*i, c)));
                        match found {
                            Some((i, (login, token))) => {
                                twl = None; // fecha e libera a memória da tela de login
                                tw[i] = Some(start_tw(i, login, token, &cfg));
                            }
                            None => return,
                        }
                    }
                    Msg::TwStatus(i, st) => {
                        let _ = dash.evaluate_script(&format!("window.__tw&&window.__tw({},{})", i, st));
                        if st.contains("\"auth\"") || st.contains("\"caiu\"") { logf!("twitch conta{}: {}", i + 1, st); }
                        return;
                    }
                    Msg::Kick(i, on) => {
                        let _ = toolbar.evaluate_script(&format!("window.__kick&&window.__kick({},{})", i, on));
                        if on {
                            logf!("conta{}: aviso de conta aberta em outro lugar", i + 1);
                            window.request_user_attention(Some(tao::window::UserAttentionType::Critical));
                        }
                        return;
                    }
                    Msg::Drop(i) => {
                        let _ = toolbar.evaluate_script(&format!("window.__drop&&window.__drop({},true)", i));
                        window.request_user_attention(Some(tao::window::UserAttentionType::Informational));
                        return;
                    }
                    Msg::Stats(i, j) => {
                        if let (Some(slot), Ok(mut v)) = (last_stats.get_mut(i), serde_json::from_str::<serde_json::Value>(&j)) {
                            // o jogo às vezes redesenha no meio da leitura: mantém o último valor bom de cada campo
                            if let (Some(old), Some(obj)) = (slot.as_ref(), v.as_object_mut()) {
                                for k in ["stats", "pl", "pk", "hunt", "timer", "bonus", "rare", "best", "items"] {
                                    let vazio = match obj.get(k) { None | Some(serde_json::Value::Null) => true, Some(serde_json::Value::String(x)) => x.is_empty(), Some(serde_json::Value::Array(a)) => a.is_empty(), _ => false };
                                    if vazio { if let Some(o) = old.get(k) { obj.insert(k.into(), o.clone()); } }
                                }
                            }
                            *slot = Some(v);
                        }
                        let _ = dash.evaluate_script(&format!("window.__upd&&window.__upd({},{})", i, j));
                        return;
                    }
                    Msg::Rename(i, n) => {
                        if i < cfg.contas.len() {
                            cfg.contas[i] = if n.is_empty() { default_name(i) } else { n };
                            save_config(&cfg);
                            push_name(&toolbar, &dash, i, &display_name(&cfg, &nicks, i));
                        }
                        return;
                    }
                    Msg::UpdateAvail(tag, url) => {
                        let _ = toolbar.evaluate_script(&format!("window.__update&&window.__update({})", serde_json::to_string(&tag).unwrap()));
                        update_url = Some(url);
                        return;
                    }
                    Msg::OpenUpdate => {
                        if let Some(u) = &update_url { let _ = std::process::Command::new("explorer").arg(u).spawn(); }
                        return;
                    }
                    Msg::Insta => {
                        // abre no navegador padrão da pessoa (não dentro do app)
                        let _ = std::process::Command::new("explorer").arg(format!("https://www.instagram.com/{INSTA}/")).spawn();
                        return;
                    }
                    Msg::OpenConfig => { let _ = std::process::Command::new("explorer").arg(base_dir()).spawn(); }
                }
                apply_layout(&window, &toolbar, &dash, &games, &lay, &cfg, twl.as_ref().map(|x| &x.1).or(kkl.as_ref().map(|x| &x.1)).or(kshow.and_then(|k| kk.get(k).and_then(|v| v.first()))));
            }
            _ => {}
        }
    });
}
