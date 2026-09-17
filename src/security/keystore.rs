//! Keystore cifrado en disco, formato JSON estándar tipo Ethereum
//! (compatible con lo que generan geth/ethers/otros clientes), usando el
//! crate `eth-keystore`. La contraseña se pide de forma interactiva al
//! arrancar la app — nunca como argumento de CLI (quedaría en el historial
//! de shell) y nunca logueada.
//!
//! La clave descifrada se envuelve en un tipo que hace zeroize al hacer
//! drop, para minimizar el tiempo que queda en memoria sin limpiar tras su
//! último uso.

use zeroize::Zeroize;

/// Wrapper que garantiza que los bytes de la clave privada se ponen a cero
/// en memoria cuando este valor se destruye (fin de sesión, panic, etc.).
/// No es una garantía absoluta (el compilador puede optimizar copias), pero
/// es la mitigación estándar razonable sin entrar en hardware wallets.
pub struct SensitiveKey(Vec<u8>);

impl Drop for SensitiveKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SensitiveKey {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Genera una nueva wallet y la guarda como keystore cifrado. Uso: primera
/// vez que se arranca la app y el usuario elige "crear wallet nueva" en vez
/// de importar una existente (decisión pendiente de UX, ver CLAUDE.md).
pub fn generate_new_keystore(_dir: &str, _password: &str) -> anyhow::Result<String> {
    todo!("usar eth_keystore::encrypt_key generando una clave nueva con OsRng")
}

/// Importa una clave privada existente (el usuario la pega una vez) y la
/// guarda cifrada. La clave en texto plano pasada aquí debe descartarse
/// (zeroize) inmediatamente después de esta llamada por parte del caller.
pub fn import_and_encrypt(_dir: &str, _private_key_hex: &str, _password: &str) -> anyhow::Result<String> {
    todo!("usar eth_keystore::encrypt_key con la clave provista por el usuario")
}

/// Desbloquea un keystore existente pidiendo la contraseña de forma
/// interactiva (nunca desde un argumento de CLI). Devuelve la clave
/// envuelta en `SensitiveKey`.
pub fn unlock_interactive(_keystore_path: &str) -> anyhow::Result<SensitiveKey> {
    todo!(
        "leer el JSON del keystore, pedir contraseña con un crate tipo \
         `rpassword` (no eco en terminal), llamar a eth_keystore::decrypt_key"
    )
}
