//! Ollama lifecycle (start it if needed, stop it only if we started it) and the small non-streaming calls.

use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const DEFAULT_PORT: u16 = 11434;

pub fn base(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EnsureInfo {
    /// "running" (was already up), "started" (we launched it) or "failed"
    pub state: &'static str,
    pub message: String,
}

pub struct Ollama {
    pub client: reqwest::Client,
    child: Mutex<Option<Child>>,
    vision: Mutex<HashMap<String, bool>>,
}

/// Inside an AppImage the loader path points at bundled libs; a child process must not inherit that.
pub fn clean_env(cmd: &mut Command) {
    if let (Ok(dir), Ok(ld)) = (std::env::var("APPDIR"), std::env::var("LD_LIBRARY_PATH")) {
        let kept: Vec<&str> = ld.split(':').filter(|p| !p.is_empty() && !p.starts_with(&dir)).collect();
        if kept.is_empty() {
            cmd.env_remove("LD_LIBRARY_PATH");
        } else {
            cmd.env("LD_LIBRARY_PATH", kept.join(":"));
        }
    }
}

impl Ollama {
    pub fn new() -> Ollama {
        // reqwest is built without a bundled TLS provider (smaller, no aws-lc); install ring once per process.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .no_proxy() // Ollama is on loopback; never route it (or anything else of ours) through a proxy
            .build()
            .expect("http client");
        Ollama { client, child: Mutex::new(None), vision: Mutex::new(HashMap::new()) }
    }

    pub async fn is_up(&self, port: u16) -> bool {
        self.client
            .get(format!("{}/api/version", base(port)))
            .timeout(Duration::from_millis(1500))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    fn spawn(port: u16) -> std::io::Result<Child> {
        let mut last = None;
        for bin in ["ollama", "/usr/local/bin/ollama", "/usr/bin/ollama"] {
            let mut cmd = Command::new(bin);
            cmd.arg("serve").env("OLLAMA_HOST", format!("127.0.0.1:{port}")).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            clean_env(&mut cmd);
            cmd.process_group(0); // own group so shutdown can signal the runner subprocesses too
            match cmd.spawn() {
                Ok(c) => return Ok(c),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("ollama not found")))
    }

    /// Make sure Ollama answers on `port`. Starts `ollama serve` only when nothing is listening.
    pub async fn ensure(&self, port: u16) -> EnsureInfo {
        if self.is_up(port).await {
            return EnsureInfo { state: "running", message: String::new() };
        }
        {
            let mut g = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(c) = g.as_mut() {
                if !matches!(c.try_wait(), Ok(None)) {
                    *g = None; // our earlier child has exited
                }
            }
            if g.is_none() {
                match Self::spawn(port) {
                    Ok(c) => *g = Some(c),
                    Err(e) => {
                        return EnsureInfo {
                            state: "failed",
                            message: format!("Ollama is not running and `ollama serve` could not be started ({e}). Is Ollama installed?"),
                        }
                    }
                }
            }
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(25) {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if self.is_up(port).await {
                return EnsureInfo { state: "started", message: String::new() };
            }
            let exited = {
                let mut g = self.child.lock().unwrap_or_else(|e| e.into_inner());
                g.as_mut().map(|c| !matches!(c.try_wait(), Ok(None))).unwrap_or(false)
            };
            if exited {
                *self.child.lock().unwrap_or_else(|e| e.into_inner()) = None;
                return EnsureInfo { state: "failed", message: format!("`ollama serve` exited immediately. Is something else using port {port}?") };
            }
        }
        EnsureInfo { state: "failed", message: format!("Ollama did not become ready on port {port} within 25s.") }
    }

    pub async fn tags(&self, port: u16) -> Result<Vec<String>, String> {
        let r = self
            .client
            .get(format!("{}/api/tags", base(port)))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|_| format!("Cannot reach Ollama on port {port}. Is it running?"))?;
        let j: Value = r.json().await.map_err(|e| e.to_string())?;
        Ok(j["models"].as_array().map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect()).unwrap_or_default())
    }

    /// Vision support from /api/show capabilities. Unknown (older Ollama, request failed) counts as supported, like the prototype.
    pub async fn has_vision(&self, port: u16, model: &str) -> bool {
        if let Some(v) = self.vision.lock().unwrap_or_else(|e| e.into_inner()).get(model) {
            return *v;
        }
        let r = self.client.post(format!("{}/api/show", base(port))).timeout(Duration::from_secs(5)).json(&json!({ "model": model })).send().await;
        let Ok(r) = r else { return true };
        let Ok(j) = r.json::<Value>().await else { return true };
        match j["capabilities"].as_array() {
            Some(caps) => {
                let v = caps.iter().any(|c| c == "vision");
                self.vision.lock().unwrap_or_else(|e| e.into_inner()).insert(model.to_string(), v);
                v
            }
            None => true,
        }
    }

    /// Stop Ollama if (and only if) this app started it.
    pub fn shutdown(&self) {
        let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        let pgid = child.id() as i32;
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_uses_loopback() {
        assert_eq!(base(11434), "http://127.0.0.1:11434");
    }

    #[test]
    fn appimage_library_path_is_scrubbed() {
        std::env::set_var("APPDIR", "/tmp/.mount_chat");
        std::env::set_var("LD_LIBRARY_PATH", "/tmp/.mount_chat/usr/lib:/opt/x");
        let mut c = Command::new("true");
        clean_env(&mut c);
        let v = c.get_envs().find(|(k, _)| *k == "LD_LIBRARY_PATH").and_then(|(_, v)| v.map(|v| v.to_string_lossy().to_string()));
        assert_eq!(v.as_deref(), Some("/opt/x"));
    }
}
