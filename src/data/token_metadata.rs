//! Resolución de metadata básica de un token (symbol, decimals) vía
//! llamadas ERC-20 estándar, más metadata específica del proyecto (de qué
//! launchpad viene, en qué fase está).

#[derive(Debug, Clone)]
pub struct TokenMetadata {
    pub address: String,
    pub symbol: String,
    pub decimals: u8,
    pub launchpad: Option<String>,
}

pub async fn resolve_erc20_metadata(_address: &str) -> anyhow::Result<TokenMetadata> {
    todo!("llamadas symbol()/decimals() estándar vía alloy")
}

/// Validación de que una dirección pegada por el usuario es realmente un
/// ERC-20 con actividad de trading real (no un contrato arbitrario o un
/// honeypot disfrazado). Pendiente de diseño cuidadoso — ver
/// security::honeypot para el chequeo de riesgo real; esto es solo la
/// comprobación de "es un ERC-20 válido y tiene curva o pool asociado".
pub async fn validate_pasted_token_address(_address: &str) -> anyhow::Result<bool> {
    todo!("comprobar interfaz ERC-20 mínima + existencia de curva/pool conocidos")
}
