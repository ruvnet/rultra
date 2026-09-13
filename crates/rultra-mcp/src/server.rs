//! Tools and `ruv://` resources for the box.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
    ServiceExt,
};
use rultra_sense::{device, Backend, DeviceId, DeviceKind, Value, Verification};
use serde::Deserialize;
use serde_json::{json, Value as J};

/// Resource URIs, using the ruvnet `ruv://` convention.
const RES: &[(&str, &str, &str)] = &[
    (
        "ruv://rultra/devices",
        "devices",
        "Every device the board carries, with how well each is actually known to work",
    ),
    (
        "ruv://rultra/telemetry",
        "telemetry",
        "One reading from every sensor that answers",
    ),
    (
        "ruv://rultra/spatial",
        "spatial",
        "Fused room state and the normalized media steering signal",
    ),
    (
        "ruv://rultra/policy",
        "policy",
        "The sensing policy currently in force, and its bounds",
    ),
    (
        "ruv://rultra/witness",
        "witness",
        "The signed, hash-chained record of every decision the box has made",
    ),
    (
        "ruv://rultra/capabilities",
        "capabilities",
        "What this box can do, derived from the catalog rather than declared",
    ),
];

fn state_dir() -> std::path::PathBuf {
    std::env::var("RULTRA_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/rultra"))
}

fn backend() -> Box<dyn Backend> {
    #[cfg(all(target_os = "linux", feature = "hardware"))]
    {
        if let Ok(b) = rultra_sense::backend::linux::LinuxBackend::open() {
            return Box::new(b);
        }
        eprintln!("rultra-mcp: hardware unavailable, using the mock backend");
    }
    Box::new(rultra_sense::backend::mock::MockBackend::crowpi())
}

fn verification_str(v: Verification) -> &'static str {
    match v {
        Verification::Working => "working",
        Verification::Unvalidated => "unvalidated",
        Verification::AcksButSilent => "acks_but_silent",
        Verification::Untested => "untested",
    }
}

fn scalar(b: &mut dyn Backend, id: DeviceId) -> Option<f64> {
    match b.read(id).ok()?.value {
        Value::Scalar { n, .. } => Some(n),
        _ => None,
    }
}

// ── the documents behind both the tools and the resources ────────────────────

fn devices_doc() -> J {
    let mut b = backend();
    let presence = b.probe().unwrap_or_default();
    json!({
        "devices": device::CATALOG.iter().map(|d| {
            let p = presence.iter().find(|p| p.device == d.id);
            json!({
                "id": format!("{:?}", d.id).to_lowercase(),
                "part": d.part,
                "kind": match d.kind { DeviceKind::Sensor => "sensor", DeviceKind::Actuator => "actuator" },
                "bus": d.bus,
                "verification": verification_str(d.verification),
                "evidence": d.evidence,
                "responding": p.map(|p| p.responding).unwrap_or(false),
                "detail": p.map(|p| p.detail.clone()).unwrap_or_default(),
            })
        }).collect::<Vec<_>>(),
        "note": "verification is a claim about evidence. Only \"working\" means someone \
                 observed correct output; \"unvalidated\" means the output is stable but \
                 has never been checked against a reference."
    })
}

fn telemetry_doc() -> J {
    let mut b = backend();
    json!({
        "at": rultra_sense::now(),
        "readings": device::CATALOG.iter()
            .filter(|d| d.kind == DeviceKind::Sensor)
            .filter_map(|d| {
                let r = b.read(d.id).ok()?;
                Some(json!({
                    "id": format!("{:?}", d.id).to_lowercase(),
                    "value": r.value,
                    "verification": verification_str(r.verification),
                }))
            }).collect::<Vec<_>>()
    })
}

fn spatial_doc() -> J {
    let mut b = backend();
    let room = rultra_spatial::RoomState::fuse(
        scalar(b.as_mut(), DeviceId::Range).map(|cm| cm / 100.0),
        scalar(b.as_mut(), DeviceId::Light),
        device::lookup(DeviceId::Range)
            .map(|d| d.verification)
            .unwrap_or(Verification::Untested),
        device::lookup(DeviceId::Light)
            .map(|d| d.verification)
            .unwrap_or(Verification::Untested),
        None,
    );
    let steering = rultra_spatial::Steering::from(&room);
    json!({
        "room": room,
        "steering": {
            "intensity": steering.intensity,
            "luminance": steering.luminance,
            "calm": steering.calm,
            "billable": steering.billable,
        },
        "note": "bands rather than raw distances: the range finder is unvalidated, so a \
                 band is what it can honestly support. `billable` is false unless every \
                 contributing sensor is verified working."
    })
}

fn policy_doc() -> J {
    let p = rultra_evolve::policy::Applier::new(state_dir().join("policy.json")).load();
    json!({
        "poll_interval_ms": p.poll_interval_ms,
        "content_hash": p.content_hash(),
        "min_poll_ms": rultra_evolve::policy::MIN_POLL_MS,
        "max_poll_ms": rultra_evolve::policy::MAX_POLL_MS,
        "thermal_ceiling_c": rultra_evolve::policy::THERMAL_CEILING_C,
        "thermal_comfort_c": rultra_evolve::policy::THERMAL_COMFORT_C,
    })
}

