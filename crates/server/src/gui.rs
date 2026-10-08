//! `iphone-use gui`: one local page that walks the whole device chain and
//! starts the pieces, so a person can tell at a glance whether an agent can
//! drive the phone, and if not, which link is broken.
//!
//! The chain, in order (each check is skipped once an earlier one fails):
//! usbmuxd → phone → pairing / Developer Disk Image → go-ios → iOS 17+ tunnel
//! → runner installed → runner answering on the phone → relays on
//! 127.0.0.1:8100/9100 → daemon (`/agent/status`) → MCP bridge (`tools/list`).
//!
//! It is built for Windows, where go-ios starts the runner (no Xcode, no
//! LaunchAgent), but runs anywhere. The page is served on loopback only and
//! every API call must carry the per-launch nonce embedded in the page, so
//! another web page in the same browser cannot drive it.

use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

const PAGE: &str = include_str!("gui.html");
const RUNNER_PORT: u16 = 8100;
const MJPEG_PORT: u16 = 9100;
/// go-ios's tunnel agent (`--tunnel-info-port`, go-ios 1.3).
const TUNNEL_INFO_PORT: u16 = 28100;
const LOG_LINES: usize = 400;
/// `ios apps --list` takes a few seconds; the page polls every few.
const APPS_TTL: Duration = Duration::from_secs(60);

pub struct GuiOptions {
    pub port: u16,
    pub open: bool,
    pub ios: Option<PathBuf>,
    pub udid: Option<String>,
    pub bundle_id: Option<String>,
    pub daemon_port: u16,
}

type Log = Arc<Mutex<VecDeque<String>>>;
type AppsCache = Option<(Instant, Result<Vec<String>, String>)>;

struct Proc {
    child: Child,
    command: String,
    started: Instant,
    log: Log,
}

struct Gui {
    nonce: String,
    exe: PathBuf,
    ios: Mutex<Option<PathBuf>>,
    udid: Mutex<Option<String>>,
    bundle_id: Mutex<Option<String>>,
    daemon_port: u16,
    token: String,
    procs: tokio::sync::Mutex<HashMap<String, Proc>>,
    /// Exit lines of processes that ended, so their logs stay readable.
    ended: Mutex<HashMap<String, (String, Log)>>,
    apps_cache: Mutex<AppsCache>,
    http: reqwest::Client,
}

pub async fn run(options: GuiOptions) -> Result<()> {
    let token = agent_token().context("prepare the agent token")?;
    let gui = Arc::new(Gui {
        nonce: random_hex(16)?,
        exe: std::env::current_exe().context("locate iphone-use")?,
        ios: Mutex::new(options.ios.or_else(find_ios)),
        udid: Mutex::new(options.udid),
        bundle_id: Mutex::new(options.bundle_id),
        daemon_port: options.daemon_port,
        token,
        procs: tokio::sync::Mutex::new(HashMap::new()),
        ended: Mutex::new(HashMap::new()),
        apps_cache: Mutex::new(None),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?,
    });
    let app = Router::new()
        .route("/", get(page))
        .route("/api/checks", get(checks))
        .route("/api/procs", get(procs))
        .route("/api/config", post(set_config))
        .route("/api/action/:name", post(action))
        .route("/api/stop/:name", post(stop))
        .route("/api/test/:name", post(test))
        .with_state(gui.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port))
        .await
        .with_context(|| format!("bind 127.0.0.1:{}", options.port))?;
    let url = format!("http://127.0.0.1:{}/", options.port);
    eprintln!("iphone-use diagnostics: {url}  (Ctrl+C stops it and everything it started)");
    if options.open {
        let _ = crate::platform::open_url(&url);
    }
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("serve the diagnostics page")?;
    gui.stop_all().await;
    Ok(())
}

// ── Routes ──────────────────────────────────────────────────────────────────

async fn page(State(gui): State<Arc<Gui>>, headers: HeaderMap) -> Response {
    if !host_is_local(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    Html(PAGE.replace("__NONCE__", &gui.nonce)).into_response()
}

/// Loopback Host (DNS rebinding) and the page's nonce (cross-site requests:
/// a custom header forces a CORS preflight this server never approves).
fn authorized(gui: &Gui, headers: &HeaderMap) -> bool {
    host_is_local(headers)
        && headers
            .get("x-iu-gui")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == gui.nonce)
}

