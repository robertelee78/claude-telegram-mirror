//! ADR-016 end-to-end: each host observer against the REAL host binary.
//!
//! These tests are `#[ignore]` by default because they need the host CLI installed and
//! start real server processes. Run them with:
//!
//!   cargo test --test host_e2e -- --ignored
//!
//! They cost nothing: session creation on both hosts emits the session-created event
//! without a model turn. What they prove that the unit tests cannot: the transport
//! (SSE / WebSocket-over-Unix), auth, the live event envelope, and the observer→daemon
//! socket leg, all against the actual binary version on this machine.
//!
//! A stand-in "daemon" Unix socket collects the `BridgeMessage`s the observer emits.

use ctm::config::{CodexHostConfig, Config, HostsConfig, OpenCodeHostConfig};
use ctm::types::{BridgeMessage, HostKind, MessageType};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

fn have(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kills a spawned server on drop, so a failing assertion never leaks a process.
struct ServerGuard(std::process::Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A stand-in daemon: accepts observer connections and forwards every parsed line.
async fn fake_daemon(path: PathBuf) -> mpsc::UnboundedReceiver<BridgeMessage> {
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let (r, _w) = stream.into_split();
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(m) = serde_json::from_str::<BridgeMessage>(&line) {
                        let _ = tx.send(m);
                    }
                }
            });
        }
    });
    rx
}

fn base_config(socket_path: PathBuf, hosts: HostsConfig) -> Config {
    Config {
        bot_token: String::new(),
        chat_id: 0,
        enabled: true,
        verbose: false,
        approvals: true,
        use_threads: false,
        chunk_size: 4000,
        rate_limit: 20,
        session_timeout: 3600,
        stale_session_timeout_hours: 24,
        auto_delete_topics: false,
        topic_delete_delay_minutes: 5,
        inactivity_delete_threshold_minutes: 720,
        socket_path: socket_path.clone(),
        config_dir: socket_path.parent().unwrap().to_path_buf(),
        config_path: socket_path.parent().unwrap().join("config.json"),
        forum_enabled: false,
        hosts,
    }
}

