//! Обновления из репозитория выпусков. Всё скачанное проверяется подписью ed25519;
//! открытый ключ встраивается при сборке из assets/updater.pub
//! (scripts/release/new-updater-key.ps1).

use crate::install;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST_URL: &str = "https://github.com/ServChan/gitmap-releases/releases/latest/download/latest.json";
const PUBKEY: Option<&str> = option_env!("GITMAP_UPDATER_PUBKEY");
const TIMEOUT: Duration = Duration::from_secs(20);
/// Как часто проверять обновления сами по себе.
pub const CHECK_EVERY_SECS: u64 = 24 * 3600;

/// Ключ проверки подписи встроен: обновления можно включать.
pub fn configured() -> bool {
    PUBKEY.is_some_and(|key| !key.trim().is_empty())
}

/// Описание выпуска (latest.json в выпуске на GitHub).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    pub sha256: String,
    /// Подпись ed25519 содержимого exe, base64.
    pub signature: String,
    /// Подпись строки «версия и хеш» (`release_text`): без неё старый подписанный
    /// установщик можно было бы выдать за новую версию.
    pub release_signature: String,
}

/// Что подписывается вместе с версией: так версию нельзя подменить отдельно от файла.
pub fn release_text(version: &str, sha256: &str) -> String {
    format!("gitmap-release\n{}\n{}\n", version.trim(), sha256.trim().to_lowercase())
}

/// Версия вида 1.2.3: только цифры и точки, она попадает в сравнения.
fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 20
        && version.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// a новее b: сравнение версий вида 1.2.3.
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parse(a) > parse(b)
}

fn verify_with(key_b64: &str, data: &[u8], signature_b64: &str) -> Result<(), String> {
    let damaged = "встроенный ключ проверки подписи повреждён";
    let key: [u8; 32] = B64.decode(key_b64.trim()).ok().and_then(|k| k.try_into().ok()).ok_or(damaged)?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| damaged.to_string())?;
    let signature: [u8; 64] =
        B64.decode(signature_b64.trim()).ok().and_then(|s| s.try_into().ok()).ok_or("подпись повреждена")?;
    key.verify(data, &Signature::from_bytes(&signature))
        .map_err(|_| "подпись не совпала: файл мог быть подменён, обновление отменено".to_string())
}

fn verify(data: &[u8], signature_b64: &str) -> Result<(), String> {
    verify_with(PUBKEY.ok_or("обновления ещё не настроены: нет ключа проверки подписи")?, data, signature_b64)
}

fn manifest_url() -> String {
    // Проверка обновления на этой машине: подпись всё равно проверяется настоящим ключом.
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("GITMAP_UPDATE_URL") {
        return url;
    }
    MANIFEST_URL.to_owned()
}

/// Новая версия программы, если она есть.
pub fn check() -> Result<Option<Manifest>, String> {
    let body = http_get(&manifest_url())?;
    let manifest: Manifest = parse_manifest(&body)?;
    Ok(newer(&manifest.version, install::VERSION).then_some(manifest))
}

fn parse_manifest(body: &[u8]) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_slice(body).map_err(|_| "описание выпуска не разобралось".to_string())?;
    if !valid_version(&manifest.version) {
        return Err("в описании выпуска неверная версия".into());
    }
    Ok(manifest)
}

/// Скачивает установщик новой версии и проверяет хеш и подпись.
/// Описание выпуска берётся свежее, а не сохранённое при прошлой проверке.
pub fn download() -> Result<PathBuf, String> {
    let manifest = check()?.ok_or("новой версии нет")?;
    verify(release_text(&manifest.version, &manifest.sha256).as_bytes(), &manifest.release_signature)
        .map_err(|_| "подпись версии не совпала: описание выпуска могли подменить, обновление отменено".to_string())?;
    let data = http_get(&manifest.url)?;
    let hash: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    if !hash.eq_ignore_ascii_case(manifest.sha256.trim()) {
        return Err("файл скачался не полностью или изменён: хеш не совпал".into());
    }
    verify(&data, &manifest.signature)?;
    let dir = install::installed_dir().map(|d| d.join(install::UPDATES_DIR)).ok_or("программа не установлена")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("не удалось создать {}: {e}", dir.display()))?;
    let path = dir.join(format!("GitMap-Setup-{}.exe", crate::sys::random_suffix()));
    std::fs::write(&path, &data).map_err(|e| format!("не удалось сохранить установщик: {e}"))?;
    Ok(path)
}

/// Запускает скачанный установщик в режиме тихого обновления.
pub fn run(installer: &Path) -> Result<(), String> {
    std::process::Command::new(installer)
        .arg("--update")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("не удалось запустить обновление: {e}"))
}

fn tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let mut config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("протоколы TLS заданы")
                .with_root_certificates(roots)
                .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(config)
        })
        .clone()
}