fn host_is_local(headers: &HeaderMap) -> bool {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

macro_rules! guard {
    ($gui:expr, $headers:expr) => {
        if !authorized(&$gui, &$headers) {
            return (StatusCode::FORBIDDEN, Json(json!({"ok": false, "error": "forbidden"})))
                .into_response();
        }
    };
}

async fn checks(State(gui): State<Arc<Gui>>, headers: HeaderMap) -> Response {
    guard!(gui, headers);
    Json(gui.run_checks().await).into_response()
}

async fn procs(State(gui): State<Arc<Gui>>, headers: HeaderMap) -> Response {
    guard!(gui, headers);
    Json(gui.proc_list().await).into_response()
}

async fn set_config(
    State(gui): State<Arc<Gui>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    guard!(gui, headers);
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    if body.get("udid").is_some() {
        *lock(&gui.udid) = text("udid");
    }
    if body.get("bundle_id").is_some() {
        *lock(&gui.bundle_id) = text("bundle_id");
    }
    if body.get("ios").is_some() {
        *lock(&gui.ios) = text("ios").map(PathBuf::from).or_else(find_ios);
        *lock(&gui.apps_cache) = None;
    }
    Json(json!({"ok": true})).into_response()
}

async fn action(
    State(gui): State<Arc<Gui>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    guard!(gui, headers);
    let result = match name.as_str() {
        "tunnel" => gui.start_tunnel().await,
        "ddi" => gui.mount_ddi().await,
        "runner" => gui.start_runner().await,
        "relays" => gui.start_relays().await,
        "daemon" => gui.start_daemon().await,
        "all" => gui.start_all().await,
        "refresh_apps" => {
            *lock(&gui.apps_cache) = None;
            Ok("will re-read the installed apps".to_string())
        }
        _ => Err(anyhow!("unknown action {name}")),
    };
    reply(result)
}

async fn stop(
    State(gui): State<Arc<Gui>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    guard!(gui, headers);
    if name == "all" {
        gui.stop_all().await;
        return reply(Ok("stopped everything this page started".into()));
    }
    reply(gui.stop_proc(&name).await)
}

async fn test(
    State(gui): State<Arc<Gui>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    guard!(gui, headers);
    let started = Instant::now();
    let result = gui.run_test(&name).await;
    let ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(mut value) => {
            value["ok"] = json!(true);
            value["ms"] = json!(ms);
            Json(value).into_response()
        }
        Err(error) => {
            Json(json!({"ok": false, "ms": ms, "error": format!("{error:#}")})).into_response()
        }
    }
}

fn reply(result: Result<String>) -> Response {
    match result {
        Ok(message) => Json(json!({"ok": true, "message": message})).into_response(),
        Err(error) => Json(json!({"ok": false, "error": format!("{error:#}")})).into_response(),
    }
}

// ── Checks ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Ok,
    Warn,
    Fail,
    Skip,
}

struct Check {
    id: &'static str,
    title: &'static str,
    level: Level,
    detail: String,
    hint: String,
    action: Option<&'static str>,
    ms: u64,
}

impl Check {
    fn new(id: &'static str, title: &'static str) -> Self {
        Check {
            id,
            title,
            level: Level::Skip,
            detail: String::new(),
            hint: String::new(),
            action: None,
            ms: 0,
        }
    }
    fn set(&mut self, level: Level, detail: impl Into<String>) -> &mut Self {
        self.level = level;
        self.detail = detail.into();
        self
    }
    fn hint(&mut self, hint: impl Into<String>) -> &mut Self {
        self.hint = hint.into();
        self
    }
    fn action(&mut self, action: &'static str) -> &mut Self {
        self.action = Some(action);
        self
    }
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "status": match self.level {
                Level::Ok => "ok",
                Level::Warn => "warn",
                Level::Fail => "fail",
                Level::Skip => "skip",
            },
            "detail": self.detail,
            "hint": self.hint,
            "action": self.action,
            "ms": self.ms,
        })
    }
}

async fn timed<T>(f: impl std::future::Future<Output = T>) -> (T, u64) {
    let started = Instant::now();
    let out = f.await;
    (out, started.elapsed().as_millis() as u64)
}