async fn wait_for<F: Fn(&BridgeMessage) -> bool>(
    rx: &mut mpsc::UnboundedReceiver<BridgeMessage>,
    pred: F,
    secs: u64,
) -> Option<BridgeMessage> {
    tokio::time::timeout(Duration::from_secs(secs), async {
        while let Some(m) = rx.recv().await {
            if pred(&m) {
                return Some(m);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test]
#[ignore = "needs the `opencode` binary; run with --ignored"]
async fn opencode_session_lifecycle_mirrors_through_real_server() {
    if !have("opencode") {
        eprintln!("skip: opencode not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("bridge.sock");
    let mut rx = fake_daemon(sock.clone()).await;

    // Real server on an ephemeral loopback port, authenticated.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let password = "ctm-e2e-secret";
    let server = Command::new("opencode")
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--hostname",
            "127.0.0.1",
        ])
        .env("OPENCODE_SERVER_PASSWORD", password)
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn opencode serve");
    let _server = ServerGuard(server);
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    // wait for /global/health
    let mut up = false;
    for _ in 0..60 {
        if let Ok(r) = http
            .get(format!("{base}/global/health"))
            .basic_auth("opencode", Some(password))
            .send()
            .await
        {
            if r.status().is_success() {
                up = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(up, "opencode serve did not come up on {base}");

    let oc = OpenCodeHostConfig {
        base_url: base.clone(),
        password_env: "CTM_E2E_OC_PW".into(),
    };
    std::env::set_var("CTM_E2E_OC_PW", password);
    let cfg = Arc::new(base_config(
        sock.clone(),
        HostsConfig {
            opencode: Some(oc.clone()),
            codex: None,
        },
    ));
    let cfg2 = Arc::clone(&cfg);
    let observer = tokio::spawn(async move { ctm::host::opencode::run_once(&cfg2, &oc).await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create a session via the API: emits session.created on /event -> SessionStart.
    let dirq = dir.path().to_string_lossy().to_string();
    let created: serde_json::Value = http
        .post(format!("{base}/session"))
        .query(&[("directory", dirq.as_str())])
        .basic_auth("opencode", Some(password))
        .json(&serde_json::json!({"title": "ctm-e2e"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = created["id"].as_str().unwrap().to_string();
    assert!(sid.starts_with("ses_"));

    let start = wait_for(
        &mut rx,
        |m| m.msg_type == MessageType::SessionStart && m.session_id == sid,
        10,
    )
    .await
    .expect("SessionStart reached the daemon socket");
    assert_eq!(start.meta().host_kind(), HostKind::OpenCode);
    assert_eq!(start.meta().host_session_id(), Some(sid.as_str()));
    // OpenCode reports the canonical directory (macOS: /var -> /private/var).
    let canon = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(
        start
            .meta()
            .project_dir()
            .map(std::fs::canonicalize)
            .and_then(Result::ok),
        Some(canon)
    );

    // Delete it: session.deleted -> SessionEnd.
    let del = http
        .delete(format!("{base}/session/{sid}"))
        .query(&[("directory", dirq.as_str())])
        .basic_auth("opencode", Some(password))
        .send()
        .await
        .unwrap();
    assert!(del.status().is_success(), "delete -> {}", del.status());
    let end = wait_for(
        &mut rx,
        |m| m.msg_type == MessageType::SessionEnd && m.session_id == sid,
        10,
    )
    .await
    .expect("SessionEnd reached the daemon socket");
    assert_eq!(end.content, "deleted");

    observer.abort();
    // `_server` (ServerGuard) kills the server on drop.
}

#[tokio::test]
#[ignore = "needs the `codex` binary; run with --ignored"]
async fn codex_thread_lifecycle_mirrors_through_real_app_server() {
    if !have("codex") {
        eprintln!("skip: codex not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("bridge.sock");
    let mut rx = fake_daemon(sock.clone()).await;

    // The managed daemon: idempotent start; we stop it only if WE started it.
    let start_out = Command::new("codex")
        .args(["app-server", "daemon", "start"])
        .output()
        .expect("codex app-server daemon start");
    let started_json: serde_json::Value =
        serde_json::from_slice(&start_out.stdout).unwrap_or(serde_json::Value::Null);
    let we_started = started_json["status"] == "started";
    let socket_path = started_json["socketPath"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| CodexHostConfig::default().socket_path);
    assert!(
        socket_path.exists(),
        "no app-server socket at {}",
        socket_path.display()
    );

    let cx = CodexHostConfig {
        socket_path: socket_path.clone(),
    };
    let cfg = Arc::new(base_config(
        sock.clone(),
        HostsConfig {
            opencode: None,
            codex: Some(cx.clone()),
        },
    ));
    let cfg2 = Arc::clone(&cfg);
    let observer = tokio::spawn(async move { ctm::host::codex::run_once(&cfg2, &cx).await });
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Start a thread over the same app-server from a second client (no model turn):
    // thread/started fires to all clients -> observer announces SessionStart.
    let script = format!(
        r#"
import net from 'node:net'; import crypto from 'node:crypto';
const s = net.connect({sock:?});
let hs=false, buf=Buffer.alloc(0), n=0;
const frame=p=>{{const d=Buffer.from(p),m=crypto.randomBytes(4);let h;const L=d.length;
 if(L<126)h=Buffer.from([0x81,0x80|L]);else{{h=Buffer.alloc(4);h[0]=0x81;h[1]=0x80|126;h.writeUInt16BE(L,2);}}
 const k=Buffer.alloc(L);for(let i=0;i<L;i++)k[i]=d[i]^m[i%4];return Buffer.concat([h,m,k]);}};
const send=o=>s.write(frame(JSON.stringify(o)));
s.on('data',d=>{{ if(!hs){{const t=d.toString('latin1');const i=t.indexOf('\r\n\r\n');if(i>=0){{hs=true;
   send({{jsonrpc:'2.0',id:1,method:'initialize',params:{{clientInfo:{{name:'ctm-e2e',title:'ctm-e2e',version:'0'}}}}}});}}return;}}
 buf=Buffer.concat([buf,d]);
 while(buf.length>=2){{const l0=buf[1]&0x7f;let o=2,l=l0;if(l0===126){{if(buf.length<4)return;l=buf.readUInt16BE(2);o=4;}}
   if(buf.length<o+l)return;const p=buf.slice(o,o+l).toString();buf=buf.slice(o+l);
   try{{const m=JSON.parse(p); if(m.id===1){{send({{jsonrpc:'2.0',id:2,method:'thread/start',params:{{cwd:{cwd:?},sandbox:'read-only',approvalPolicy:'on-request'}}}});}}
       if(m.id===2){{console.log(JSON.stringify(m)); s.end(); }} }}catch{{}} }} }});
s.write('GET / HTTP/1.1\r\nHost: l\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: '+crypto.randomBytes(16).toString('base64')+'\r\nSec-WebSocket-Version: 13\r\n\r\n');
"#,
        sock = socket_path.to_string_lossy(),
        cwd = dir.path().to_string_lossy(),
    );
    let out = Command::new("node")
        .args(["--input-type=module", "-e", &script])
        .output()
        .expect("node available for the second client");
    let resp: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "thread/start reply: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let tid = resp["result"]["thread"]["id"]
        .as_str()
        .expect("thread id")
        .to_string();

    let start = wait_for(
        &mut rx,
        |m| m.msg_type == MessageType::SessionStart && m.session_id == tid,
        15,
    )
    .await
    .expect("SessionStart for the new Codex thread reached the daemon socket");
    assert_eq!(start.meta().host_kind(), HostKind::Codex);
    assert_eq!(start.meta().host_session_id(), Some(tid.as_str()));

    observer.abort();
    // Clean up: archive the probe thread and stop the daemon only if we started it.
    let _ = Command::new("codex").args(["delete", &tid]).output();
    if we_started {
        let _ = Command::new("codex")
            .args(["app-server", "daemon", "stop"])
            .output();
    }
}
