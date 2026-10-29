use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub struct Bridge {
    enabled: bool,
    path: PathBuf,
    seq: AtomicU64,
}

impl Bridge {
    pub fn from_env() -> Self {
        let enabled = std::env::var("ASCLEPIUS_BRIDGE").ok().as_deref() == Some("1");
        let path = std::env::var("ASCLEPIUS_BRIDGE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".local/share/anticonf/data/bridge.jsonl")
            });
        Self {
            enabled,
            path,
            seq: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn post(&self, kind: &str, body: &str) {
        if !self.enabled {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let id = format!("asc_{}_{}", now as u64, seq);
        let line = serde_json::json!({
            "id": id,
            "ts": now,
            "source": "asclepius",
            "kind": kind,
            "body": body,
        });
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{line}") {
                    eprintln!("asclepius bridge post failed: {e}");
                }
            }
            Err(e) => eprintln!("asclepius bridge open failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_bridge_writes_nothing() {
        let bridge = Bridge {
            enabled: false,
            path: std::env::temp_dir().join("asclepius-bridge-test-disabled.jsonl"),
            seq: AtomicU64::new(0),
        };
        bridge.post("status", "hello");
        assert!(!bridge.path.exists());
    }

    #[test]
    fn enabled_bridge_appends_jsonl() {
        let path = std::env::temp_dir().join("asclepius-bridge-test-enabled.jsonl");
        let _ = std::fs::remove_file(&path);
        let bridge = Bridge {
            enabled: true,
            path: path.clone(),
            seq: AtomicU64::new(0),
        };
        bridge.post("status", "panel committed");
        bridge.post("status", "second event");
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["source"], "asclepius");
        assert_eq!(first["kind"], "status");
        assert_eq!(first["body"], "panel committed");
        assert!(first["id"].as_str().unwrap().starts_with("asc_"));
        let _ = std::fs::remove_file(&path);
    }
}
