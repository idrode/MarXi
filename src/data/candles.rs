//! Agregación de swaps/curve-trades individuales en velas OHLCV.
//! Algoritmo estándar de agregación por ventana de tiempo — sin
//! complejidad conceptual nueva, la parte no trivial es alimentarlo con la
//! fuente correcta según la fase del token (ver `data::watcher`).

#[derive(Debug, Clone, Copy)]
pub struct Candle {
    pub open_time: u64, // unix timestamp del inicio de la ventana
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

pub struct CandleAggregator {
    pub window_seconds: u64,
    current: Option<Candle>,
    pub completed: Vec<Candle>,
}

impl CandleAggregator {
    pub fn new(window_seconds: u64) -> Self {
        Self { window_seconds, current: None, completed: Vec::new() }
    }

    /// Alimenta un trade individual (precio + volumen + timestamp). Cierra
    /// la vela actual y abre una nueva si el trade cae fuera de la ventana
    /// en curso.
    /// Todas las velas, incluida la que aún está abierta. La vela en curso se
    /// devuelve tal cual está: para un backfill histórico es la última vela
    /// real del token, no un parcial que haya que descartar.
    pub fn snapshot(&self) -> Vec<Candle> {
        let mut out = self.completed.clone();
        if let Some(c) = self.current {
            out.push(c);
        }
        out
    }

    pub fn ingest(&mut self, timestamp: u64, price: f64, volume: f64) {
        let window_start = timestamp - (timestamp % self.window_seconds);

        match &mut self.current {
            Some(c) if c.open_time == window_start => {
                c.high = c.high.max(price);
                c.low = c.low.min(price);
                c.close = price;
                c.volume += volume;
            }
            Some(c) => {
                self.completed.push(*c);
                self.current = Some(Candle {
                    open_time: window_start,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume,
                });
            }
            None => {
                self.current = Some(Candle {
                    open_time: window_start,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume,
                });
            }
        }
    }
}
