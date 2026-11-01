use embed_manifest::manifest::DpiAwareness;
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("GitMap").dpi_awareness(DpiAwareness::PerMonitorV2))
            .expect("не удалось встроить манифест");
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set("FileDescription", "Карта веток")
            .set("ProductName", "Карта веток")
            .compile()
            .expect("не удалось встроить значок");
    }
    // Открытый ключ проверки обновлений (scripts/release/new-updater-key.ps1).
    // Без него программа собирается, но обновления выключены.
    println!("cargo:rerun-if-changed=assets/updater.pub");
    if let Ok(key) = std::fs::read_to_string("assets/updater.pub") {
        println!("cargo:rustc-env=GITMAP_UPDATER_PUBKEY={}", key.trim());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
