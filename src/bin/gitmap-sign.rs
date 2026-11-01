//! Подпись выпусков (ed25519). Закрытый ключ не хранится в файлах: скрипты
//! scripts/release берут его из диспетчера учётных данных и передают через
//! переменную окружения GITMAP_SIGNING_KEY.
//!
//!   gitmap-sign keygen          новый ключ: PRIVATE=… и PUBLIC=…
//!   gitmap-sign pubkey          открытый ключ для закрытого из переменной
//!   gitmap-sign sign <файл>     подпись файла, base64
//!   gitmap-sign sha256 <файл>   хеш файла

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

fn fail(message: &str) -> ! {
    eprintln!("ошибка: {message}");
    std::process::exit(1);
}

fn key_from_env() -> SigningKey {
    let raw = std::env::var("GITMAP_SIGNING_KEY").unwrap_or_else(|_| fail("нет переменной GITMAP_SIGNING_KEY"));
    let bytes: [u8; 32] = B64
        .decode(raw.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| fail("ключ в GITMAP_SIGNING_KEY повреждён"));
    SigningKey::from_bytes(&bytes)
}

fn read(path: Option<String>) -> Vec<u8> {
    let path = path.unwrap_or_else(|| fail("укажите файл"));
    std::fs::read(&path).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("keygen") => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).unwrap_or_else(|_| fail("нет источника случайных чисел"));
            let key = SigningKey::from_bytes(&seed);
            println!("PRIVATE={}", B64.encode(seed));
            println!("PUBLIC={}", B64.encode(key.verifying_key().to_bytes()));
        }
        Some("pubkey") => println!("{}", B64.encode(key_from_env().verifying_key().to_bytes())),
        Some("sign") => {
            let data = read(args.next());
            println!("{}", B64.encode(key_from_env().sign(&data).to_bytes()));
        }
        Some("sha256") => {
            let data = read(args.next());
            println!("{}", Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
        _ => fail("команды: keygen | pubkey | sign <файл> | sha256 <файл>"),
    }
}