fn witness_doc(limit: usize) -> J {
    let path = state_dir().join("witness.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({ "entries": [], "verified": false, "reason": "no chain on disk yet" });
    };
    match rultra_witness::Chain::parse_jsonl(&text) {
        Ok(entries) => {
            let total = entries.len();
            let tail: Vec<J> = entries
                .iter()
                .rev()
                .take(limit)
                .map(|e| json!({ "seq": e.seq, "at": e.at, "event": e.event, "hash": e.hash }))
                .collect();
            json!({ "count": total, "showing": tail.len(), "verified": true, "entries": tail })
        }
        // Reported, never silently skipped: hiding a malformed line would
        // conceal exactly the tampering the chain exists to detect.
        Err(e) => json!({ "entries": [], "verified": false, "reason": e.to_string() }),
    }
}

fn capabilities_doc() -> J {
    let working: Vec<&str> = device::CATALOG
        .iter()
        .filter(|d| d.verification == Verification::Working)
        .map(|d| d.part)
        .collect();
    json!({
        // Derived from the catalog rather than declared, so it cannot drift
        // into advertising something the board no longer proves.
        "sense": device::CATALOG.iter().filter(|d| d.kind == DeviceKind::Sensor)
            .map(|d| json!({ "id": format!("{:?}", d.id).to_lowercase(),
                             "verification": verification_str(d.verification) })).collect::<Vec<_>>(),
        "actuate": device::CATALOG.iter().filter(|d| d.kind == DeviceKind::Actuator)
            .map(|d| json!({ "id": format!("{:?}", d.id).to_lowercase(),
                             "verification": verification_str(d.verification) })).collect::<Vec<_>>(),
        "proven_parts": working,
        "governed_loop": {
            "runs_unattended": true,
            "cadence": "every 15 minutes via rultra-cycle.timer",
            "gate": "autogenous hard AND-gate with canary and verified rollback",
        },
    })
}

// ── tool arguments ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// Device id, e.g. `light`, `cpu_temp`, `range`.
    pub device: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WitnessArgs {
    /// How many of the most recent entries to return. Defaults to 20.
    pub limit: Option<u32>,
}

// Fields are read only inside the hardware-gated branch.
#[cfg_attr(not(all(target_os = "linux", feature = "hardware")), allow(dead_code))]
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MatrixArgs {
    /// `heart`, `clear`, or `scroll`.
    pub pattern: String,
    /// Text for `scroll`.
    pub text: Option<String>,
}

// Fields are read only inside the hardware-gated branch.
#[cfg_attr(not(all(target_os = "linux", feature = "hardware")), allow(dead_code))]
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LcdArgs {
    /// First line, up to 16 characters.
    pub line1: String,
    /// Second line, up to 16 characters.
    pub line2: Option<String>,
}

/// The box, as an MCP server.
#[derive(Clone)]
pub struct RultraMcp {
    // Read by the code #[tool_handler] generates, which clippy cannot see.
    #[allow(dead_code)]
    tool_router: ToolRouter<RultraMcp>,
}

