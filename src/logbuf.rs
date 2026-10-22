//! Buffer mémoire des lignes de log du service — exposé via
//! `GET /admin/logs?x-tail=N` pour le diagnostic sans accès disque
//! (vu en prod : l'agent backend cherchait le fichier de log hors de sa
//! sandbox ; avec cet endpoint il se diagnostique tout seul).

use std::collections::VecDeque;

const CAP: usize = 500;

/// Ring buffer borné de lignes de log (thread-safe côté serveur :
/// verrouillé par l'appelant, comme Registry/Stats).
pub struct LogBuffer {
    inner: VecDeque<String>,
    cap: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self {
            inner: VecDeque::with_capacity(CAP),
            cap: CAP,
        }
    }
}

impl LogBuffer {
    /// Pousse une ligne (tronquée à 500 chars — les sorties backend
    /// peuvent être énormes, on ne garde que l'utile au diagnostic).
    pub fn push(&mut self, line: &str) {
        let line = line.trim_end();
        if line.is_empty() {
            return;
        }
        let truncated: String = line.chars().take(500).collect();
        if self.inner.len() >= self.cap {
            self.inner.pop_front();
        }
        self.inner.push_back(truncated);
    }

    /// Les `n` dernières lignes, de la plus ancienne à la plus récente.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let skip = self.inner.len().saturating_sub(n);
        self.inner.iter().skip(skip).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_tail_et_cap() {
        let mut b = LogBuffer::default();
        assert!(b.is_empty());
        for i in 0..(CAP + 10) {
            b.push(&format!("ligne {i}"));
        }
        assert_eq!(b.len(), CAP);
        let tail = b.tail(5);
        assert_eq!(tail.len(), 5);
        assert_eq!(tail[0], format!("ligne {}", CAP + 5));
        assert_eq!(tail[4], format!("ligne {}", CAP + 9));
        // lignes vides ignorées, troncature des lignes énormes
        b.push("");
        b.push(&"x".repeat(2000));
        assert_eq!(b.len(), CAP);
        assert_eq!(b.tail(1)[0].chars().count(), 500);
    }
}
