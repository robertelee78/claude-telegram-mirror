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

    // Exercise the config-file password path (what a launchd-managed daemon uses).
    let oc = OpenCodeHostConfig {
        enabled: true,
        base_url: Some(base.clone()),
        password_env: "CTM_E2E_OC_PW_UNSET".into(),
        password: Some(password.into()),
    };
    let cfg = Arc::new(base_config(
        sock.clone(),
        HostsConfig {
            opencode: oc.clone(),
            codex: CodexHostConfig {
                enabled: false,
                ..Default::default()
            },
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

/// Like `fake_daemon`, but also lets the test write daemon→observer messages down the
/// most recent observer connection (what `host_dispatch::send_to_host_observer` does).
async fn fake_daemon_rw(
    path: PathBuf,
) -> (
    mpsc::UnboundedReceiver<BridgeMessage>,
    mpsc::UnboundedSender<BridgeMessage>,
) {
    use tokio::io::AsyncWriteExt;
    let listener = UnixListener::bind(&path).unwrap();
    let (up_tx, up_rx) = mpsc::unbounded_channel();
    let (down_tx, mut down_rx) = mpsc::unbounded_channel::<BridgeMessage>();
    let writers: Arc<tokio::sync::Mutex<Vec<tokio::net::unix::OwnedWriteHalf>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let w2 = Arc::clone(&writers);
    tokio::spawn(async move {
        while let Some(m) = down_rx.recv().await {
            let line = format!("{}\n", serde_json::to_string(&m).unwrap());
            if let Some(w) = w2.lock().await.last_mut() {
                let _ = w.write_all(line.as_bytes()).await;
            }
        }
    });
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let (r, w) = stream.into_split();
            writers.lock().await.push(w);
            let tx = up_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(m) = serde_json::from_str::<BridgeMessage>(&line) {
                        let _ = tx.send(m);
                    }
                }
            });
        }
    });
    (up_rx, down_tx)
}