#[tool_router]
impl RultraMcp {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    fn ok(v: J) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into()),
        )]))
    }

    #[tool(
        description = "Every device on the board with how well each is actually known to work. Verification is a claim about evidence: only \"working\" means someone observed correct output.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn rultra_devices(&self) -> Result<CallToolResult, McpError> {
        Self::ok(devices_doc())
    }

    #[tool(
        description = "One reading from every sensor that answers right now.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn rultra_telemetry(&self) -> Result<CallToolResult, McpError> {
        Self::ok(telemetry_doc())
    }

    #[tool(
        description = "Read one device by id. Errors rather than fabricating a value when the device does not answer.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn rultra_read(
        &self,
        Parameters(a): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let want = a.device.to_lowercase();
        let Some(d) = device::CATALOG
            .iter()
            .find(|d| format!("{:?}", d.id).to_lowercase() == want)
        else {
            return Err(McpError::invalid_params(
                format!("unknown device: {want}"),
                None,
            ));
        };
        match backend().read(d.id) {
            Ok(r) => Self::ok(json!({ "id": want, "value": r.value, "at": r.at,
                                      "verification": verification_str(r.verification) })),
            Err(e) => Err(McpError::internal_error(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Fused room state (proximity and light as bands) plus the normalized media steering signal.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn rultra_spatial(&self) -> Result<CallToolResult, McpError> {
        Self::ok(spatial_doc())
    }

    #[tool(
        description = "The signed, hash-chained record of every decision the box has made.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn rultra_witness(
        &self,
        Parameters(a): Parameters<WitnessArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::ok(witness_doc(a.limit.unwrap_or(20).min(500) as usize))
    }

    #[tool(
        description = "What this box can sense and actuate, derived from the catalog rather than declared.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn rultra_capabilities(&self) -> Result<CallToolResult, McpError> {
        Self::ok(capabilities_doc())
    }

    #[tool(
        description = "Run one governed cycle: observe, propose, gate, promote or roll back. May change the sensing policy, and always appends to the witness chain.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn rultra_cycle(&self) -> Result<CallToolResult, McpError> {
        let out = std::process::Command::new("rultra")
            .arg("cycle")
            .arg("8")
            .output();
        match out {
            Ok(o) if o.status.success() => Self::ok(
                serde_json::from_slice(&o.stdout)
                    .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&o.stdout)})),
            ),
            Ok(o) => Err(McpError::internal_error(
                String::from_utf8_lossy(&o.stderr).to_string(),
                None,
            )),
            Err(e) => Err(McpError::internal_error(
                format!("rultra binary unavailable: {e}"),
                None,
            )),
        }
    }

    #[tool(
        description = "Draw on the 8x8 LED matrix: heart, clear, or scroll text.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn rultra_matrix(
        &self,
        Parameters(a): Parameters<MatrixArgs>,
    ) -> Result<CallToolResult, McpError> {
        #[cfg(all(target_os = "linux", feature = "hardware"))]
        {
            use rultra_sense::backend::linux::LinuxBackend;
            const HEART: [u8; 8] = [0x00, 0x66, 0xff, 0xff, 0xff, 0x7e, 0x3c, 0x18];
            let r = match a.pattern.as_str() {
                "heart" => LinuxBackend::matrix_draw(&HEART),
                "clear" => LinuxBackend::matrix_draw(&[0; 8]),
                "scroll" => LinuxBackend::matrix_scroll(a.text.as_deref().unwrap_or("RULTRA"), 60),
                other => {
                    return Err(McpError::invalid_params(
                        format!("unknown pattern: {other}"),
                        None,
                    ))
                }
            };
            return match r {
                Ok(()) => Self::ok(json!({ "ok": true, "pattern": a.pattern })),
                Err(e) => Err(McpError::internal_error(e.to_string(), None)),
            };
        }
        #[cfg(not(all(target_os = "linux", feature = "hardware")))]
        {
            let _ = a;
            Err(McpError::internal_error(
                "built without the hardware feature".to_string(),
                None,
            ))
        }
    }

    #[tool(
        description = "Write up to two 16-character lines to the character LCD.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn rultra_lcd(
        &self,
        Parameters(a): Parameters<LcdArgs>,
    ) -> Result<CallToolResult, McpError> {
        #[cfg(all(target_os = "linux", feature = "hardware"))]
        {
            use rultra_sense::backend::linux::LinuxBackend;
            let b =
                LinuxBackend::open().map_err(|e| McpError::internal_error(e.to_string(), None))?;
            return match b.lcd_write(&a.line1, a.line2.as_deref().unwrap_or("")) {
                Ok(()) => Self::ok(json!({ "ok": true })),
                Err(e) => Err(McpError::internal_error(e.to_string(), None)),
            };
        }
        #[cfg(not(all(target_os = "linux", feature = "hardware")))]
        {
            let _ = a;
            Err(McpError::internal_error(
                "built without the hardware feature".to_string(),
                None,
            ))
        }
    }
}

impl Default for RultraMcp {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler]
impl ServerHandler for RultraMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_instructions(
            "rultra MCP — the sensors, actuators, governed loop and witness chain of a \
             CrowPi/Raspberry Pi 5 box. Read ruv://rultra/{devices,telemetry,spatial,policy,\
             witness,capabilities}. Verification is load-bearing: only \"working\" means a \
             device was observed producing correct output, \"unvalidated\" means its output \
             is stable but never checked against a reference, and \"acks_but_silent\" means \
             it answers its bus while producing no observable effect. Do not present an \
             unvalidated reading as a measurement.",
        )
    }

    async fn list_resources(
        &self,
        _p: Option<PaginatedRequestParams>,
        _c: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(
            RES.iter()
                .map(|(uri, name, desc)| {
                    let mut r = Resource::new(*uri, *name);
                    r.description = Some((*desc).to_string());
                    r.mime_type = Some("application/json".to_string());
                    r
                })
                .collect(),
        ))
    }

    async fn read_resource(
        &self,
        p: ReadResourceRequestParams,
        _c: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let doc = match p.uri.as_str() {
            "ruv://rultra/devices" => devices_doc(),
            "ruv://rultra/telemetry" => telemetry_doc(),
            "ruv://rultra/spatial" => spatial_doc(),
            "ruv://rultra/policy" => policy_doc(),
            "ruv://rultra/witness" => witness_doc(50),
            "ruv://rultra/capabilities" => capabilities_doc(),
            other => {
                return Err(McpError::resource_not_found(
                    format!("unknown resource: {other}"),
                    None,
                ))
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into()),
            p.uri,
        )])
        .into())
    }
}

/// Serve on stdio.
pub async fn run() -> anyhow::Result<()> {
    let service = RultraMcp::new().serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
