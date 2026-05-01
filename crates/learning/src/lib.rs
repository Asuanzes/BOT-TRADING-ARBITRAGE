use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

/// Contexto discretizado capturado en el momento de la decisión de entrada.
/// Sirve como clave del HashMap de aprendizaje — cada combinación acumula
/// su propia estadística de wins/total independientemente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TradeFeatures {
    /// Zona del precio YES en el CLOB al tomar la decisión.
    /// 0 = < 0.35 · 1 = 0.35–0.45 · 2 = 0.45–0.55 · 3 = 0.55–0.65 · 4 = > 0.65
    pub price_bucket: u8,
    /// Dirección e intensidad del momentum de BTC (USD/s).
    /// 0 = < −10 · 1 = −10..−2 · 2 = −2..+2 · 3 = +2..+10 · 4 = > +10
    pub momentum_bucket: u8,
    /// Sesión UTC (hora / 6): 0=Asia · 1=Europa · 2=NY-apertura · 3=NY-tarde
    pub hour_bucket: u8,
    /// Tiempo restante en la ventana: 0 = >25s · 1 = 15–25s · 2 = <15s
    pub time_bucket: u8,
    /// Dirección de la entrada: 0 = Up (YES) · 1 = Down (NO)
    pub direction: u8,
}

impl TradeFeatures {
    /// Construye el contexto a partir de datos en tiempo real.
    /// `yes_price` — precio del token YES en el CLOB.
    /// `momentum` — momentum_usd_per_sec del snapshot.
    /// `utc_hour` — hora UTC actual (0–23).
    /// `remaining_secs` — segundos restantes en la ventana.
    /// `direction_up` — true si la entrada es en dirección UP/YES.
    pub fn extract(
        yes_price:      f64,
        momentum:       f64,
        utc_hour:       u8,
        remaining_secs: f64,
        direction_up:   bool,
    ) -> Self {
        let price_bucket = if yes_price < 0.35 { 0 }
            else if yes_price < 0.45 { 1 }
            else if yes_price < 0.55 { 2 }
            else if yes_price < 0.65 { 3 }
            else { 4 };

        let momentum_bucket = if momentum < -10.0 { 0 }
            else if momentum < -2.0 { 1 }
            else if momentum < 2.0  { 2 }
            else if momentum < 10.0 { 3 }
            else { 4 };

        let hour_bucket = utc_hour / 6;

        let time_bucket = if remaining_secs > 25.0 { 0 }
            else if remaining_secs >= 15.0 { 1 }
            else { 2 };

        Self {
            price_bucket,
            momentum_bucket,
            hour_bucket,
            time_bucket,
            direction: if direction_up { 0 } else { 1 },
        }
    }
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct LearningEntry {
    wins:  u32,
    total: u32,
}

/// Motor de aprendizaje Bayesiano con log-odds y Laplace smoothing.
///
/// Aprende qué combinaciones de contexto (precio · momentum · hora · tiempo · dirección)
/// son históricamente rentables y retorna un bias de confianza en [−0.4, +0.4].
/// El bias es 0.0 cuando hay menos de 5 observaciones en ese bucket.
pub struct LearningEngine {
    data: HashMap<TradeFeatures, LearningEntry>,
    path: String,
}

impl LearningEngine {
    /// Carga desde JSON o devuelve un motor vacío si el archivo no existe.
    pub fn load_or_default(path: &str) -> Self {
        let data = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { data, path: path.to_string() }
    }

    /// Serializa el estado a `logs/learning.json`.
    pub fn save(&self) {
        match serde_json::to_string_pretty(&self.data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    warn!("learning: no se puede guardar {}: {e}", self.path);
                }
            }
            Err(e) => warn!("learning: serialización fallida: {e}"),
        }
    }

    /// Registra el outcome de un trade.  Llamar tras cada cierre.
    pub fn record_outcome(&mut self, f: &TradeFeatures, won: bool) {
        let entry = self.data.entry(*f).or_default();
        entry.total += 1;
        if won { entry.wins += 1; }
        let (wins, total) = (entry.wins, entry.total);
        let bias = self.confidence_bias(f);
        info!(
            "learning: bucket=({},{},{},{},{}) wins={}/{} bias={:.3}",
            f.price_bucket, f.momentum_bucket, f.hour_bucket, f.time_bucket, f.direction,
            wins, total, bias,
        );
    }

    /// Bias de confianza Bayesiano para el contexto dado.
    ///
    /// Retorna 0.0 si hay < 5 observaciones en ese bucket (prior neutral).
    /// En caso contrario retorna un valor en [−0.4, +0.4]:
    ///   positivo → el contexto ha sido históricamente rentable → aumentar confianza
    ///   negativo → el contexto ha sido históricamente perdedor → reducir confianza
    pub fn confidence_bias(&self, f: &TradeFeatures) -> f64 {
        let entry = match self.data.get(f) {
            Some(e) if e.total >= 5 => e,
            _ => return 0.0,
        };

        // Laplace smoothing: evita log(0) y suaviza distribuciones pequeñas
        let win_rate = (entry.wins as f64 + 1.0) / (entry.total as f64 + 2.0);

        // sqrt(n) confidence scaling: crece lentamente, tope en 3.0 (≥36 trades)
        let scale = ((entry.total as f64).sqrt() * 0.5).min(3.0);

        // Log-odds ponderado
        let log_odds = (win_rate / (1.0 - win_rate)).ln() * scale;

        // Transformación logística de vuelta a probabilidad
        let prob = 1.0 / (1.0 + (-log_odds).exp());

        // Centrar en 0 y escalar al rango [−0.4, +0.4]
        (prob - 0.5) * 0.8
    }

    /// Total de trades registrados (para logging/debug).
    pub fn total_trades(&self) -> u32 {
        self.data.values().map(|e| e.total).sum()
    }

    /// Número de buckets con al menos una observación.
    pub fn active_buckets(&self) -> usize {
        self.data.values().filter(|e| e.total > 0).count()
    }
}