/// ADR-016 §Default enablement: the pipe plugin ctm provisions makes an OpenCode
/// process mirror itself with NO port configured for ctm and NO password. `serve` is
/// used only because a TUI needs a terminal; the plugin loads identically in both
/// (spike-verified), and the HTTP port here is the *test's* handle on the process,
/// not ctm's.
#[tokio::test]
#[ignore = "needs the `opencode` binary; run with --ignored"]
async fn opencode_pipe_plugin_mirrors_a_process_with_no_port_and_no_password() {
    use ctm::host::opencode_plugin;
    if !have("opencode") {
        eprintln!("skip: opencode not installed");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("bridge.sock");
    let (mut rx, down) = fake_daemon_rw(sock.clone()).await;

    // What the daemon's keeper writes — into an isolated XDG_CONFIG_HOME here.
    let xdg = dir.path().join("xdg");
    let plugin = xdg
        .join("opencode/plugins")
        .join(opencode_plugin::PLUGIN_FILE);
    let pipe_sock = opencode_plugin::pipe_socket_path(&sock);
    assert_eq!(
        opencode_plugin::ensure_at(&plugin, &pipe_sock).unwrap(),
        opencode_plugin::PluginState::Installed
    );

    let cfg = Arc::new(base_config(sock.clone(), HostsConfig::default()));
    let cfg2 = Arc::clone(&cfg);
    let pipe_server = tokio::spawn(async move { ctm::host::opencode_pipe::serve(cfg2).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(pipe_sock.exists(), "pipe socket bound");

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let proj = dir.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let server = Command::new("opencode")
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--hostname",
            "127.0.0.1",
        ])
        .env("XDG_CONFIG_HOME", &xdg)
        .env_remove("OPENCODE_SERVER_PASSWORD")
        .current_dir(&proj)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn opencode serve");
    let _server = ServerGuard(server);
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    let mut up = false;
    for _ in 0..80 {
        if let Ok(r) = http.get(format!("{base}/global/health")).send().await {
            if r.status().is_success() {
                up = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(up, "opencode serve did not come up on {base}");

    // Spike-established (opencode 1.18.31): the FIRST API request to an instance is
    // invisible to plugin hooks — whatever it is. A TUI issues many before the user's
    // first prompt, so this only shows up in a headless test; warm the instance up the
    // same way.
    let projq = proj.to_string_lossy().to_string();
    let _ = http
        .get(format!("{base}/session"))
        .query(&[("directory", projq.as_str())])
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 1. Event leg: session.created arrives through the plugin → SessionStart.
    let created: serde_json::Value = http
        .post(format!("{base}/session"))
        .query(&[("directory", projq.as_str())])
        .json(&serde_json::json!({"title": "ctm-pipe-e2e"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = created["id"].as_str().unwrap().to_string();
    let start = wait_for(
        &mut rx,
        |m| m.msg_type == MessageType::SessionStart && m.session_id == sid,
        15,
    )
    .await
    .expect("SessionStart via the pipe");
    assert_eq!(start.meta().host_kind(), HostKind::OpenCode);
    assert_eq!(start.meta().host_session_id(), Some(sid.as_str()));

    // 2. Call leg: a daemon→observer HostInject `/rename` becomes a PATCH executed by
    //    OpenCode's in-process client. Observable through the server's own API.
    let mut meta = serde_json::Map::new();
    meta.insert("action".into(), serde_json::Value::String("slash".into()));
    down.send(BridgeMessage {
        msg_type: MessageType::HostInject,
        session_id: sid.clone(),
        timestamp: String::new(),
        content: "/rename Renamed By Pipe".into(),
        metadata: Some(meta),
    })
    .unwrap();
    let mut renamed = false;
    for _ in 0..40 {
        let info: serde_json::Value = http
            .get(format!("{base}/session/{sid}"))
            .query(&[("directory", projq.as_str())])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if info["title"] == "Renamed By Pipe" {
            renamed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        renamed,
        "rename delivered through the pipe and executed in-process"
    );

    // 3. Process exit: the pipe closes → every announced session is ended.
    drop(_server);
    let end = wait_for(
        &mut rx,
        |m| m.msg_type == MessageType::SessionEnd && m.session_id == sid,
        15,
    )
    .await
    .expect("SessionEnd when the OpenCode process goes away");
    assert_eq!(end.content, "opencode exited");

    pipe_server.abort();
}

/// ADR-016 §Codex outbound: the hook path, end to end against the REAL codex binary
/// and its REAL app-server — install the hook file into an isolated CODEX_HOME, have
/// the app-server compute its hashes (`hooks/list`), trust them through Codex's own
/// config RPC (`config/batchWrite`), then run `ctm codex-hook` exactly as Codex would
/// and assert the messages reach the daemon socket on the same session id.
#[tokio::test]
#[ignore = "needs the `codex` binary; run with --ignored"]
async fn codex_hooks_install_trust_and_forward_to_the_daemon() {
    use ctm::host::{codex_hooks, codex_rpc};
    if !have("codex") {
        eprintln!("skip: codex not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("bridge.sock");
    let mut rx = fake_daemon(sock.clone()).await;

    // 1. Install into an isolated CODEX_HOME so the operator's own hooks.json and
    // config.toml are untouched. The daemon insists on finding the managed standalone
    // install under CODEX_HOME, so link the real one in rather than copying 220 MB.
    // The path is short on purpose: the app-server's control socket lives under CODEX_HOME and
    // macOS temp dirs blow past the 104-byte sockaddr_un limit ("path must be shorter
    // than SUN_LEN").
    let codex_home = PathBuf::from(format!("/tmp/ctm-e2e-cx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&codex_home);
    std::fs::create_dir_all(&codex_home).unwrap();
    struct HomeGuard(PathBuf);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            let _ = Command::new("codex")
                .args(["app-server", "daemon", "stop"])
                .env("CODEX_HOME", &self.0)
                .output();
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _home_guard = HomeGuard(codex_home.clone());
    let real_home = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".codex"));
    if !real_home.join("packages/standalone/current").exists() {
        eprintln!("skip: no managed standalone codex install to link");
        return;
    }
    std::os::unix::fs::symlink(real_home.join("packages"), codex_home.join("packages")).unwrap();
    if real_home.join("auth.json").exists() {
        let _ =
            std::os::unix::fs::symlink(real_home.join("auth.json"), codex_home.join("auth.json"));
    }
    let hooks_file = codex_home.join("hooks.json");
    let exe = env!("CARGO_BIN_EXE_ctm");
    assert_eq!(
        codex_hooks::ensure_at(&hooks_file, std::path::Path::new(exe)).unwrap(),
        codex_hooks::HooksState::Installed
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks_file).unwrap()).unwrap();
    for event in codex_hooks::EVENTS {
        assert!(doc["hooks"][*event].is_array(), "{event} written");
    }

    // 2. Codex itself validates the file and computes each hook's hash. Its daemon is
    //    keyed by CODEX_HOME, so this one is separate from the operator's.
    let start = Command::new("codex")
        .args(["app-server", "daemon", "start"])
        .env("CODEX_HOME", &codex_home)
        .output()
        .expect("codex app-server daemon start");
    let started: serde_json::Value =
        serde_json::from_slice(&start.stdout).unwrap_or(serde_json::Value::Null);
    let socket_path = started["socketPath"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "daemon reported no socketPath\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&start.stdout),
                String::from_utf8_lossy(&start.stderr)
            )
        });
    let mut rpc = codex_rpc::Rpc::connect(&socket_path).await.unwrap();
    let listed = rpc.call("hooks/list", serde_json::json!({})).await.unwrap();
    let ours = codex_hooks::ours_from_list(&listed, &hooks_file);
    assert_eq!(
        ours.len(),
        codex_hooks::EVENTS.len(),
        "codex parsed and listed every ctm hook: {listed}"
    );
    assert!(
        ours.iter().all(|h| h.current_hash.starts_with("sha256:")),
        "codex computed a hash for each"
    );

    // 3. Trust them through Codex's own config writer, then confirm Codex agrees.
    let edits: Vec<serde_json::Value> = ours.iter().map(codex_hooks::trust_edit).collect();
    rpc.call("config/batchWrite", serde_json::json!({ "edits": edits }))
        .await
        .unwrap();
    let relisted = rpc.call("hooks/list", serde_json::json!({})).await.unwrap();
    let after = codex_hooks::ours_from_list(&relisted, &hooks_file);
    assert!(
        after.iter().all(|h| h.trusted),
        "every ctm hook is trusted after the config write: {relisted}"
    );

    // 4. Run the forwarder exactly as Codex runs it: payload on stdin, `{}` on stdout.
    let session_id = "01a0bbe8-61f0-73a2-9617-ea9855a402cb";
    let payloads = [
        serde_json::json!({"session_id": session_id, "cwd": dir.path(), "hook_event_name": "SessionStart", "model": "gpt-6-astra", "permission_mode": "default", "source": "startup"}),
        serde_json::json!({"session_id": session_id, "turn_id": "t1", "cwd": dir.path(), "hook_event_name": "Stop", "stop_hook_active": false, "last_assistant_message": "MIRRORED_OUT"}),
    ];
    for payload in &payloads {
        let mut child = Command::new(exe)
            .arg("codex-hook")
            .env("TELEGRAM_BRIDGE_SOCKET", &sock)
            .env("TELEGRAM_MIRROR", "true")
            .env("TELEGRAM_BOT_TOKEN", "1:test")
            .env("TELEGRAM_CHAT_ID", "-1001234567890")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn ctm codex-hook");
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "hook exits 0");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "{}",
            "hook returns a neutral decision"
        );
    }

    let start_msg = wait_for(
        &mut rx,
        |m| m.msg_type == MessageType::SessionStart && m.session_id == session_id,
        10,
    )
    .await
    .expect("SessionStart reached the daemon");
    assert_eq!(start_msg.meta().host_kind(), HostKind::Codex);
    assert_eq!(start_msg.meta().host_session_id(), Some(session_id));
    // The hook transport must not be registered as the injection observer.
    assert_eq!(
        start_msg.metadata.as_ref().unwrap()["hostTransport"],
        "hook"
    );

    let reply = wait_for(
        &mut rx,
        |m| m.msg_type == MessageType::AgentResponse && m.session_id == session_id,
        10,
    )
    .await
    .expect("the agent's final message reached the daemon");
    assert_eq!(reply.content, "MIRRORED_OUT");
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
        enabled: true,
        socket_path: socket_path.clone(),
        binary: None,
    };
    let cfg = Arc::new(base_config(
        sock.clone(),
        HostsConfig {
            opencode: OpenCodeHostConfig {
                enabled: false,
                ..Default::default()
            },
            codex: cx.clone(),
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
