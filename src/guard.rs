//! Protections anti-spam : token bucket par IP + plafond de concurrence.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Token bucket : `capacity` rafale, `per_min` remplissage par minute.
pub struct RateLimiter {
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
    capacity: f64,
    per_min: f64,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(capacity: u32, per_min: f64) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: capacity as f64,
            per_min,
        }
    }

    /// Ok, ou Err(secondes à attendre avant le prochain jeton).
    pub fn check(&self, ip: IpAddr) -> Result<(), u64> {
        self.check_at(ip, Instant::now())
    }

    pub fn check_at(&self, ip: IpAddr, now: Instant) -> Result<(), u64> {
        let mut m = self.buckets.lock().unwrap();
        let rate_per_sec = self.per_min / 60.0;
        let b = m.entry(ip).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.duration_since(b.last).as_secs_f64();
        b.last = now;
        b.tokens = (b.tokens + elapsed * rate_per_sec).min(self.capacity);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            let wait = ((1.0 - b.tokens) / rate_per_sec).ceil().max(1.0) as u64;
            Err(wait)
        }
    }
}

/// Plafond d'appels simultanés vers le backend (opencode est lourd :
/// ~1 proc par requête, on ne veut pas 20 spawns parallèles).
pub struct Gate {
    max: u32,
    cur: Mutex<u32>,
    cv: Condvar,
}

pub struct GateGuard<'a> {
    gate: &'a Gate,
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut cur = self.gate.cur.lock().unwrap();
        *cur = cur.saturating_sub(1);
        self.gate.cv.notify_one();
    }
}

impl Gate {
    pub fn new(max: u32) -> Self {
        Self {
            max,
            cur: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    /// Acquiert un slot ou None après `wait`.
    pub fn acquire(&self, wait: Duration) -> Option<GateGuard<'_>> {
        let mut cur = self.cur.lock().unwrap();
        let deadline = Instant::now() + wait;
        while *cur >= self.max {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (c, _t) = self.cv.wait_timeout(cur, deadline - now).unwrap();
            cur = c;
        }
        *cur += 1;
        Some(GateGuard { gate: self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, n))
    }

    #[test]
    fn bucket_rafale_puis_refus() {
        let rl = RateLimiter::new(3, 60.0);
        let t0 = Instant::now();
        assert!(rl.check_at(ip(1), t0).is_ok());
        assert!(rl.check_at(ip(1), t0).is_ok());
        assert!(rl.check_at(ip(1), t0).is_ok());
        let wait = rl.check_at(ip(1), t0).unwrap_err();
        assert!(wait >= 1);
        // autre IP : non affectée
        assert!(rl.check_at(ip(2), t0).is_ok());
    }

    #[test]
    fn bucket_recharge_avec_le_temps() {
        let rl = RateLimiter::new(1, 60.0); // 1 jeton/min
        let t0 = Instant::now();
        assert!(rl.check_at(ip(1), t0).is_ok());
        assert!(rl.check_at(ip(1), t0).is_err());
        // +60s => 1 jeton reconstitué
        let t1 = t0 + Duration::from_secs(60);
        assert!(rl.check_at(ip(1), t1).is_ok());
    }

    #[test]
    fn gate_bloque_puis_libere() {
        let g = Gate::new(1);
        let a = g.acquire(Duration::from_millis(10)).unwrap();
        assert!(g.acquire(Duration::from_millis(30)).is_none()); // occupé
        drop(a); // libère
        assert!(g.acquire(Duration::from_millis(10)).is_some());
    }
}