impl Gui {
    async fn run_checks(self: &Arc<Self>) -> Value {
        let mut list: Vec<Check> = Vec::new();
        let mut summary = json!({});

        // 1. usbmuxd
        let mut c = Check::new("usbmuxd", "usbmuxd（Apple Mobile Device Service）");
        let (devices, ms) = timed(crate::usbmux::list_attached()).await;
        c.ms = ms;
        let devices = match devices {
            Ok(devices) => {
                c.set(
                    Level::Ok,
                    format!("{} 已响应", crate::usbmux::mux_address_for_display()),
                );
                Some(devices)
            }
            Err(error) => {
                c.set(Level::Fail, format!("{error:#}")).hint(if cfg!(windows) {
                    "安装微软商店的 Apple Devices（或 iTunes），确认服务 Apple Mobile Device Service 在运行"
                } else {
                    "usbmuxd 没有响应"
                });
                None
            }
        };
        list.push(c);

        // 2. the phone
        let mut c = Check::new("device", "iPhone 连接");
        let mut udid = None;
        let mut ios_major = 0u32;
        if let Some(devices) = &devices {
            let wanted = lock(&self.udid).clone();
            let pick = match &wanted {
                Some(want) => devices
                    .iter()
                    .find(|(serial, _)| {
                        crate::usbmux::normalize_udid(serial) == crate::usbmux::normalize_udid(want)
                    })
                    .map(|(serial, _)| serial.clone()),
                None if devices.len() == 1 => Some(devices[0].0.clone()),
                None => None,
            };
            match pick {
                None if devices.is_empty() => {
                    c.set(Level::Fail, "没有发现 iPhone")
                        .hint("用 USB 连接 iPhone、解锁，并在手机上点“信任”");
                }
                None => {
                    c.set(
                        Level::Fail,
                        format!(
                            "发现 {} 台：{}",
                            devices.len(),
                            devices
                                .iter()
                                .map(|d| d.0.as_str())
                                .collect::<Vec<_>>()
                                .join("、")
                        ),
                    )
                    .hint("在上方填写要用的 UDID");
                }
                Some(serial) => {
                    let (info, ms) = timed(crate::lockdown::device_info(&serial)).await;
                    c.ms = ms;
                    match info {
                        Ok(info) => {
                            ios_major = info
                                .product_version
                                .as_deref()
                                .and_then(|v| v.split('.').next())
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            c.set(
                                Level::Ok,
                                format!(
                                    "{} · iOS {} · {} · {}",
                                    info.name.as_deref().unwrap_or("?"),
                                    info.product_version.as_deref().unwrap_or("?"),
                                    info.product_type.as_deref().unwrap_or("?"),
                                    info.connection
                                ),
                            );
                            summary = json!({
                                "udid": serial,
                                "name": info.name,
                                "ios": info.product_version,
                            });
                        }
                        Err(error) => {
                            c.set(Level::Fail, format!("{error:#}"))
                                .hint("解锁手机并点“信任”；仍失败就重新插拔数据线");
                        }
                    }
                    udid = Some(serial);
                }
            }
        }
        let phone_ok = c.level == Level::Ok;
        list.push(c);

        // 3. pairing + Developer Disk Image (informational: go-ios mounts it)
        let mut c = Check::new("ddi", "配对记录 / 开发者磁盘镜像");
        if phone_ok {
            let serial = udid.clone().unwrap_or_default();
            let (ddi, ms) = timed(crate::lockdown::ddi_status(&serial)).await;
            c.ms = ms;
            match ddi {
                Ok(status) if status.mounted => {
                    c.set(
                        Level::Ok,
                        format!("已挂载 {}", status.image_type.as_deref().unwrap_or("")),
                    );
                }
                Ok(_) => {
                    c.set(Level::Warn, "未挂载")
                        .hint("点“挂载”（go-ios image auto）；需要先打开开发者模式")
                        .action("ddi");
                }
                Err(error) => {
                    c.set(Level::Warn, format!("无法读取：{error:#}"))
                        .hint("只影响这一项显示；go-ios 会自己挂载镜像。若后面也失败，在手机上重新“信任”这台电脑")
                        .action("ddi");
                }
            }
        }
        list.push(c);

        // 4. go-ios
        let mut c = Check::new("goios", "go-ios（ios 命令）");
        let ios = lock(&self.ios).clone();
        let mut ios_ok = false;
        match &ios {
            None => {
                c.set(Level::Fail, "没有找到 ios 可执行文件").hint(
                    "从 github.com/danielpaulus/go-ios/releases 下载，放到 iphone-use 同目录，或在上方填写路径",
                );
            }
            Some(path) => {
                let (out, ms) =
                    timed(run_capture(path, &["version"], Duration::from_secs(15))).await;
                c.ms = ms;
                match out {
                    Ok((0, stdout, _)) => {
                        let version = serde_json::from_str::<Value>(stdout.trim())
                            .ok()
                            .and_then(|v| {
                                v.get("version").and_then(Value::as_str).map(str::to_string)
                            })
                            .unwrap_or_else(|| stdout.trim().to_string());
                        c.set(Level::Ok, format!("{} · {}", version, path.display()));
                        ios_ok = true;
                    }
                    Ok((code, _, stderr)) => {
                        c.set(
                            Level::Fail,
                            format!("退出码 {code}：{}", last_line(&stderr)),
                        );
                    }
                    Err(error) => {
                        c.set(Level::Fail, format!("{error:#}"));
                    }
                }
            }
        }
        list.push(c);

        // 5. iOS 17+ tunnel
        let mut c = Check::new("tunnel", "iOS 17+ 隧道（go-ios tunnel）");
        if phone_ok && ios_ok {
            if ios_major < 17 {
                c.set(Level::Ok, format!("iOS {ios_major} 不需要"));
            } else if port_open(TUNNEL_INFO_PORT).await {
                c.set(
                    Level::Ok,
                    format!("隧道代理在 127.0.0.1:{TUNNEL_INFO_PORT} 运行"),
                );
            } else {
                c.set(Level::Fail, "没有运行")
                    .hint("点“启动”（ios tunnel start --userspace）")
                    .action("tunnel");
            }
        }
        let tunnel_ok = c.level == Level::Ok;
        list.push(c);

        // 6. runner installed
        let mut c = Check::new("installed", "Runner 已安装");
        let mut bundle = lock(&self.bundle_id).clone();
        if phone_ok && ios_ok && tunnel_ok {
            if let Some(id) = &bundle {
                c.set(Level::Ok, format!("{id}（手动指定）"));
            } else {
                let (apps, ms) =
                    timed(self.installed_apps(udid.as_deref().unwrap_or_default())).await;
                c.ms = ms;
                match apps {
                    Ok(lines) => match find_runner(&lines) {
                        Some(id) => {
                            c.set(Level::Ok, id.clone());
                            bundle = Some(id);
                        }
                        None => {
                            c.set(Level::Fail, format!("在 {} 个应用里没有找到 iPhoneUse-Runner", lines.len()))
                                .hint("用 Sideloadly 安装 iPhoneUse-Runner-unsigned.ipa；如果改过 bundle id，在上方填写")
                                .action("refresh_apps");
                        }
                    },
                    Err(error) => {
                        c.set(Level::Fail, error).action("refresh_apps");
                    }
                }
            }
        }
        list.push(c);

        // 7. runner answering on the phone
        let mut c = Check::new("runner", "Runner 在手机上运行");
        let runner_proc = self.proc_state("runner").await;
        if phone_ok {
            let serial = udid.clone().unwrap_or_default();
            let (status, ms) = timed(crate::lockdown::runner_status(&serial, RUNNER_PORT)).await;
            c.ms = ms;
            match status {
                Ok(status) => {
                    c.set(
                        Level::Ok,
                        format!(
                            "端口 {RUNNER_PORT} 应答，状态 {}",
                            status.state.as_deref().unwrap_or("?")
                        ),
                    );
                }
                Err(error) => {
                    c.set(Level::Fail, format!("{error:#}"));
                    match runner_proc.as_deref() {
                        Some("running") => {
                            c.hint("ios runtest 正在启动，等几秒；一直不好就看下方 runner 日志（签名、开发者模式、隧道）");
                        }
                        Some(ended) => {
                            c.hint(format!("ios runtest 已退出（{ended}），看下方 runner 日志"))
                                .action("runner");
                        }
                        None => {
                            c.hint("点“启动”（ios runtest）").action("runner");
                        }
                    }
                }
            }
        }
        let runner_ok = c.level == Level::Ok;
        list.push(c);

        // 8. relays
        let mut c = Check::new("relays", "端口转发 127.0.0.1:8100 / 9100");
        let started = Instant::now();
        let control = self
            .http
            .get(format!("http://127.0.0.1:{RUNNER_PORT}/status"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        // The relay accepts even when the phone side is gone; only bytes of
        // the stream prove the video path.
        let video = match self
            .http
            .get(format!("http://127.0.0.1:{MJPEG_PORT}/"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(mut response) if response.status().is_success() => matches!(
                tokio::time::timeout(Duration::from_secs(3), response.chunk()).await,
                Ok(Ok(Some(_)))
            ),
            _ => false,
        };
        c.ms = started.elapsed().as_millis() as u64;
        match (control, video) {
            (true, true) => {
                c.set(Level::Ok, "8100 /status 正常，9100 有画面数据");
            }
            _ => {
                c.set(
                    Level::Fail,
                    format!(
                        "8100 {}，9100 {}",
                        if control { "正常" } else { "不通" },
                        if video {
                            "有画面数据"
                        } else {
                            "没有画面数据"
                        }
                    ),
                )
                .hint(if runner_ok {
                    "点“启动”（iphone-use relay ×2）"
                } else {
                    "先让 Runner 运行起来"
                })
                .action("relays");
            }
        }
        let relays_ok = c.level == Level::Ok;
        list.push(c);

        // 9. daemon
        let mut c = Check::new("daemon", "iphone-use 守护进程");
        let (status, ms) = timed(self.agent_get("/agent/status")).await;
        c.ms = ms;
        match status {
            Ok(status) => {
                let drivable = status.get("drivable").and_then(Value::as_bool) == Some(true);
                let state = status
                    .get("device_state")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                if drivable {
                    c.set(Level::Ok, format!("可驱动（device_state={state}）"));
                } else {
                    let daemon_hint = status
                        .get("hint")
                        .and_then(Value::as_str)
                        .filter(|h| !h.is_empty())
                        .unwrap_or("确认手机已解锁，等十几秒健康检查");
                    let hint = if !runner_ok {
                        "先修好上面的 Runner；它恢复后守护进程会自动重连".to_string()
                    } else if !relays_ok {
                        "先修好上面的端口转发".to_string()
                    } else {
                        format!("守护进程报告：{daemon_hint}")
                    };
                    c.set(
                        Level::Warn,
                        format!("运行中，但不可驱动（device_state={state}）"),
                    )
                    .hint(hint);
                }
                summary["drivable"] = json!(drivable);
            }
            Err(error) => {
                c.set(Level::Fail, format!("{error:#}"))
                    .action("daemon")
                    .hint(if relays_ok {
                        "点“启动”（iphone-use serve）"
                    } else {
                        "可以先启动；要能驱动手机还需要前面几步都通过"
                    });
            }
        }
        list.push(c);

        // 10. MCP
        let mut c = Check::new("mcp", "MCP 桥（iphone-use-mcp）");
        let mcp = mcp_binary(&self.exe);
        match &mcp {
            None => {
                c.set(Level::Fail, "iphone-use-mcp 不在 iphone-use 同目录");
            }
            Some(path) => {
                c.set(
                    Level::Ok,
                    format!("{}（点右侧“MCP 握手”实测）", path.display()),
                );
            }
        }
        list.push(c);

        let first_failure = list
            .iter()
            .find(|c| c.level == Level::Fail)
            .map(|c| c.title);
        json!({
            "checks": list.iter().map(Check::json).collect::<Vec<_>>(),
            "summary": summary,
            "first_failure": first_failure,
            "bundle_id": bundle,
            "ios_path": ios.map(|p| p.display().to_string()),
            "udid_override": lock(&self.udid).clone(),
            "daemon_url": format!("http://127.0.0.1:{}", self.daemon_port),
            "token_file": token_path().map(|p| p.display().to_string()),
            "mcp_path": mcp.map(|p| p.display().to_string()),
            "mcp_env": {
                "PHONE_REMOTE_URL": format!("http://127.0.0.1:{}", self.daemon_port),
                "PHONE_REMOTE_TOKEN": self.token,
            },
        })
    }

    async fn installed_apps(&self, udid: &str) -> Result<Vec<String>, String> {
        if let Some((at, cached)) = lock(&self.apps_cache).as_ref() {
            if at.elapsed() < APPS_TTL {
                return cached.clone();
            }
        }
        let ios = lock(&self.ios).clone().ok_or("没有 go-ios")?;
        let udid_arg = format!("--udid={udid}");
        let result = match run_capture(
            &ios,
            &["apps", "--list", &udid_arg],
            Duration::from_secs(40),
        )
        .await
        {
            Ok((0, stdout, _)) => Ok(stdout
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()),
            Ok((code, _, stderr)) => Err(format!("ios apps 退出码 {code}：{}", last_line(&stderr))),
            Err(error) => Err(format!("{error:#}")),
        };
        *lock(&self.apps_cache) = Some((Instant::now(), result.clone()));
        result
    }

    async fn current_udid(&self) -> Result<String> {
        if let Some(udid) = lock(&self.udid).clone() {
            return Ok(udid);
        }
        let devices = crate::usbmux::list_attached().await?;
        match devices.as_slice() {
            [one] => Ok(one.0.clone()),
            [] => bail!("没有发现 iPhone"),
            _ => bail!("连接了多台 iPhone，请先填写 UDID"),
        }
    }

    fn ios_path(&self) -> Result<PathBuf> {
        lock(&self.ios)
            .clone()
            .ok_or_else(|| anyhow!("没有找到 go-ios（ios 可执行文件）"))
    }

    // ── Actions ─────────────────────────────────────────────────────────────

    async fn start_tunnel(&self) -> Result<String> {
        if port_open(TUNNEL_INFO_PORT).await {
            return Ok("隧道代理已在运行".into());
        }
        let ios = self.ios_path()?;
        self.spawn("tunnel", &ios, &["tunnel", "start", "--userspace"], &[])
            .await?;
        for _ in 0..20 {
            if port_open(TUNNEL_INFO_PORT).await {
                return Ok("隧道代理已启动".into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok("已启动 ios tunnel，等待它就绪（看 tunnel 日志）".into())
    }

    async fn mount_ddi(&self) -> Result<String> {
        let ios = self.ios_path()?;
        let udid = format!("--udid={}", self.current_udid().await?);
        let (code, stdout, stderr) =
            run_capture(&ios, &["image", "auto", &udid], Duration::from_secs(180)).await?;
        if code != 0 {
            bail!(
                "ios image auto 退出码 {code}：{}",
                last_line(&format!("{stdout}\n{stderr}"))
            );
        }
        Ok("开发者磁盘镜像已挂载".into())
    }

    async fn start_runner(&self) -> Result<String> {
        let ios = self.ios_path()?;
        let udid = self.current_udid().await?;
        let configured = lock(&self.bundle_id).clone();
        let bundle = match configured {
            Some(id) => id,
            None => {
                let apps = self.installed_apps(&udid).await.map_err(|e| anyhow!(e))?;
                find_runner(&apps).ok_or_else(|| anyhow!("手机上没有找到 iPhoneUse-Runner"))?
            }
        };
        let args = [
            "runtest".to_string(),
            format!("--bundle-id={bundle}"),
            format!("--test-runner-bundle-id={bundle}"),
            "--xctest-config=iPhoneUse.xctest".to_string(),
            "--test-to-run=RunnerTests/testServe".to_string(),
            format!("--udid={udid}"),
        ];
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.spawn("runner", &ios, &args, &[]).await?;
        for _ in 0..60 {
            if crate::lockdown::runner_status(&udid, RUNNER_PORT)
                .await
                .is_ok()
            {
                return Ok("Runner 已在手机上运行".into());
            }
            if let Some(state) = self.proc_state("runner").await {
                if state != "running" {
                    bail!("ios runtest 已退出（{state}），看 runner 日志");
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok("ios runtest 已启动，Runner 还没应答（看 runner 日志）".into())
    }

    async fn start_relays(&self) -> Result<String> {
        let udid = self.current_udid().await?;
        let exe = self.exe.clone();
        for port in [RUNNER_PORT, MJPEG_PORT] {
            let name = format!("relay-{port}");
            if self.proc_state(&name).await.as_deref() == Some("running") {
                continue;
            }
            if port_open(port).await {
                bail!("127.0.0.1:{port} 已被其他程序占用（可能是之前没关掉的 relay）");
            }
            let listen = format!("127.0.0.1:{port}");
            let device_port = port.to_string();
            self.spawn(
                &name,
                &exe,
                &[
                    "relay",
                    "--udid",
                    &udid,
                    "--listen",
                    &listen,
                    "--device-port",
                    &device_port,
                ],
                &[],
            )
            .await?;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok("端口转发已启动".into())
    }

    async fn start_daemon(&self) -> Result<String> {
        if self.agent_get("/agent/status").await.is_ok() {
            return Ok("守护进程已在运行".into());
        }
        if port_open(self.daemon_port).await {
            bail!(
                "127.0.0.1:{} 已被占用，但不是用本页的 token 能访问的 iphone-use",
                self.daemon_port
            );
        }
        let udid = self.current_udid().await.ok();
        let port = self.daemon_port.to_string();
        let mut env = vec![
            ("PHONE_REMOTE_WDA_MANAGED", "0".to_string()),
            ("PHONE_REMOTE_HOST", "127.0.0.1".to_string()),
            ("PHONE_REMOTE_PORT", port),
            ("PHONE_REMOTE_AGENT_TOKEN", self.token.clone()),
        ];
        if let Some(udid) = udid {
            env.push(("PHONE_REMOTE_UDID", udid));
        }
        let exe = self.exe.clone();
        self.spawn("daemon", &exe, &["serve"], &env).await?;
        for _ in 0..30 {
            if self.agent_get("/agent/status").await.is_ok() {
                return Ok("守护进程已启动".into());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        bail!("守护进程没有应答，看 daemon 日志")
    }

    async fn start_all(self: &Arc<Self>) -> Result<String> {
        let mut done = Vec::new();
        let udid = self.current_udid().await?;
        let info = crate::lockdown::device_info(&udid).await?;
        let major: u32 = info
            .product_version
            .as_deref()
            .and_then(|v| v.split('.').next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if major >= 17 {
            done.push(self.start_tunnel().await.context("隧道")?);
        }
        if !matches!(crate::lockdown::ddi_status(&udid).await, Ok(s) if s.mounted) {
            done.push(self.mount_ddi().await.context("挂载开发者磁盘镜像")?);
        }
        if crate::lockdown::runner_status(&udid, RUNNER_PORT)
            .await
            .is_err()
        {
            done.push(self.start_runner().await.context("启动 Runner")?);
        }
        done.push(self.start_relays().await.context("端口转发")?);
        done.push(self.start_daemon().await.context("守护进程")?);
        Ok(done.join("；"))
    }

    // ── Tests through the daemon ────────────────────────────────────────────

    async fn run_test(&self, name: &str) -> Result<Value> {
        match name {
            "status" => {
                let status = self.agent_get("/agent/status").await?;
                Ok(json!({ "result": status }))
            }
            "elements" => {
                let elements = self.agent_get("/agent/elements").await?;
                let rows = elements
                    .get("elements")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let labels: Vec<String> = rows
                    .iter()
                    .filter_map(|r| {
                        let label = r.get("label").and_then(Value::as_str)?.trim();
                        let kind = r.get("kind").and_then(Value::as_str).unwrap_or("");
                        (!label.is_empty()).then(|| format!("{kind} “{label}”"))
                    })
                    .take(40)
                    .collect();
                Ok(json!({ "count": rows.len(), "labels": labels }))
            }
            "screenshot" => {
                let response = self
                    .http
                    .get(self.daemon_url("/agent/screenshot?max_side=900"))
                    .bearer_auth(&self.token)
                    .send()
                    .await
                    .context("GET /agent/screenshot")?;
                let status = response.status();
                let bytes = response.bytes().await?;
                if !status.is_success() {
                    bail!("HTTP {status}: {}", String::from_utf8_lossy(&bytes));
                }
                use base64::Engine as _;
                Ok(json!({
                    "png": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    "bytes": bytes.len(),
                }))
            }
            "home" => {
                let result = self
                    .agent_post("/agent/input", json!({"type": "shortcut", "name": "home"}))
                    .await?;
                Ok(json!({ "result": result }))
            }
            "mcp" => self.mcp_handshake().await,
            _ => bail!("unknown test {name}"),
        }
    }

    fn daemon_url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.daemon_port)
    }

    async fn agent_get(&self, path: &str) -> Result<Value> {
        let response = self
            .http
            .get(self.daemon_url(path))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(if path == "/agent/status" {
                4
            } else {
                45
            }))
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    anyhow!("127.0.0.1:{} 没有服务在监听", self.daemon_port)
                } else {
                    anyhow!(e)
                }
            })?;
        decode(response, path).await
    }

    async fn agent_post(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(self.daemon_url(path))
            .bearer_auth(&self.token)
            .header("X-Phone-Control", "1")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        decode(response, path).await
    }

    async fn mcp_handshake(&self) -> Result<Value> {
        let path = mcp_binary(&self.exe).ok_or_else(|| anyhow!("没有找到 iphone-use-mcp"))?;
        let mut command = Command::new(&path);
        command
            .env("PHONE_REMOTE_URL", self.daemon_url(""))
            .env("PHONE_REMOTE_TOKEN", &self.token)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        no_console_window(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("start {}", path.display()))?;
        let requests = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "iphone-use-gui", "version": env!("CARGO_PKG_VERSION")}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "phone_status", "arguments": {}}}),
        ];
        let mut stdin = child.stdin.take().context("MCP stdin")?;
        for request in &requests {
            stdin.write_all(format!("{request}\n").as_bytes()).await?;
        }
        stdin.flush().await?;
        let stdout = child.stdout.take().context("MCP stdout")?;
        let mut lines = BufReader::new(stdout).lines();
        let mut tools = None;
        let mut status_text = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tools.is_none() || status_text.is_none() {
            let line = match tokio::time::timeout_at(deadline, lines.next_line()).await {
                Ok(Ok(Some(line))) => line,
                _ => break,
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match message.get("id").and_then(Value::as_i64) {
                Some(2) => {
                    tools = message
                        .pointer("/result/tools")
                        .and_then(Value::as_array)
                        .map(|t| t.len());
                }
                Some(3) => {
                    status_text = Some(
                        message
                            .pointer("/result/content/0/text")
                            .and_then(Value::as_str)
                            .unwrap_or_else(|| message.get("error").map_or("", |_| "调用出错"))
                            .chars()
                            .take(600)
                            .collect::<String>(),
                    );
                }
                _ => {}
            }
        }
        drop(stdin);
        let _ = child.kill().await;
        let count = tools.ok_or_else(|| anyhow!("MCP 没有返回 tools/list"))?;
        Ok(json!({ "tools": count, "phone_status": status_text }))
    }

    // ── Processes ───────────────────────────────────────────────────────────

    async fn spawn(
        &self,
        name: &str,
        program: &std::path::Path,
        args: &[&str],
        env: &[(&str, String)],
    ) -> Result<()> {
        let mut procs = self.procs.lock().await;
        if let Some(existing) = procs.get_mut(name) {
            if existing.child.try_wait()?.is_none() {
                bail!("{name} 已在运行");
            }
            procs.remove(name);
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in env {
            command.env(key, value);
        }
        no_console_window(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("start {}", program.display()))?;
        let log = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(out) = child.stdout.take() {
            pump(out, log.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, log.clone());
        }
        let line = format!("{} {}", program.display(), args.join(" "));
        push_log(&log, format!("$ {line}"));
        procs.insert(
            name.to_string(),
            Proc {
                child,
                command: line,
                started: Instant::now(),
                log,
            },
        );
        Ok(())
    }

    /// `running`, `exit N`, or None when this page never started it.
    async fn proc_state(&self, name: &str) -> Option<String> {
        let mut procs = self.procs.lock().await;
        if let Some(proc) = procs.get_mut(name) {
            return Some(match proc.child.try_wait() {
                Ok(None) => "running".to_string(),
                Ok(Some(status)) => format!("exit {}", status.code().unwrap_or(-1)),
                Err(error) => format!("unknown: {error}"),
            });
        }
        lock(&self.ended).get(name).map(|(state, _)| state.clone())
    }

    async fn stop_proc(&self, name: &str) -> Result<String> {
        let mut procs = self.procs.lock().await;
        let mut proc = procs
            .remove(name)
            .ok_or_else(|| anyhow!("{name} 不是本页启动的"))?;
        let _ = proc.child.kill().await;
        push_log(&proc.log, "[stopped from the page]".to_string());
        lock(&self.ended).insert(name.to_string(), ("stopped".to_string(), proc.log.clone()));
        Ok(format!("{name} 已停止"))
    }

    async fn stop_all(&self) {
        let names: Vec<String> = self.procs.lock().await.keys().cloned().collect();
        // Dependents first: the daemon, then relays, then the runner and tunnel.
        let order = ["daemon", "relay-8100", "relay-9100", "runner", "tunnel"];
        for name in order.iter().map(|n| n.to_string()).chain(names) {
            let _ = self.stop_proc(&name).await;
        }
    }

    async fn proc_list(&self) -> Value {
        let mut out = Vec::new();
        let mut procs = self.procs.lock().await;
        for (name, proc) in procs.iter_mut() {
            let state = match proc.child.try_wait() {
                Ok(None) => "running".to_string(),
                Ok(Some(status)) => format!("exit {}", status.code().unwrap_or(-1)),
                Err(error) => format!("unknown: {error}"),
            };
            out.push(json!({
                "name": name,
                "pid": proc.child.id(),
                "state": state,
                "uptime_s": proc.started.elapsed().as_secs(),
                "command": proc.command,
                "log": lock(&proc.log).iter().cloned().collect::<Vec<_>>(),
            }));
        }
        for (name, (state, log)) in lock(&self.ended).iter() {
            if !procs.contains_key(name) {
                out.push(json!({
                    "name": name,
                    "pid": null,
                    "state": state,
                    "uptime_s": 0,
                    "command": "",
                    "log": lock(log).iter().cloned().collect::<Vec<_>>(),
                }));
            }
        }
        out.sort_by_key(|p| p["name"].as_str().unwrap_or_default().to_string());
        json!(out)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn push_log(log: &Mutex<VecDeque<String>>, line: String) {
    let mut log = lock(log);
    if log.len() >= LOG_LINES {
        log.pop_front();
    }
    log.push_back(line);
}

fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    log: Arc<Mutex<VecDeque<String>>>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            push_log(&log, strip_ansi(&line));
        }
    });
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// No console window flashing up for each child on Windows.
fn no_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

async fn run_capture(
    program: &std::path::Path,
    args: &[&str],
    limit: Duration,
) -> Result<(i32, String, String)> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    no_console_window(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("start {}", program.display()))?;
    let mut stdout = child.stdout.take().context("stdout")?;
    let mut stderr = child.stderr.take().context("stderr")?;
    let run = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        a?;
        b?;
        let status = child.wait().await?;
        anyhow::Ok((
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        ))
    };
    tokio::time::timeout(limit, run)
        .await
        .map_err(|_| anyhow!("{} {} 超时", program.display(), args.join(" ")))?
}

/// The last meaningful line of go-ios output (its logs are JSON lines).
fn last_line(text: &str) -> String {
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| {
            let msg = v.get("msg").and_then(Value::as_str)?.to_string();
            let err = v.get("err").and_then(Value::as_str).unwrap_or_default();
            Some(if err.is_empty() {
                msg
            } else {
                format!("{msg}: {err}")
            })
        })
        .unwrap_or_else(|| line.trim().to_string())
}