/// GET с переходом по перенаправлениям GitHub и повтором при обрыве связи.
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let mut url = url.to_string();
    for _ in 0..6 {
        let mut attempt = get_once(&url);
        for pause in [1, 3] {
            if attempt.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_secs(pause));
            attempt = get_once(&url);
        }
        let (status, headers, body) = attempt?;
        match status {
            200 => return Ok(body),
            301 | 302 | 303 | 307 | 308 => {
                url = header(&headers, "location").ok_or("перенаправление без адреса")?.to_string();
            }
            404 => return Err("выпусков пока нет".into()),
            code => return Err(format!("сервер обновлений ответил {code}")),
        }
    }
    Err("слишком много перенаправлений".into())
}

fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn split_url(rest: &str) -> (&str, &str) {
    match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    }
}

fn get_once(url: &str) -> Result<(u16, String, Vec<u8>), String> {
    #[cfg(debug_assertions)]
    if let Some(rest) = url.strip_prefix("http://127.0.0.1") {
        let (authority, path) = split_url(rest);
        let host = format!("127.0.0.1{authority}");
        let mut tcp = TcpStream::connect(&host).map_err(|_| format!("{host} не отвечает"))?;
        return exchange(&mut tcp, &host, path);
    }
    let rest = url.strip_prefix("https://").ok_or("поддерживается только https")?;
    let (host, path) = split_url(rest);
    let addr = (host, 443)
        .to_socket_addrs()
        .map_err(|_| format!("не удалось найти {host}: нет интернета?"))?
        .next()
        .ok_or_else(|| format!("не удалось найти {host}"))?;
    let tcp = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|_| format!("{host} не отвечает"))?;
    tcp.set_read_timeout(Some(TIMEOUT)).ok();
    tcp.set_write_timeout(Some(TIMEOUT)).ok();
    let name = ServerName::try_from(host.to_string()).map_err(|_| "неверный адрес".to_string())?;
    let connection = ClientConnection::new(tls_config(), name).map_err(|e| e.to_string())?;
    exchange(&mut StreamOwned::new(connection, tcp), host, path)
}

fn exchange(stream: &mut (impl Read + Write), host: &str, path: &str) -> Result<(u16, String, Vec<u8>), String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: GitMap/{}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        install::VERSION
    );
    stream.write_all(request.as_bytes()).map_err(|e| format!("связь прервалась: {e}"))?;
    let mut raw = Vec::new();
    match stream.read_to_end(&mut raw) {
        Ok(_) => {}
        // Сервер закрыл соединение без прощания TLS: данные уже получены.
        Err(e) if e.kind() == ErrorKind::UnexpectedEof && !raw.is_empty() => {}
        Err(e) => return Err(format!("загрузка прервалась: {e}")),
    }
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("неполный ответ сервера")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = &raw[split + 4..];
    let chunked = header(&head, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    Ok((status, head, if chunked { dechunk(body) } else { body.to_vec() }))
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(end) = body.windows(2).position(|w| w == b"\r\n") {
        let size_text = String::from_utf8_lossy(&body[..end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        if size == 0 || body.len() < end + 2 + size {
            break;
        }
        out.extend_from_slice(&body[end + 2..end + 2 + size]);
        body = &body[(end + 4 + size).min(body.len())..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn versions_are_digits_and_dots() {
        assert!(valid_version("1.0.2"));
        assert!(!valid_version(r"1\..\..\x"));
        assert!(!valid_version("1..2"));
        assert!(!valid_version(""));
    }

    #[test]
    fn compares_versions() {
        assert!(newer("1.0.10", "1.0.9"));
        assert!(newer("v1.1.0", "1.0.2"));
        assert!(!newer("1.0.2", "1.0.2"));
        assert!(!newer("1.0.1", "1.0.2"));
    }

    #[test]
    fn signature_binds_file_and_version() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = B64.encode(key.verifying_key().to_bytes());
        let text = release_text("1.0.1", "ABC");
        let signature = B64.encode(key.sign(text.as_bytes()).to_bytes());
        assert!(verify_with(&public, text.as_bytes(), &signature).is_ok());
        let forged = release_text("9.9.9", "ABC");
        assert!(verify_with(&public, forged.as_bytes(), &signature).is_err());
        assert!(verify_with(&public, text.as_bytes(), "не подпись").is_err());
    }

    #[test]
    fn chunked_body_is_joined() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), b"Wikipedia");
    }

    #[test]
    fn manifest_with_a_bad_version_is_rejected() {
        let good = br#"{"version":"1.0.1","url":"u","sha256":"s","signature":"a","release_signature":"b"}"#;
        assert!(parse_manifest(good).is_ok());
        let bad = br#"{"version":"1.0.1-x","url":"u","sha256":"s","signature":"a","release_signature":"b"}"#;
        assert!(parse_manifest(bad).is_err());
    }
}
