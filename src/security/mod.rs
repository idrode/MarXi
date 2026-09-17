//! Gestión de la clave privada. Principio no negociable: la clave
//! descifrada vive SOLO en memoria durante la sesión activa, nunca se
//! escribe en claro a disco ni se loguea (ni siquiera en logs de debug).

pub mod keystore;
pub mod honeypot;