/// The runner among `ios apps --list` lines ("<bundle id> <name> <version>").
/// Sideloadly may rename the bundle id, so the app name counts too.
fn find_runner(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("xctrunner")
                && (lower.contains("iphone-use") || lower.contains("iphoneuse"))
        })
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string)
}

async fn port_open(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect(("127.0.0.1", port))
        )
        .await,
        Ok(Ok(_))
    )
}

async fn decode(response: reqwest::Response, path: &str) -> Result<Value> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("{path}: 401，守护进程不是用本页的 token 启动的（先停掉它，再从本页启动）");
    }
    let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
    if !status.is_success() {
        bail!("{path}: HTTP {status} {}", value);
    }
    Ok(value)
}

/// go-ios: `ios`/`ios.exe` next to iphone-use, else on PATH.
fn find_ios() -> Option<PathBuf> {
    let name = format!("ios{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let local = dir.join(&name);
            if local.is_file() {
                return Some(local);
            }
        }
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(&name))
            .find(|candidate| candidate.is_file())
    })
}

fn mcp_binary(exe: &std::path::Path) -> Option<PathBuf> {
    let candidate = exe
        .parent()?
        .join(format!("iphone-use-mcp{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

fn token_path() -> Option<PathBuf> {
    Some(
        crate::platform::home_dir()?
            .join(".iphone-use")
            .join("agent-token"),
    )
}

/// The same token file `scripts/windows/iphone-use.ps1` uses, so either can
/// start the daemon and the other still reaches it.
fn agent_token() -> Result<String> {
    let path = token_path().ok_or_else(|| anyhow!("no home directory"))?;
    if let Ok(text) = std::fs::read_to_string(&path) {
        let token = text.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let token = random_hex(24)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, &token).with_context(|| format!("write {}", path.display()))?;
    Ok(token)
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("random: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_runner_by_id_or_renamed() {
        let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            find_runner(&lines(&[
                "com.apple.Preferences Settings 18.6",
                "com.leeguoo.iphone-use.runner.xctrunner iPhoneUse-Runner 0.2.0",
            ])),
            Some("com.leeguoo.iphone-use.runner.xctrunner".into())
        );
        assert_eq!(
            find_runner(&lines(&[
                "com.ABC123.xctrunner.renamed iPhoneUse-Runner 0.2.0"
            ])),
            Some("com.ABC123.xctrunner.renamed".into())
        );
        assert_eq!(
            find_runner(&lines(&[
                "com.facebook.WebDriverAgentRunner.xctrunner WDA 1"
            ])),
            None
        );
    }

    #[test]
    fn last_line_reads_go_ios_json_logs() {
        let text = "{\"level\":\"INFO\",\"msg\":\"a\"}\n{\"level\":\"ERROR\",\"msg\":\"failed\",\"err\":\"no device\"}\n";
        assert_eq!(last_line(text), "failed: no device");
        assert_eq!(last_line("plain text\n\n"), "plain text");
    }

    #[test]
    fn only_loopback_hosts_are_served() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:44390".parse().unwrap());
        assert!(host_is_local(&headers));
        headers.insert("host", "localhost:44390".parse().unwrap());
        assert!(host_is_local(&headers));
        headers.insert("host", "evil.example:44390".parse().unwrap());
        assert!(!host_is_local(&headers));
    }

    #[test]
    fn ansi_codes_are_stripped() {
        assert_eq!(strip_ansi("\u{1b}[32m INFO\u{1b}[0m ok"), " INFO ok");
    }
}
